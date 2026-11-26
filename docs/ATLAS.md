# CensusIndex Atlas — leer índices de otras PCs por Tailscale

Estado: diseño. Base de código: `Arena<T>` (commit `d5f9c9a`) ya permite que las
arenas del snapshot se lean sin copiarlas, que es lo que necesita el hub para
tener 20 M de filas sin cargarlas en RAM.

## El problema

Cada PC indexa sus discos y produce un `.wcat`. Queremos:

1. Buscar en **todas** las PCs sin importar cuál está prendida.
2. Si una PC está apagada y su disco no está online, usar la **versión más
   nueva conocida** de ese disco, diciendo de dónde salió.
3. Marcar cada resultado con **origen**: qué PC, qué disco, qué versión y qué
   tan fresca es.
4. Viajar por **Tailscale**, que resuelve NAT, DNS entre nodos y cifrado
   (WireGuard) sin que inventemos nada.

## Identidad: tres niveles

| Nivel | Identificador | De dónde sale |
|---|---|---|
| Red | `machine_id` (estable) + `dns_name` | `tailscale status --json` |
| PC | `pc_id` = hash de la clave pública de la PC | la genera CensusIndex la primera vez |
| Disco | `volume_id` = `vol_serial` + tamaño | `GetVolumeInformationW`, ya lo tenemos |

Una entrada de índice se identifica por la terna **`pc_id` / `volume_id` /
`version`**. Eso permite que dos PCs tengan el mismo disco físico (un SSD
externo que se enchufa en dos máquinas) y se entienda como **el mismo disco**,
no como dos.

## Versiones: qué significa "más nueva"

Cada snapshot trae, en su cabecera y en la cola:

- `built_at` (segundos Unix), `vol_serial`, `nodes`, `stats.build_ms`
- `journal_id` y `next_usn` aplicados (C2)
- **`content_hash`**: FNV-64 del archivo, calculado al guardar (barato, ya
  recorremos los bytes)

Comparación de dos versiones del mismo `volume_id`:

1. Gana el `journal_id` más alto; dentro de ese, el `next_usn` más alto (es el
   orden real de los cambios, no el reloj).
2. Si no hay journal (exFAT, disco de red), desempata `built_at`.
3. Si empatan, desempata `content_hash` (determinista, evita discusiones).

Esa regla es importante: **el reloj de las PCs miente** (zonas horarias, BIOS,
hibernación). El USN no.

## Cómo viaja

```
   PC-A (Windows)            PC-B (Linux)             Notebook (macOS)
   censusindex agent ──┐         censusindex agent ──┐         censusindex agent ──┐
                  │                        │                        │
             Tailscale (WireGuard, IP 100.x.y.z, MagicDNS)
                  │                        │                        │
                  └────────────► Hub (una PC o un server) ◄─────────┘
                                    │
                       %LOCALAPPDATA%\Census\atlas\
                         manifest.json
                         <pc_id>/<volume_id>/<version>.wcat
```

- **Descubrimiento**: `tailscale status --json` (o `tailscale status` en texto
  si el JSON falla) da los peers, su `dns_name`, si están online y su SO. No hay
  que configurar IPs a mano.
- **Transporte**: HTTP/1.1 sobre la IP de Tailscale (o MagicDNS), con `ureq`
  que ya está en el proyecto. El hub expone:
  - `GET /manifest` → lista de `{pc_id, pc_name, volume_id, version, built_at, next_usn, bytes, content_hash}`
  - `GET /snapshot/<pc_id>/<volume_id>/<version>` → el `.wcat` (o un rango con `Range`)
  - `GET /query?...` → consulta federada del lado del hub (opcional)
- **Push**: cada agente, después de indexar, hace `PUT /snapshot` con su
  archivo. Si el hub no está, guarda en una cola local y reintenta.
- **Tailscale no hace falta que esté en el hub**: si el hub es una PC, ya está
  en la tailnet. Si es un server, se une con `tailscale up` y listo.

## Offline: el punto central

El hub **nunca borra** la última versión de un disco. Estados posibles por
`(pc_id, volume_id)`:

| Estado | Cómo se detecta | Qué se muestra |
|---|---|---|
| `online` | el agente responde `/ping` con su `pc_id` | datos frescos |
| `stale` | el agente no responde pero la versión es < 7 días | "última copia de hace 2 días" |
| `offline` | no responde y la versión es vieja | "offline · copia del 18/11 22:41" |
| `archivada` | versión fuera del anillo (más de N) | sólo con `--as-of` |

Regla dura: **un resultado nunca se presenta como actual si viene de un
snapshot viejo**. La antigüedad se muestra siempre, y en el IPC va en un campo
nuevo (`freshness_ms`) para que Wiz y Sort decidan.

## Fusión de resultados

Cada fila remota viaja con `origen`:

```
{ pc: "PC-A", dns: "pc-a.tailnet.ts.net", disk: "E:", version: 42,
  built_at: 1763... , freshness: "online" | "stale" | "offline",
  path: "E:\\Backup\\fotos\\2019\\...", size, mtime, frn }
```

