# macOS — pendiente y estado

Actualizado: 2026-11-22. Crate: `mac/`. Contrato: `docs/PLATAFORMAS.md`,
`docs/ATLAS.md` §9–§10.

## Lo que ya está

- Tipos del contrato: `FileKey` (lookup del árbol) y `FileId` (identidad con
  `btime`), `Entrada` por lotes con nombres en bytes + `encoding`, `Cambio`
  neutro, `Marca` opaca `m1:<vol_uuid>:<event_id>`.
- **Ids en el formato del contrato**: `DiskHwId` y `volume_id` son FNV-1a 64 en
  hex minúsculas de 16 caracteres, **sin prefijo** (antes decían `disk:`/`vol:`
  y no coincidían con `tests/identity.rs` §10). Las raíces sueltas siguen
  siendo `dir:<hostname>:<ruta>` (§10.5), que no se hashea a propósito.
- `Index`/`Builder`: nodos planos, arena UTF-8, `Compacto` de 12 B, hijos CSR,
  orden por nombre, subárboles Euler, tamaños recursivos, hardlinks contados
  una vez. Los índices derivados salen de una sola función
  (`index::compute_derived`) que usan el `Builder` y la carga del snapshot.
- Consulta: `path:`, `file:`, `folder:`, `ext:`, `size:`, `dm:`, `rc:`,
  términos, orden y paginación. Plegado idéntico a Windows (ASCII + Latin-1
  C0..DE sin ×, sin quitar acentos).
- `getattrlistbulk` en `fuente_mac.rs`: un syscall por carpeta, sin `lstat`
  por archivo; workers + colector; no sigue symlinks; no cruza puntos de
  montaje; lee `ATTR_CMN_FILEID`, `CRTIME`, `MODTIME`, tamaño y `alloc`.
- Volúmenes macOS: `getmntinfo`, `MNT_LOCAL`, `ATTR_VOL_UUID`, nombre de
  volumen, red/nube, raíces de `~/Library/CloudStorage` y iCloud.
- §10: `DiskHwId` desde `diskutil info -plist` (`DiskUUID`, `MediaName`,
  `TotalSize`, serial si existe), `volume_id = hash(DiskHwId || offset ||
  fs_uuid)`, mount sólo presentación, `sin_identidad` fuera de federación.
  `Volume.model` guarda el `MediaName` para el JSON.
- §9: `cloud_id` con vectores compartidos; parser de
  `CloudStorage/GoogleDrive-<cuenta>/Shared drives/<drive>`; alias manual para
  rclone/FUSE; duplicados avisados, nunca fusionados en silencio.
- **Snapshot `.cnx` v1 por volumen** (`src/snapshot.rs`): un archivo por
  `volume_id` en `~ …/CensusIndex/snapshots` (`config::snapshots_dir()`), no uno
  global. Cabecera de 96 B con versión, flags, cantidades, `total_len`,
  `content_hash`, `body_checksum`, `header_block_checksum`, `built_at` y
  `generation`; bloque variable con provenance (`pc_id`, host, platform, arch,
  env, versión del binario), `Marca`, volúmenes (con `disk_hw_id`, serial,
  `cloud_id`, `identity_state`, `uuid`) y raíces; cuerpo con nodos de 64 B y
  las dos arenas. Los derivados se recalculan al cargar (no se reindexa ni se
  toca el disco).
- **Carga robusta**: tamaño real contra `total_len`, magic, versión, banderas,
  coherencia de `body_len`/`header_block_len`, dos checksums, `content_hash`,
  y por nodo `name_off + name_len` dentro de las dos arenas, `parent` y `vol`
  dentro de rango, y raíces con `parent == sí mismo`. Tests: **todas** las
  truncaciones de un archivo válido (una por cada longitud menor que el
  archivo), 128 buffers de basura determinista
  (basura pura, basura con magic, byte cambiado, cola agregada) y una prueba
  aparte con checksums recalculados para verificar que las validaciones
  estructurales no dependen de ellos.
- **Remapeo de montaje** (`Snapshot::remap_mount`): mismo `volume_id` con otro
  punto de montaje reescribe el nombre de los nodos raíz (en el hueco si entra,
  al final si no), refresca `mount`/`roots` y recalcula `content_hash`. No se
  reindexa: mismos nodos, misma `Marca`, mismos archivos.
- **Catálogo** (`src/catalogo.rs`): carga todos los `.cnx` presentes en orden
  de archivo, descarta duplicados de `volume_id` por `Marca`/`built_at` con
  aviso, ignora un archivo corrupto con aviso **sin dejar de responder por los
  demás**, reconcilia online/offline contra los montajes actuales (las raíces
  `dir:` se resuelven mirando si la ruta existe) y consulta en paralelo con
  resultado determinista: orden por la clave pedida y desempate total por
  `(volume_id, path)`. Los offline no se leen salvo `include_offline`.
