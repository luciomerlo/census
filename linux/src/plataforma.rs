//! Contrato neutro acordado en `docs/PLATAFORMAS.md`.
//!
//! Windows, macOS y Linux comparten estos tipos (cada crate tiene su copia,
//! no hay código compartido). Los nombres viajan como bytes + `encoding`:
//! en Linux un nombre de archivo no tiene por que ser UTF-8.

use std::path::Path;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Hash)]
pub enum Kind {
    #[default]
    File,
    Dir,
    Symlink,
    Other,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Encoding {
    #[default]
    Utf8,
    Utf16Le,
    Otro,
}

impl Encoding {
    pub fn detect(name: &[u8]) -> Encoding {
        if std::str::from_utf8(name).is_ok() {
            Encoding::Utf8
        } else {
            Encoding::Otro
        }
    }
}

/// Clave de arbol: lo que identifica un nodo dentro de un volumen.
/// En Linux `gen` queda en 0 (`i_generation` no es portable).
#[derive(Clone, PartialEq, Eq, Debug, Default, Hash)]
pub struct FileKey {
    pub volume_id: String,
    pub ino: u64,
    pub gen: u32,
}

impl FileKey {
    pub fn linux(volume_id: &str, ino: u64) -> Self {
        Self {
            volume_id: volume_id.to_string(),
            ino,
            gen: 0,
        }
    }
}

/// Identidad completa para Atlas: clave + `btime` para desempatar inodos
/// reutilizados. Si el filesystem no da `STATX_BTIME`, `btime_ns = 0`.
#[derive(Clone, PartialEq, Eq, Debug, Default, Hash)]
pub struct FileId {
    pub key: FileKey,
    pub btime_ns: i64,
}

impl FileId {
    pub fn linux(volume_id: &str, ino: u64, btime_ns: i64) -> Self {
        Self {
            key: FileKey::linux(volume_id, ino),
            btime_ns,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entrada {
    pub id: FileId,
    pub parent_id: FileId,
    pub dev_id: u64,
    pub kind: Kind,
    pub size: u64,
    pub alloc: u64,
    pub mtime_ns: i64,
    pub btime_ns: i64,
    pub nlink: u32,
    pub flags: u32,
    pub name: Vec<u8>,
    pub encoding: Encoding,
}

#[derive(Clone, Debug)]
pub struct Cambio {
    /// Linux: inodo del archivo. `None` si el evento no lo trae.
    pub id: Option<FileId>,
    pub parent: Option<FileId>,
    /// Ruta del evento (macOS/Linux).
    pub path: Vec<u8>,
    /// Nombre del registro (Windows/USN).
    pub nombre: Option<Vec<u8>>,
    pub kind: Kind,
    pub marca: Marca,
}

/// Version del indice. Opaca: solo la interpreta su plataforma.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Marca(pub String);

impl Marca {
    pub fn linux(generation: u64, cambio_n: u64) -> Self {
        Marca(format!("l1:{generation}:{cambio_n}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for Marca {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Contrato de fuente de nodos: por lotes, nunca un `Vec` con todo.
pub trait Fuente {
    fn nombre(&self) -> &'static str;
    fn escanear(
        &self,
        raiz: &Path,
        emit: &mut dyn FnMut(&[Entrada]) -> Result<(), String>,
    ) -> Result<(), String>;
}

pub const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
pub const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

#[inline]
pub fn fnv_update(mut h: u64, bytes: &[u8]) -> u64 {
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    h
}

pub fn fnv1a64(bytes: &[u8]) -> u64 {
    fnv_update(FNV_OFFSET, bytes)
}

/// `volume_id = hash(fs_uuid || fstype || total_size)`, normativo del acuerdo.
/// Se concatenan los bytes sin separador; el tamano va little-endian.
pub fn volume_id(fs_uuid: &str, fstype: &str, total_size: u64) -> String {
    let mut buf = Vec::with_capacity(fs_uuid.len() + fstype.len() + 8);
    buf.extend_from_slice(fs_uuid.as_bytes());
    buf.extend_from_slice(fstype.as_bytes());
    buf.extend_from_slice(&total_size.to_le_bytes());
    format!("{:016x}", fnv1a64(&buf))
}

/// Letra sintetica estable para `export --wcat`: se deriva del `volume_id`
/// ordenado, nunca del orden de montaje.
pub fn synthetic_letter(index: usize) -> char {
    // A..Z, despues AA..AZ, BA.. (poco probable, pero no se repite).
    const L: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ";
    if index < 26 {
        L[index] as char
    } else {
        let a = (index / 26 - 1).min(25);
        let b = index % 26;
        L[a] as char
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_id_is_stable_and_sensitive() {
        let a = volume_id("uuid-1", "ext4", 1000);
        assert_eq!(a, volume_id("uuid-1", "ext4", 1000));
        assert_ne!(a, volume_id("uuid-1", "ext4", 1001));
        assert_ne!(a, volume_id("uuid-2", "ext4", 1000));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn marca_format() {
        assert_eq!(Marca::linux(7, 3).to_string(), "l1:7:3");
    }

    #[test]
    fn encoding_detects_non_utf8() {
        assert_eq!(Encoding::detect(b"normal.txt"), Encoding::Utf8);
        assert_eq!(Encoding::detect(&[0xff, 0xfe]), Encoding::Otro);
    }

    #[test]
    fn synthetic_letters_are_stable() {
        assert_eq!(synthetic_letter(0), 'A');
        assert_eq!(synthetic_letter(25), 'Z');
        assert_ne!(synthetic_letter(26), synthetic_letter(0));
    }
}
