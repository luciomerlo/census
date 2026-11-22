# Census Atlas — leer índices de otras PCs por Tailscale

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
| PC | `pc_id` = hash de la clave pública de la PC | la genera Census la primera vez |
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
   census agent ──┐         census agent ──┐         census agent ──┐
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
census atlas init                 # crea pc_id (clave Ed25519) y une la tailnet
census atlas peers                # lista PCs, estado y última versión de cada disco
census atlas push                 # sube el snapshot propio al hub
census atlas pull                 # baja los snapshots que falten
census atlas query "mp3"          # busca en todas las PCs
census atlas query "mp3" --as-of 2026-03-03
census atlas where "informe.docx" # dónde está, en qué PC, qué tan fresco
census atlas list                 # discos conocidos por PC, con versión y estado
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
2. **`census atlas peers`**: parsear `tailscale status --json`, sin red propia.
3. **Hub mínimo**: `GET /manifest`, `PUT /snapshot`, `GET /snapshot/:id`.
4. **Consulta federada** con deadline y marcado de frescura.
5. **`--as-of`** sobre el anillo de versiones.
6. **Modo hashes** y filtrado de raíces ocultas.

Los pasos 1 y 2 no dependen de nada de red y son los que fijan el contrato.
