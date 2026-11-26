# Acuerdo entre los chats de CensusIndex (Windows / macOS / Linux)

Fecha: 2026-11-22. Base: `cbb341a`. Este documento es el contrato; si algo no
está acá, no está acordado.

## 1. Estructura del repo

| Parte | Dónde vive | Quién |
|---|---|---|
| Core Windows (MFT, USN, consulta, snapshot) | `src/` | chat Windows |
| macOS | **crate standalone `mac/`** | chat Mac |
| Linux | **crate standalone `linux/`** | chat Linux |

Motivo de los crates separados: el core actual es Windows-only (`source.rs`,
`runs.rs`, `win.rs` con `#![cfg(windows)]`, nombres u16 y snapshot de MFT) y no
compila en macOS ni Linux. Forzar un core portable ahora obliga a refactorizar
`Index`/`snapshot` y pone en riesgo C2 (journal en caliente).

Los tres crates comparten **contrato de tipos y de wire**, no código. Si más
adelante se extrae un `censusindex-core` portable, los dos fuentes se adaptan: el
contrato de abajo es el que hay que respetar.

## 2. Fuente de nodos: por lotes, no `Vec` completa

```rust
pub trait Fuente {
    fn nombre(&self) -> &'static str;                  // "mft", "getattrlistbulk", "getdents"
    fn escanear(&self, raiz: &Path, emit: &mut dyn FnMut(&[Entrada]) -> Result<()>) -> Result<()>;
}
```

`Vec<Entrada>` completa no escala: con 2 M de nodos duplica el índice y el pico
de RAM. macOS emite 256–1024 entradas por buffer de `getattrlistbulk`; Windows
emite por bloques de registros MFT; Linux por lotes de `getdents64`.

## 3. `Entrada` y `Cambio` neutros

```rust
pub struct Entrada {
    pub id: FileId,          // identidad estable, ver §5
    pub parent_id: FileId,
    pub dev_id: u64,
    pub kind: Kind,          // archivo / carpeta / enlace / otro
    pub size: u64,
    pub alloc: u64,          // tamaño asignado (para "espacio real")
    pub mtime_ns: i64,
    pub btime_ns: i64,       // creación; 0 si no existe
    pub nlink: u32,
    pub flags: u32,
    pub name: Vec<u8>,       // bytes, no String
    pub encoding: Encoding,  // Utf8 | Utf16Le | Otro
}

pub struct Cambio {
    pub id: Option<FileId>,        // Windows: FRN. macOS: None (FSEvents no lo da)
    pub parent: Option<FileId>,
    pub path: Vec<u8>,             // macOS/Linux: ruta del evento
    pub nombre: Option<Vec<u8>>,   // Windows: nombre del registro USN
    pub kind: Kind,
    pub marca: Marca,              // ver §4
}
```

Reglas:

- **Nunca `String`**: macOS admite nombres que no son UTF-8 y Windows admite
  surrogates sueltos. Todo nombre viaja como bytes + `encoding`.
- `Cambio` es neutro: FSEvents entrega ruta + flags + `eventId`, no inodo, y
  hacer `stat` después del evento corre carrera. Por eso `id` es `Option` y en
  macOS se resuelve por `path`.
- Windows hoy tiene su propio `Cambio` en `usn.rs`; se mapea 1:1 a este tipo en
  la frontera (ya tiene FRN, padre, nombre, motivo y atributos).

## 4. `Marca`: versión del índice, opaca y por plataforma

```
w1:<journal_id>:<next_usn>          Windows (NTFS USN)
m1:<vol_uuid>:<last_event_id>       macOS (FSEvents eventId, monótono por volumen)
m1:g<generation>:e<event_id>        macOS tras reindexado completo
l1:<generation>:<cambio_n>          Linux (fanotify no tiene orden durable)
```

- La `Marca` se serializa como string y **sólo la interpreta su plataforma**.
- Atlas compara marcas **sólo si coinciden `(pc_id, volume_id)`**. Si cambia la
  plataforma o el volumen, el orden es `built_at` y después `content_hash`.
- Con `eventId` o USN disponibles, **el reloj no entra en el orden**: los
  relojes entre PCs mienten (zona horaria, BIOS, hibernación).

## 5. Identidad

### Archivo

| Plataforma | `FileId` |
|---|---|
| Windows | `{ volume_id, frn: u64 }` (FRN ya enmascarado a 48 bits) |
| macOS | `{ volume_uuid: [u8;16], ino: u64, gen: u32, btime_ns: i64 }` — con `ATTR_CMN_FILEID` + `ATTR_CMN_CRTIME`. `st_gen` puede ser 0 en APFS: **no se depende de él**, `btime` desempata |
| Linux | `{ fs_uuid, ino: u64, btime_ns: i64 }` — `i_generation` de ext4 no es portable |

**`FileKey` vs `FileId`** — son dos cosas distintas y el contrato las separa:

```rust
pub struct FileKey { pub volume_id: VolumeId, pub ino: u64, pub gen: u32 }
pub struct FileId  { pub key: FileKey, pub btime_ns: i64 }
```

- **`FileKey`** es para *lookup del árbol*: es lo único que trae el padre
  (`ATTR_CMN_PARENTID` no incluye `btime`).