Deduplicación entre PCs:

- **Mismo disco** (`vol_serial` + tamaño + FRN iguales) → es literalmente el
  mismo archivo, no un duplicado: se muestra una sola vez con dos ubicaciones.
- **Discos distintos con el mismo contenido** → heurística `(size, mtime, hash
  de los primeros 64 KB)`; con Wiz, hash completo. Se agrupa como "copias".

Orden global: coincidencia exacta de nombre > prefijo > subcadena; después
online antes que offline; después mtime descendente. La antigüedad **no**
ordena: ordena la disponibilidad, porque buscar un archivo que no podés abrir
es peor que verlo segundo.

## Presupuesto de tiempo

Una consulta federada con 5 peers:

1. En paralelo a cada peer online: `GET /query` con deadline **150 ms**.
2. Los que no contestan siguen en segundo plano y su resultado se agrega a la
   próxima consulta (nunca se bloquea la ventana).
3. Los offline se resuelven del hub (local, ~20 ms por millón de filas).

## Comandos

```
censusindex atlas init                 # crea pc_id (clave Ed25519) y une la tailnet
censusindex atlas peers                # lista PCs, estado y última versión de cada disco
censusindex atlas push                 # sube el snapshot propio al hub
censusindex atlas pull                 # baja los snapshots que falten
censusindex atlas query "mp3"          # busca en todas las PCs
censusindex atlas query "mp3" --as-of 2026-03-03
censusindex atlas where "informe.docx" # dónde está, en qué PC, qué tan fresco
censusindex atlas list                 # discos conocidos por PC, con versión y estado
```

## Seguridad

- Cada PC firma su manifest con Ed25519; el hub verifica contra el `pc_id`.
- Tailscale ya cifra el transporte; la firma protege contra un hub
  comprometido que quiera inyectar filas falsas.
- Raíces marcadas `oculta` no entran al snapshot que se publica (se filtran al
  guardar, no al enviar).
- El hub no necesita ver nombres de archivo si el usuario elige "sólo hashes":
  modo `privacidad = hashes`, donde sólo viajan `(hash, size, mtime)` y la ruta
  se pide al peer cuando está online.

## Plan de implementación

1. **Manifest + versiones** (hoy): `atlas::Manifest`, `atlas::Version`, selección
   de la versión vigente por disco con la regla de arriba, y tests puros.
2. **`censusindex atlas peers`**: parsear `tailscale status --json`, sin red propia.
3. **Hub mínimo**: `GET /manifest`, `PUT /snapshot`, `GET /snapshot/:id`.
4. **Consulta federada** con deadline y marcado de frescura.
5. **`--as-of`** sobre el anillo de versiones.
6. **Modo hashes** y filtrado de raíces ocultas.

Los pasos 1 y 2 no dependen de nada de red y son los que fijan el contrato.

## 9. Nube compartida: identidad, duplicados y selección por app

El caso que rompe todo si no se resuelve: **el mismo Google Drive compartido**
puede aparecer montado por programas distintos (Google Drive para escritorio,
rclone, `google-drive-ocamlfuse`, el de Wiz, el de Sort) **y en varias PCs a la
vez**. Sin identidad de nube, un archivo del drive compartido aparece 5 veces.

### 9.1 Identidad de un volumen de nube

```
cloud_id = hash(provider || account || unidad)
```

| Campo | Windows | Linux | macOS |
|---|---|---|---|
| `provider` | `gdrive`, `onedrive`, `dropbox`… (por etiqueta de volumen y proveedor detectado) | igual, por tipo de montaje (`fuse.rclone`, `gvfs`, …) | igual, por `diskutil`/`statfs` |
| `account` | el correo de la etiqueta (`lucio@… - Google Drive`) | cuenta del montaje o `rclone config` | idem |
| `unidad` | "Mi unidad" o el nombre del drive compartido | idem | idem |

Regla: **la identidad de nube no usa la letra de unidad**, igual que la de
disco no usa la letra. La letra es de la sesión, el `cloud_id` es del contenido.

### 9.2 Duplicados

1. Si dos entradas comparten `cloud_id` → **son el mismo volumen**: Atlas las
   colapsa y muestra una sola vez, con las ubicaciones reales adentro.
2. Si no se puede atribuir `cloud_id` (montaje raro, sin etiqueta), se marca
   **`posible_duplicado`** y se muestra la advertencia; **nunca** se fusiona en
   silencio.
3. El usuario puede resolverlo a mano:
   `censusindex atlas unir <volumen-a> <volumen-b>` (los trata como el mismo) y
   `censusindex atlas separar <volumen>` (deshace).

Lo mismo aplica entre PCs: el hub compara `cloud_id` de los snapshots de todas
las máquinas. Un drive compartido indexado en tres PCs es **un** volumen.

### 9.3 Activar o desactivar unidades, por app

- Cada app tiene su propia lista, como hoy: `settings.ini` de CensusIndex,
  la de Wiz y la de Sort. Ninguna escribe la de otra.
