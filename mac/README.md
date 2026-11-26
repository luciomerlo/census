# CensusIndex para macOS

Índice de archivos para macOS (Intel y Apple silicon). Lee el disco con
`getattrlistbulk`, guarda un snapshot propio y contesta consultas en
milisegundos. Es un crate independiente del core Windows (`src/`) y del crate
Linux (`linux/`), y respeta el contrato de `docs/PLATAFORMAS.md`.

Estado: **alpha**. El núcleo, los tipos, la consulta, la identidad de volúmenes
y la fuente `getattrlistbulk` están implementados y compilados para ambos
targets. Falta el snapshot, la CLI completa, FSEvents, MCP y Tailscale; el
detalle está en [PENDIENTE.md](PENDIENTE.md).

## Cómo funciona en macOS

1. **Enumerar**: `getattrlistbulk(2)` devuelve nombre, inodo, padre, tipo,
   tamaño, espacio asignado, `mtime` y `btime` de cientos de hijos por syscall.
   No hay un `lstat` por archivo. Los directorios se reparten entre workers.
2. **Indexar**: nodos planos + arena de nombres UTF-8 + `Compacto` de 12 B +
   hijos CSR + rangos de subárbol. Es la misma forma que Windows/Linux para que
   la consulta sea idéntica.
3. **Consultar**: sintaxis de CensusIndex (`path:`, `file:`, `folder:`, `ext:`,
   `size:`, `dm:`, `rc:`, términos), orden y paginación.
4. **Actualizar**: FSEvents dará un `eventId` durable y monótono por volumen,
   equivalente al USN Journal de NTFS. La `Marca` será
   `m1:<vol_uuid>:<last_event_id>`.
5. **Identidad**: la letra o `/Volumes/X` no identifican nada.
   `DiskHwId` sale de `diskutil info -plist` (serial o `DiskUUID`),
   `volume_id = hash(DiskHwId || offset || fs_uuid)` y el mount es sólo
   presentación. Si cambia el mount, se remapea el prefijo del snapshot.
6. **Nube**: `cloud_id = FNV-1a64(provider \0 account \0 unit)`.
   `CloudStorage/GoogleDrive-<cuenta>/Shared drives/<drive>` y los montajes de
   rclone se unifican con alias manual; sin cuenta atribuible queda
   `posible_duplicado` y no se fusiona.
7. **Compartir**: pendiente. El diseño es el mismo de Atlas: snapshot opaco,
   `GET /query` al dueño cuando está online, selección explícita de volúmenes
   offline y Tailscale para el transporte.

## Compilar

Requisitos: macOS 12+, Rust stable, `xcode-select --install` (Command Line
Tools).

```bash
cd mac

# Arquitectura de esta Mac
cargo build --release

# Intel y Apple silicon
rustup target add x86_64-apple-darwin aarch64-apple-darwin
cargo build --release --target x86_64-apple-darwin
cargo build --release --target aarch64-apple-darwin

# Universal2
./scripts/make-universal.sh

# Apps .app para Intel, Apple Silicon y universal2
# (firma ad-hoc; con MACOS_SIGN_IDENTITY usa Developer ID)
./scripts/build-apps.sh
```

El binario queda en `target/<target>/release/censusindex` y, para universal2, en
`dist/censusindex-universal`. `build-apps.sh` deja `dist/CensusIndex-Intel.app`,
`dist/CensusIndex-AppleSilicon.app`, `dist/CensusIndex-universal.app` y sus `.zip` con
`SHA256SUMS.txt`.

En Linux/Windows se puede verificar el código portable y el código macOS sin
enlazar:

```bash
cargo test
cargo check --target x86_64-apple-darwin
cargo check --target aarch64-apple-darwin
```

## Primeros comandos

```bash
cargo run -- version
cargo run -- volumes
cargo run -- cloud-id gdrive alice@gmail.com my-drive
cargo run -- fold "ÑANDÚ"
```

`cloud-id` imprime `28590f656b6fe252` para ese ejemplo: es el vector compartido
con Linux y Windows.

Cuando esté la CLI completa:

```bash
censusindex index                 # volúmenes locales; nube/red sólo con --include-*
censusindex query "informe"
censusindex top --n 20
censusindex export --format wcat --out ~/Desktop/CensusIndex.wcat
censusindex serve                 # loopback:8478
censusindex mcp                   # stdio para agentes
```

## Full Disk Access

Para leer Mail, Messages, Safari y otras carpetas protegidas hay que darle
Full Disk Access al binario o a la app en **Ajustes del Sistema › Privacidad y
seguridad › Acceso total al disco**. Sin ese permiso CensusIndex indexa lo que puede
y lo informa; no pide `sudo` ni desactiva protecciones.

## Identidad y compatibilidad

- `FileKey = (volume_id, ino, gen)` para el árbol; `FileId` agrega `btime_ns`
  para identidad y Atlas. `st_gen` de APFS puede ser 0; no se depende de él.
- Hardlinks: mismo inodo, varias rutas; el tamaño se cuenta una vez.
- Clones APFS: inodos distintos con extents compartidos; se contarán como
  copias que liberan poco, no como el mismo archivo.
- Symlinks: se indexa el enlace, no se sigue.
- Nombres: bytes + `encoding`; nunca `String` en el contrato.
- Snapshot local: `.cnx` (pendiente). `C.wcat` v3 se exporta para el hub de
  Windows con nombres UTF-16LE normalizados por el dueño.

## Estructura

| Archivo | Qué hace |
|---|---|
| `src/types.rs` | `FileKey`/`FileId`, `Entrada`, `Cambio`, `Marca`, `Volume` |
| `src/index.rs` | índice en RAM y builder |
| `src/query.rs` | parser y evaluación de consultas; plegado normativo |
| `src/fuente_mac.rs` | `getattrlistbulk` + workers |
| `src/source.rs` | recorrido portable para red/nube y tests |
| `src/volumes.rs` | `getmntinfo`, DiskHwId, `cloud_id`, raíces |
| `src/config.rs` | configuración, overrides, selección offline |
| `scripts/` | builds, universal2, `.app`, firma/notarización |
| `packaging/` | `Info.plist`, entitlements, LaunchAgent |

## Licencia

MIT, la misma del repositorio.
