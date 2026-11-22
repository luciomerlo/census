# macOS — pendiente y estado

Actualizado: 2026-11-22. Crate: `mac/`. Contrato: `docs/PLATAFORMAS.md`,
`docs/ATLAS.md` §9–§10.

## Lo que ya está

- Tipos del contrato: `FileKey` (lookup del árbol) y `FileId` (identidad con
  `btime`), `Entrada` por lotes con nombres en bytes + `encoding`, `Cambio`
  neutro, `Marca` opaca `m1:<vol_uuid>:<event_id>`.
- `Index`/`Builder`: nodos planos, arena UTF-8, `Compacto` de 12 B, hijos CSR,
  orden por nombre, subárboles Euler, tamaños recursivos, hardlinks contados
  una vez.
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
- §9: `cloud_id` con vectores compartidos; parser de
  `CloudStorage/GoogleDrive-<cuenta>/Shared drives/<drive>`; alias manual para
  rclone/FUSE; duplicados avisados, nunca fusionados en silencio.
- Tests portables en Windows (31) y `cargo check`/`clippy -D warnings` para
  Intel y Apple silicon.

## Lo que falta (en orden)

1. **Snapshot `.cnx` + catálogo por volumen**
   - Guardar un `.cnx` por `volume_id` (no uno global): dos discos externos que
     alternan en `/Volumes/Backup` no se pisan.
   - Cabecera con versión, provenance (`pc_id`, host, arch, versión), `Marca`,
     `content_hash` FNV-1a 64, volúmenes y raíces; checksum y carga robusta.
   - Al cargar, si el `volume_id` coincide pero cambió el mount, **remapear el
     prefijo** de la raíz, sin reindexar.
   - Catálogo que carga todos los `.cnx` presentes y consulta en paralelo.
2. **`export --format wcat`**
   - `C.wcat` v3 little-endian exacto, nombres normalizados a UTF-16LE en el
     dueño (U+FFFD sólo si el nombre no era UTF-8), letras sintéticas estables
     derivadas del `volume_id` ordenado, y `manifest.json` al lado con
     `letter -> {root_path, volume_id, cloud_id, Marca, content_hash, platform}`.
   - Verificar con el core Windows: cabecera, nodos, arena y búsqueda con
     acento.
3. **CLI**
   - `index`, `query`, `top`, `stats`, `bench`, `volumes --json`.
   - `bench` separa walk / assemble / derived / save / query.
4. **FSEvents (`vigilante_mac.rs`)**
   - `FSEventStreamCreate` con `sinceWhen = last_event_id` persistido,
     `kFSEventStreamCreateFlagFileEvents`, callback en run loop propio.
   - Overlay de altas/bajas/modificaciones; rename sin ruta vieja → marcar la
     carpeta padre para rescaneo; `MustScanSubDirs` → reindexado.
   - `Marca` se actualiza con el `eventId` real, no con el reloj.
5. **HTTP + MCP**
   - `/health`, `/api/v1/search|count|top|volumes|stats|manifest|snapshot`,
     `/mcp` JSON-RPC; `census mcp` stdio.
   - Herramientas: `search`, `count`, `largest`, `recent`, `volumes`, `stats`,
     `peers`, `remote_search`. Todo lectura.
6. **Tailscale / Atlas**
   - `tailscale status --json`, peers con `has_census`, `pull` de snapshots
     opacos, selección explícita de offline, `latest-wins` por `Marca`,
     procedencia y frescura en cada fila.
   - Nunca leer offline por defecto; `--todas` sólo si el usuario lo pide.
7. **Empaquetado**
   - `.app` con `Info.plist`, firma Developer ID, notarización, universal2.
   - LaunchAgent opcional para `census serve`; no arrancar tareas pesadas solas.
   - Full Disk Access: `census access` explica qué falta, sin pedir sudo.
8. **Pruebas en VM reales**
   - macOS Intel (12/13) y Apple silicon (13/14/15).
   - Árbol sintético de 1 M de archivos; medir nodos/s y RAM.
   - Nombres no UTF-8, symlinks, hardlinks, firmlinks, APFS clones.
   - Dos discos externos alternando el mismo `/Volumes/Backup`.
   - Google Drive montado por Google Drive y por rclone (mismo `cloud_id`).

## Cómo probar lo que hay

```bash
cd mac
cargo test
cargo clippy --all-targets -- -D warnings
cargo check --target x86_64-apple-darwin
cargo check --target aarch64-apple-darwin
cargo run -- volumes
cargo run -- cloud-id gdrive alice@gmail.com my-drive   # 28590f656b6fe252
```

En una Mac real, `cargo run -- volumes` debe mostrar al menos el volumen de
arranque con `identity_state=ok`, los externos con su `disk_hw_id` y los
montajes de nube con `cloud_id` o `posible_duplicado`.

## Riesgos conocidos

- El parser de `getattrlistbulk` está compilado para ambos targets pero **no se
  ejecutó todavía en una Mac**. El orden de atributos está tomado de
  `getattrlistbulk(2)` y de la tabla de XNU; la primera prueba en VM debe
  comparar contra `find`/`du`.
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

## Decisiones abiertas para los otros chats

- `pc_id`: Mac usa 32 hex persistido. Si Windows usa Ed25519, la pubkey va en
  campo aparte y el id sigue siendo 32 hex.
- Vectores de `cloud_id`: Mac ya usa `FNV-1a64(provider \0 account \0 unit)` sin
  prefijo. Linux y Windows deben coincidir con
  `("gdrive","alice@gmail.com","my-drive") -> 28590f656b6fe252`.
- `volume_id` de nube en manifest: va `volume_id = cloud_id` **y** `cloud_id`.
