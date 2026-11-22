# Linux: estado y pendientes

Actualizado: 2026-11-22. Crate standalone en `linux/` (tiene `[workspace]` propio,
no entra al workspace de Windows). No toca `src/` ni `C.wcat`.

## Qué está hecho y compila

| Archivo | Estado |
|---|---|
| `src/plataforma.rs` | Contrato común: `Kind`, `Encoding`, `FileKey`/`FileId`, `Entrada`, `Cambio`, `Marca l1:<gen>:<cambio>`, `fnv1a64`, `volume_id` viejo y `synthetic_letter`. |
| `src/index.rs` | Nodo de 64 B, arena de nombres + `folded` con offsets, CSR de hijos, preorden `tout`, rangos por nombre, `content_hash`, `find_dir`/`child`/`file_id`, `Root.cloud_id`. |
| `src/config.rs` | XDG (`CENSUS_DATA_DIR`/`CENSUS_CONFIG_DIR`), puerto 8478, token, `pc_id` de 32 hex, filtros `enabled/disabled_volumes`, offline opt-in, `cloud_overrides`. |
| `src/volumes.rs` | `mountinfo`, `statvfs`, `by-uuid`/`by-label`, removible, redes/nube, `cloud_id`, `auto_roots`, `root_meta_for`; por defecto no autoindexa nube/red. |
| `src/cloud.rs` | Detecta rclone, gdfuse, s3fs, gvfs, onedrive, dropbox y juicefs; atribuye `provider/cuenta/unidad`; desconocida queda `desconocida:<mount>` + `posible_duplicado`; overrides mandan. |
| `src/diskid.rs` | **Nuevo, aislado**: `DiskHwId` por udev/sysfs, `volume_id = hash(DiskHwId\|\|offset\|\|fs_uuid)`, `serial_ambiguo`, `sin_identidad`. Tests de hashes y ambigüedad. **Todavía no está integrado** en `Mount`/`Root`/snapshot. |
| `src/main.rs` | WIP útil: `version`, `cloud-id <provider> <account> <unidad>`, `mounts`. Los demás comandos avisan que faltan. |
| `src/{api,catalog,http,mcp,mirrors,query,scan,snapshot,tailscale,watch}.rs` | Stubs con el contrato y el pendiente; `query` exporta `Sort`/`Query` para que `lib.rs` compile. |

Verificación actual **en Windows** (las rutas `cfg(unix)` no se ejecutan):

```bash
cd linux
cargo check          # OK (solo warnings de código unix no usado en Windows)
cargo test           # 35 tests verdes
```

No hay todavía ninguna medición de `getdents64`/`statx`: no se inventan números.
La referencia de Windows es 335k nodos/s y 197-216 ms de carga de snapshot; el
objetivo Linux es 250k-600k entradas/s en NVMe caliente, a confirmar en hierro.

## P0 — terminar el camino mínimo (indexar, guardar, consultar, servir)

1. **Integrar `diskid.rs`** (regla §10, lo más importante):
   - agregar a `Mount` y `Root`: `disk_hw_id`, `serial`, `model`, `capacity`,
     `partition_offset`, `identity_status`, `federable`;
   - `volume_id` de una partición debe ser `diskid::volume_id(disk_hw_id,
     partition_offset, fs_uuid)`; **nunca** `sdX`, letra ni `/media/...`;
   - agrupar todos los montajes, llamar `mark_ambiguous` y `disambiguate`;
   - `sin_identidad` no entra a federación (sí puede consultarse localmente);
   - **cambio de punto de montaje**: mismo `volume_id`, se actualiza
     `Root.path`/prefijo y se reutiliza el subárbol; no se reindexa;
   - dos discos con la misma etiqueta `BACKUP` y distinta serial son dos
     volúmenes; el que no está montado queda `offline`, nunca se fusiona;
   - al cambiar el hash de `volume_id` hay que **subir la versión del snapshot**
     (hoy no hay `.cns` guardado, así que conviene hacerlo antes del primero).
2. **`scan.rs`**: pool de workers con `getdents64` (buffer 64 KiB) + `statx`
   (`STATX_BTIME | AT_STATX_DONT_SYNC`), cola con `Condvar`, ensamblado en
   preorden, poda por `mtime`/`dev`/`ino` contra el índice anterior,
   `copy_subtree` para raíces que no se recorren, `WalkSource: Fuente` por
   lotes (`emit(&[Entrada])`, nunca un `Vec` completo), nombres no UTF-8.
   Tests: árbol temporal, hardlinks, nombre `0xFF`, poda, raíz vacía.