- **CLI**: `volumes --json` con los nombres del contrato (`volume_id`,
  `disk_hw_id`, `identity_status`, `federable`, `mount`, `model`, `serial`,
  `capacity`, `cloud_id`); `snapshot save|list|query` con `--json` estable
  (`VolumeInfo`, `Hit`, `SavedRow`). `snapshot save` escribe un `.cnx` por
  volumen y una `Marca` por volumen (`m1:<vol_uuid>:0` si no hay `eventId`).
- Tests portables en Windows (53) y `cargo check`/`clippy -D warnings` para
  Intel y Apple silicon.

## Lo que falta (en orden)

1. ~~**Snapshot `.cnx` + catálogo por volumen**~~ — hecho. Lo que queda de este
   punto, con motivo:
   - La `Marca` que se guarda es `m1:<vol_uuid>:0` (reindexado completo) porque
     **FSEvents todavía no existe**: el `eventId` real llega con el punto 4.
   - `generation` está fija en 1 y no se persiste: no hay contador durable hasta
     que exista FSEvents/reindexado incremental.
   - `snapshot save` usa `WalkFuente` (recorrido portable) incluso en macOS.
     Conectar `getattrlistbulk` y el comando `index` es el punto 3; hasta
     entonces **no hay medición** de nodos/s ni de tamaño/tiempo del `.cnx`.
   - La carga de `.cnx` es secuencial (la consulta sí es paralela). Medir en
     una Mac real antes de paralelizar: hoy no hay números.
   - `identity_status` del JSON de macOS no puede valer `serial_ambiguo`: la
     detección de seriales repetidos de puentes USB (§10.4) no está
     implementada; quedan `ok`, `sin_identidad`, `cloud` y
     `posible_duplicado`.
   - `config::index_path()` (el `index.cnx` global) quedó sin uso; borrarlo
     cuando ningún otro módulo lo lea.
   - `mac/README.md` todavía dice "snapshot `.cnx` (pendiente)": no lo toqué
     porque este trabajo sólo alcanza `mac/src/` y este archivo.
2. **`export --format wcat`**
   - `C.wcat` v3 little-endian exacto, nombres normalizados a UTF-16LE en el
     dueño (U+FFFD sólo si el nombre no era UTF-8), letras sintéticas estables
     derivadas del `volume_id` ordenado, y `manifest.json` al lado con
     `letter -> {root_path, volume_id, cloud_id, Marca, content_hash, platform}`.
   - Verificar con el core Windows: cabecera, nodos, arena y búsqueda con
     acento.
3. **CLI completa**
   - `index`, `query`, `top`, `stats`, `bench`. Hoy hay `snapshot save|list|
     query`, que cubre el camino del `.cnx` pero no reemplaza a `index`.
   - `bench` separa walk / assemble / derived / save / query.
4. **FSEvents (`vigilante_mac.rs`)**
   - `FSEventStreamCreate` con `sinceWhen = last_event_id` persistido,
     `kFSEventStreamCreateFlagFileEvents`, callback en run loop propio.
   - Overlay de altas/bajas/modificaciones; rename sin ruta vieja → marcar la
     carpeta padre para rescaneo; `MustScanSubDirs` → reindexado.
   - `Marca` se actualiza con el `eventId` real, no con el reloj.
5. **HTTP + MCP**
   - `/health`, `/api/v1/search|count|top|volumes|stats|manifest|snapshot`,
     `/mcp` JSON-RPC; `censusindex mcp` stdio.
   - Herramientas: `search`, `count`, `largest`, `recent`, `volumes`, `stats`,
     `peers`, `remote_search`. Todo lectura.
6. **Tailscale / Atlas**
   - `tailscale status --json`, peers con `has_censusindex`, `pull` de snapshots
     opacos, selección explícita de offline, `latest-wins` por `Marca`,
     procedencia y frescura en cada fila.
   - Nunca leer offline por defecto; `--todas` sólo si el usuario lo pide.
7. **Empaquetado**
   - `.app` con `Info.plist`, firma Developer ID, notarización, universal2.
   - LaunchAgent opcional para `censusindex serve`; no arrancar tareas pesadas solas.
   - Full Disk Access: `censusindex access` explica qué falta, sin pedir sudo.
8. **Pruebas en VM reales**
   - macOS Intel (12/13) y Apple silicon (13/14/15).
   - Árbol sintético de 1 M de archivos; medir nodos/s y RAM.
   - Nombres no UTF-8, symlinks, hardlinks, firmlinks, APFS clones.
   - Dos discos externos alternando el mismo `/Volumes/Backup` (el caso está
     cubierto por tests portables; falta el disco real).
   - Google Drive montado por Google Drive y por rclone (mismo `cloud_id`).

## Cómo probar lo que hay

```bash
cd mac
cargo test                                              # 53 portables
cargo clippy --all-targets -- -D warnings
cargo check --target x86_64-apple-darwin
cargo check --target aarch64-apple-darwin
cargo run -- volumes                                    # o volumes --json
cargo run -- cloud-id gdrive alice@gmail.com my-drive   # 28590f656b6fe252
cargo run -- snapshot save --root ~/Documents --json    # un .cnx por volumen
cargo run -- snapshot list                              # online/offline, avisos
cargo run -- snapshot query "informe" --todas --json
```

