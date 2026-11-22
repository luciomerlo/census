# Linux: estado y pendientes

Actualizado: 2026-11-22. Crate standalone en `linux/` (`[workspace]` propio, no
es miembro del workspace de Windows). No toca `src/`, `C.wcat` ni `mac/`.

## v1 hecho y verificado

- **CLI**: `index` (alias `scan`), `query`, `top`, `stats`, `volumes --json`,
  `serve`. `index` sin args usa montajes locales (sin red/nube); con ruta
  explícita indexa esa ruta.
- **Snapshot global** `.cns` en `CENSUS_DATA_DIR/index.cns`: nodos de 64 B,
  nombres en bytes, raíces con `volume_id`/`marca`, checksum FNV-1a,
  guardado atómico; los índices derivados se reconstruyen al cargar.
- **Walker** `getdents64` (64 KiB) + `statx` (`STATX_BTIME`, `NOFOLLOW`), con
  fallback portable para tests en Windows; preorden, `tout`, hardlinks,
  nombres no UTF-8, `one_fs` y exclusión por prefijo.
- **Identidad integrada**: `Mount`/`Root` llevan `disk_hw_id`, `serial`,
  `model`, `capacity`, `partition_offset`, `identity_status`, `federable`;
  `volume_id = hash(DiskHwId || offset || fs_uuid)`, nunca `sdX`/letra/mount.
  `mark_ambiguous` + `disambiguate`; `sin_identidad`/`serial_ambiguo` no federa;
  dos discos `BACKUP` con distinta serial no se fusionan.
- **HTTP v1** en `127.0.0.1:8787`: `/v1/info`, `/health`, `/volumes`, `/search`,
  `/top`, `/stats`; envelopes, `source:{pc_id,state}`, `--lan` con Bearer,
  puerto ocupado = exit 1.
- **JSON estable** para Sort/Wiz/Ocupa (`docs/MINIMO.md`): volúmenes con 17
  campos + `marca` (null si no hay snapshot), archivos con
  `path,name,size_bytes,alloc_bytes,mtime_ns,kind,volume_id,source`.
- **Empaquetado Intel/ARM**: `scripts/build-linux.sh` genera
  `dist/census-0.1.0-linux-x86_64.tar.gz` y `...-aarch64.tar.gz` (musl
  estático) + `SHA256SUMS.txt`; service systemd e `install.sh`.
- **Calidad**: 42 tests en Windows; `cargo clippy --target
  x86_64-unknown-linux-musl --all-targets -- -D warnings` limpio; builds
  `x86_64-unknown-linux-musl` y `aarch64-unknown-linux-musl` OK.
- Workflow de CI Linux (`e456859`, de Wiz/Mac): matriz `ubuntu-latest` +
  `ubuntu-24.04-arm`.

No hay mediciones de rendimiento todavía: no se inventan números. Referencia
Windows 335k nodos/s; objetivo Linux 250k-600k/s en NVMe, a medir en hierro.

## P0 — completar v1 sin cambiar el contrato

1. **Reutilización/poda**: `index` hoy siempre recorre todo; implementar poda
   por `mtime`/`dev`/`ino` contra el snapshot anterior y `copy_subtree` para
   raíces no recorridas. `--full` ya existe y debe forzar el recorrido.
2. **Remapeo de montaje**: si cambia `/media/A` → `/mnt/A`, mismo `volume_id`,
   actualizar `Root.path` y los prefijos sin reindexar; no mezclar discos con
   igual etiqueta. `keep_other_roots` para conservar raíces desconectadas.
3. **Snapshot incremental**: `read_meta` para `stats`/`volumes` sin cargar
   nodos; sección por volumen y compactación; tolerancia a snapshots truncados
   ya cubierta, falta migración de versión.
4. **`query`**: paralelizar el barrido sobre `compacto`, filtros `file:`,
   `folder:`, `dm:`/`dc:`/`da:`/`rc:` y paginado; hoy tiene términos, `path:`,
   `ext:`, `size:`, sort y limit.
5. **Catálogo multi-PC** (`catalog.rs`/`mirrors.rs`): cargar `.cns` locales y
   remotos, agrupar por `(pc_id, volume_id)`/`cloud_id`, desempatar por
   `Marca` y luego contenido/`built_at`, offline opt-in. **Fuera de v1**.
6. **`tailscale.rs` + HTTP remoto**: `tailscale status --json`, peers y
   `serve` publicado; el HTTP local ya está. Sin UDP.
7. **`mcp.rs`/`api.rs`**: herramientas MCP y capa API común; **fuera de v1**.
8. **`watch.rs`**: inotify/fanotify, subárboles sucios y recarga sin reiniciar.
9. **Vías nativas**: btrfs `TREE_SEARCH_V2`, XFS `FSBULKSTAT`, ext4
   `libext2fs`/`debugfs` como aceleradores opcionales; el walker portable sigue
   siendo la vía universal.
10. **`export --wcat`**: letras sintéticas estables por `volume_id` ordenado +
    `manifest.json`, sólo para verificación cruzada con Windows (no es cliente).
11. **Medir en Linux real**: `index` sobre 1M+ archivos, `query/top`, carga de
    `.cns`; pasar número + primer `.cns` al chat Windows para prueba cruzada.
12. **Tests Linux**: correr `cargo test` en CI (hoy los tests corren en Windows;
    CI Linux corre clippy/build). Añadir integración HTTP y snapshot corrupto en
    el runner ARM.

## Bloqueos / notas

- Sin WSL/Docker en esta máquina (HCS no disponible); los binarios Linux se
  cross-compilan con `rust-lld` desde Windows y se validan en CI.
- Tailscale/MCP/federación quedan fuera de v1; el usuario pidió minimalismo.
- `docs/MINIMO.md` es el contrato congelado; si cambia, actualizar CLI, HTTP y
  tests a la vez.
- Reglas: nombres bytes + `encoding`; sólo lectura; offline opt-in; el mount es
  presentación; antes un volumen faltante que dos fusionados.