3. **`snapshot.rs`**: `.cns` little-endian con cabecera, host/`pc_id`, raíces,
   nodos de 64 B, arenas, `folded`, compacto, rankings, checksum FNV-1a;
   `read_meta` para el catálogo sin cargar nodos; carga robusta (truncado,
   versión, checksum) y remapeo de prefijo. `export --wcat` con letras
   sintéticas estables por `volume_id` ordenado + `manifest.json` (no tocar
   `C.wcat`).
4. **`query.rs`**: misma sintaxis que Windows (términos, `path:`, `file:`,
   `folder:`, `ext:`, `size:`, `dm:`/`dc:`/`da:`, `rc:`, `sort:`, paginado),
   matching sobre `compacto` en paralelo, orden por nombre/tamaño/fecha/ruta,
   filtro por origen (`local`, `peer:<pc>`, `offline` solo si está habilitado).
5. **`catalog.rs` + `mirrors.rs`**: cargar índice local + `mirrors/*.cns`;
   agrupar por `(pc_id, volume_id)` o `cloud_id`; si coinciden `pc_id` y
   `volume_id` gana la `Marca` (`l1:<gen>:<cambio>`); si no, desempatar por
   `max_mtime_ns` de contenido → `built_at` → `content_hash` (los relojes de
   PCs distintos no son confiables); mostrar origen/frescura por resultado;
   offline oculto salvo opt-in; overrides `unir`/`separar` mandan.
6. **`tailscale.rs` + `http.rs`**: `tailscale status --json` para peers, DNS
   names y detección de nodos con Census; servidor `127.0.0.1:8478` con
   `/health`, `/api/v1/search|top|volumes|stats|snapshot`, `/mcp`; token
   Bearer opcional e identidad `Tailscale-User-Login`; cliente HTTP para
   `remote_search`/`pull`. Publicar con `tailscale serve --bg 8478` (HTTPS,
   MagicDNS), no abrir puertos a la LAN.
7. **`mcp.rs`**: stdio JSON-RPC (solo stdout JSON-RPC, diagnóstico a stderr) y
   transporte HTTP; herramientas `search, count, largest, recent, volumes,
   stats, peers, remote_search, pull`; solo lectura.
8. **`watch.rs`**: `notify`/inotify por directorio (o fanotify por montaje si
   hay root), marcar sucios, reindexar subárboles con poda, recargar `serve`
   sin reiniciar; compactar overlay.
9. **`main.rs` completo**: `index`, `query`, `top`, `volumes`, `stats`, `bench`
   (walk/assemble/derived/save/query), `serve`, `peers`, `pull`, `mirrors`,
   `mcp`, `watch`, `export --wcat`, `config`. `index` reutiliza snapshot si no
   cambió `volume_id`; `--full` reindexa.
10. **Tests de integración y bench real**: snapshot corrupto/truncado,
    hardlinks, nombres no UTF-8, remapeo de mount, dos discos `BACKUP`,
    `cloud_id` compartido entre gvfs/rclone, catálogo con offline. Medir en
    Linux real y pasar el número + primer `.cns` al chat Windows para la
    verificación cruzada.

## P1 — vías nativas (opcionales, con root)

- btrfs: `BTRFS_IOC_TREE_SEARCH_V2` para enumerar el árbol sin recorrer.
- XFS: `XFS_IOC_FSBULKSTAT`.
- ext4: `libext2fs`/`debugfs` (evitar montar en solo lectura si ya está montado).
- Sin root o sin soporte: el walker portable sigue siendo la vía universal.

## Bloqueos actuales

- **No hay Linux en esta máquina**: `wsl --install` pide reinicio y el `--import`
  de Ubuntu base falla con `HCS_E_SERVICE_NOT_AVAILABLE` (sin virtualización
  anidada). No hay Docker. Por eso no se compilaron/ejecutaron las ramas
  `cfg(unix)` ni se midió nada.
- **Tailscale no instalado** en Windows; el usuario va a probar en PCs Linux.
  Instalación prevista: `curl -fsSL https://tailscale.com/install.sh | sh`,
  `sudo tailscale up`, `sudo tailscale set --operator=$USER`,
  `tailscale serve --bg 8478`.
- Falta cerrar con Windows el formato exacto de `pc_id` (¿32 hex? ¿clave
  Ed25519?) y si el manifest de nube lleva `volume_id = cloud_id` además del
  campo `cloud_id`. Propuesta: ambos.

## Reglas que no se negocian

- Nombres Unix = bytes + `encoding`; nunca `String` para el nombre.
- Fuente por lotes; nunca materializar el árbol completo en un `Vec`.
- Solo lectura: indexar/consultar; nada borra, mueve ni renombra.
- Offline es opt-in explícito; sin atribución → `posible_duplicado`, nunca
  fusión silenciosa; los overrides manuales mandan.
- `mount` es presentación: no entra en `DiskHwId`, `volume_id` ni `cloud_id`.
- Antes un volumen faltante que dos discos fusionados.