`snapshot save` sin `--root` indexa los volúmenes locales detectados; en una
máquina que no es macOS el detector sólo devuelve las raíces de `config.roots`.

En una Mac real, `cargo run -- volumes` debe mostrar al menos el volumen de
arranque con `identity_state=ok`, los externos con su `disk_hw_id` y los
montajes de nube con `cloud_id` o `posible_duplicado`.

## Formato `.cnx` v1 (para el hub y Linux)

Un archivo por `volume_id`; nada de esto está implementado en el hub todavía.
Va documentado acá porque `docs/` es de otros chats y el detalle tiene que
migrar ahí cuando su dueño lo acepte.

Little-endian, campo por campo, sin `repr(C)`. Cabecera fija de 96 B:
`magic` `CENSUSCNX` (9 B), `version` u16 (1), `flags` u16 (bit 0 = LE),
relleno 3 B, `node_count` u32, `name_bytes` u32, `folded_bytes` u32,
`volume_count` u32, `root_count` u32, `header_block_len` u32, `body_len` u64,
`content_hash` u64, `body_checksum` u64, `header_block_checksum` u64,
`built_at` i64, `generation` u64, `total_len` u64.

Bloque variable (`header_block_len`): provenance (6 textos), `Marca`, por
volumen (`volume_id`, `name`, `mount`, `fs`, `method`, `uuid[16]`,
`disk_hw_id`, `disk_guid`, `serial`, `cloud_id`, `identity_state` u8,
`partition_offset`, `total`, `free`, cinco banderas u8, `nodes`), y por raíz
(`node_id` u32, `volume_index` u16, ruta). Cuerpo (`body_len`): nodos de 64 B,
arena `names`, arena `folded`. Todo texto es u32 largo + UTF-8.

## Riesgos conocidos

- El parser de `getattrlistbulk` está compilado para ambos targets pero **no se
  ejecutó todavía en una Mac**. El orden de atributos está tomado de
  `getattrlistbulk(2)` y de la tabla de XNU; la primera prueba en VM debe
  comparar contra `find`/`du`.
- El `.cnx` v1 **no guarda los índices derivados**: se recalculan al cargar.
  Es a propósito (un archivo con checksum válido no puede romper invariantes),
  pero el costo de carga no está medido en una Mac real.
- Los firmlinks de APFS (`/Users` → volumen Data) se recorren desde `/`; hay
  que verificar que `/System/Volumes/Data` no se cuente dos veces (hoy se saltea
  por ser punto de montaje).
- `diskutil info -plist` puede no traer `PartitionOffset` en todas las versiones;
  el fallback usa `VolumeUUID`. Si tampoco hay UUID, el volumen queda
  `sin_identidad`.
- Los nombres no UTF-8 se conservan en `.cnx`; al exportar a `C.wcat` se
  reemplazan en el dueño y se informa cuántos.
- FSEvents no da la ruta vieja en un rename: la estrategia es rescaneo del
  padre, no un `stat` con carrera.
- El remapeo de montaje escribe el nombre nuevo en el hueco del viejo o al
  final de la arena: muchas alternancias de nombre más largo harían crecer la
  arena hasta el siguiente guardado (que reescribe todo).

## Decisiones abiertas para los otros chats

- `pc_id`: Mac usa 32 hex persistido. Si Windows usa Ed25519, la pubkey va en
  campo aparte y el id sigue siendo 32 hex.
- Vectores de `cloud_id`: Mac ya usa `FNV-1a64(provider \0 account \0 unit)` sin
  prefijo. Linux y Windows deben coincidir con
  `("gdrive","alice@gmail.com","my-drive") -> 28590f656b6fe252`.
- `volume_id` de nube en manifest: va `volume_id = cloud_id` **y** `cloud_id`.
- **Choque de documentos a resolver por sus dueños**: `docs/PLATAFORMAS.md` §5
  dice `volume_id = hash(fs_uuid || fstype || total_size)` para macOS, y
  `docs/ATLAS.md` §10.1 dice `hash(DiskHwId || offset || fs_uuid)`. Mac
  implementa §10 (es la regla dura y la más específica). Si Linux copió §5, los
  `volume_id` no van a coincidir entre plataformas para el mismo disco.
- `tests/identity.rs` de la raíz espera `identity_status` con cuatro valores
  (`ok`, `serial_ambiguo`, `posible_duplicado`, `sin_identidad`) y un `mount` de
  dos caracteres (`C:`): es el contrato de **Windows**. El JSON de macOS usa los
  nueve campos pedidos para esta plataforma y `identity_status` vale `ok`,
  `sin_identidad`, `cloud` o `posible_duplicado`. Falta decidir si Atlas unifica
  el vocabulario o mantiene la diferencia por plataforma.