- **`FileId`** es para *identidad y Atlas*: agrega `btime_ns` para desempatar
  reutilización de inodo.

En Windows `FileKey = (volume_id, frn)` y `btime_ns` no existe (0).

Casos especiales:

- **Hardlinks** comparten inodo: se cuenta una vez y `nlink` dice cuántos nombres.
- **Clones APFS** tienen inodos distintos con extents compartidos: se agrupan
  como "copias que liberan poco espacio", no como el mismo archivo.

### Volumen

| Plataforma | `volume_id` |
|---|---|
| Windows | `vol_serial` (+ tamaño) — **es por volumen, no por disco físico** |
| macOS | `hash(fs_uuid || fstype || total_size || volume_name)`; UUID de `ATTR_VOL_UUID`, fallback `statfs.f_fsid + size` |
| Linux | `hash(fs_uuid || fstype || total_size)` de `statfs`/`blkid` |

`volume_id` es **FNV-1a 64 en hex minúsculas** en las tres plataformas, sobre
`fs_uuid || fstype || total_size`. Fallbacks acordados:

- macOS: `ATTR_VOL_UUID` → `statfs.f_fsid + size` → `hash(volume_name + size)`.
- Linux: `/dev/disk/by-uuid` (match por `source` canónico) → `blkid -s UUID -o value`
  → `fsid`. Directorios sueltos (`--root`): `dir:<hostname>:<path>`.
- Windows: `vol_serial` (lo da `GetVolumeInformationW`, ya implementado).

`volume_id` es lo que usa Atlas. **Renombrado hecho** en `docs/ATLAS.md`
(antes decía `disk_id`, que se confundía con disco físico: `C:` y `D:` en el
mismo SSD son volúmenes distintos).

## 6. `Compacto` y snapshot

- El registro de escaneo `Compacto` (12 B: `off`, `len`, `flags`, `parent`) lo
  produce el **builder común** de cada crate; el layout es el mismo en las tres
  plataformas para que la consulta sea idéntica.
- **`C.wcat` no se toca** (v3, little-endian, campo por campo).
- Objeción de macOS aceptada: el hub **no** parsea nombres ajenos para
  consultarlos. Dos caminos:
  1. **Por defecto**: el hub guarda snapshots **opacos** y consulta al dueño con
     `GET /query` cuando está online. Cero conversiones, cero pérdida.
  2. **Offline de otra plataforma**: al publicar, el dueño **normaliza** los
     nombres a UTF-16LE (con reemplazo sólo si el nombre no era UTF-8 válido) y
     firma el snapshot. Así el hub parsea un solo formato y la eventual pérdida
     ocurre en el dueño, no en el hub.

## 6 bis. Snapshots nativos y export

- Cada crate puede tener su snapshot nativo (Linux: `.cns`, macOS: `.cnx`) con
  bytes, multi-raíz, `pc_id`, `volume_id`, `Marca` y `content_hash`. El hub los
  guarda **opacos**.
- Para que el core Windows verifique y para offline entre plataformas existe
  `censusindex export --wcat`: produce un `C.wcat` v3 exacto (nombres UTF-16LE,
  letras sintéticas) más un `manifest.json` al lado con:
  `letter -> { root_path, volume_id, Marca, content_hash, platform }`.
- **Las letras sintéticas tienen que ser estables**: derivadas de `volume_id`
  (por ejemplo `A:`, `B:`, … según el orden de los `volume_id` ordenados), nunca
  del orden de montaje. Si cambian entre exports, Atlas ve volúmenes distintos
  en cada publicación.
- La conversión a UTF-16 ocurre **en el dueño**; un nombre inválido se reemplaza
  por U+FFFD al exportar, no antes.

## 6 ter. Plegado de nombres (normativo)

La tabla es la misma en las tres plataformas, para que un término con acento
encuentre lo mismo en cualquiera:

- ASCII `A-Z` → `a-z`.
- Latin-1 `U+00C0..=U+00DE` **menos `U+00D7`** → `+0x20`.
- Nada más: no se quitan acentos, no se tocan ligaduras, no se usa el
  `to_lowercase` de Unicode.

En Linux/macOS se aplica sobre los escalares UTF-8 preservando offsets; si el
nombre no es UTF-8 válido, se pliega sólo ASCII y se marca `Encoding::Otro`.

## 7. Reglas de trabajo

- `git pull --rebase` antes de empezar; commits chicos y acotados a los
  archivos propios.
- `cargo test --workspace` y `cargo clippy --workspace --all-targets -- -D warnings`
  en 0 antes de pushear.
- No cambiar el formato `C.wcat` ni el layout de `Compacto` sin avisar acá.
- Lo que se agrega se documenta en `docs/`.

## 8. Pendiente de Linux

- Confirmar `l1:<generation>:<cambio_n>` y si puede dar `btime` (`statx` con
  `STATX_BTIME`; no todos los filesystems lo soportan).
- Confirmar el id de volumen (`fs_uuid`) y el rendimiento medido de
  `getdents64`: la referencia de Windows es 1.64 M de nodos en 4,9 s (335k/s).