- La clave es **`volume_id`** (local) o **`cloud_id`** (nube), nunca la letra.
- Una app puede indexar un drive que otra ignora; no se sincronizan listas.
  Lo único compartido es la **identidad**, para que Atlas sepa agrupar.

### 9.4 Elegir qué leer de las otras PCs

Cuando Windows consulta a otra PC, la consulta lleva una **selección de
volúmenes remotos** `(pc_id, volume_id)`:

- por defecto: **todo lo online**;
- los **offline** se incluyen sólo si el usuario los marcó
  (`censusindex atlas incluir <pc> <volumen>`) o si usa `--todas`;
- cada resultado dice de dónde salió y si es online/offline/stale.

La selección se guarda en `%LOCALAPPDATA%\Census\atlas\seleccion.json` y viaja
en la consulta federada, así el peer no tiene que adivinar.

### 9.5 Cuando la detección falla

El usuario puede desactivar a mano, y eso **manda sobre la detección**:

```
censusindex atlas excluir <pc> <volumen>     # no lo leo más de esa PC
censusindex atlas incluir <pc> <volumen>
censusindex atlas unir <volumen-a> <volumen-b>
censusindex atlas separar <volumen>
censusindex atlas estado                    # qué está excluido y por qué
```

Archivo: `%LOCALAPPDATA%\Census\atlas\overrides.json`, con `motivo` y fecha para
que después se entienda por qué se excluyó algo.

**Regla dura:** si la detección automática y la decisión manual se contradicen,
gana la manual. Un duplicado visible es peor que un volumen faltante avisado.

## 10. Identidad por hardware, no por letra (regla dura)

La letra de unidad **no es identidad**. Cambia al reasignar en el Administrador
de discos, y dos discos externos distintos se conectan en momentos distintos y
reciben la misma letra (`E:` hoy es un Seagate, mañana es un WD). Indexar por
letra mezcla dos discos en el mismo índice y es irrecuperable.

### 10.1 Tres niveles, de más físico a más lógico

```
DiskHwId   = hash(serial || vendor || model || capacidad)     # el aparato
volume_id  = hash(DiskHwId || offset_particion || fs_uuid)     # la partición
mount      = letra o punto de montaje                          # SÓLO presentación
```

- **`DiskHwId`** identifica el aparato físico. Windows: número de serie y modelo
  de `IOCTL_STORAGE_QUERY_PROPERTY` (`StorageDeviceProperty`, offset de serie)
  y, si está, `StorageDeviceUniqueIdProperty`. Fallback: GUID de disco GPT.
- **`volume_id`** identifica la partición: `DiskHwId` + offset/tamaño de partición
  + UUID del filesystem. Dos particiones del mismo disco son volúmenes distintos.
- **`mount`** (letra o punto de montaje) se guarda como atributo **de
  presentación**, se refresca en cada escaneo y **nunca** entra en una clave.

Windows además puede usar `GetVolumeNameForVolumeMountPointW`
(`\?\Volume{GUID}\`), que es estable por volumen y sobrevive al cambio de letra.

### 10.2 Qué pasa cuando cambia la letra

- Se detecta el mismo `volume_id` en otro `mount` → **se remapea el prefijo de
  ruta** del índice y del snapshot. No se reindexa: es el mismo volumen, con
  otra etiqueta de presentación.
- Si el volumen no está, el índice queda `offline` con su `volume_id` intacto y
  vuelve a engancharse cuando reaparece, con la letra que tenga.

### 10.3 Letra repetida entre discos distintos

Dos discos externos que se conectan alternadamente en `E:` tienen `DiskHwId`
distinto → **no se mezclan**: son dos volúmenes, y el que no está conectado
queda `offline`. Esto es exactamente el bug que evita la identidad por hardware.

### 10.4 Trampa real: puentes USB baratos

Muchas cajas USB-SATA informan **el número de serie del puente**, no el del
disco: dos discos distintos devuelven la misma serie. Mitigación:

1. Si la misma `serial` aparece con distinta capacidad o distinto modelo →
   `serial_ambiguo` y se agrega `capacidad + modelo + GUID de disco` al hash.
2. Si aun así colisiona → se marca `posible_duplicado` y se pide confirmación
   manual (`atlas unir`/`atlas separar`), nunca fusión silenciosa.

### 10.5 Otras fuentes

| Tipo | Identidad |
|---|---|
| Disco local | `DiskHwId` (serial + modelo + capacidad) |
| Partición / volumen | `volume_id` (DiskHwId + offset + fs_uuid) |
| Nube | `cloud_id` (provider + account + unidad) — §9 |
| Recurso de red | `share_id` (host + share + account) |
| Directorio suelto (`--root`) | `dir:<hostname>:<path>` |

**Regla:** si la identidad no se puede determinar, el volumen **no se indexa en
federación**; aparece como `sin_identidad` y se resuelve a mano. Preferimos un
volumen faltante avisado antes que dos discos fusionados por error.
