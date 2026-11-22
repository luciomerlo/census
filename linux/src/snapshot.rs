//! Snapshot binario `.cns` (little-endian, checksum FNV-1a).
//!
//! PENDIENTE: cabecera + host + raices + nodos de 64 B + arenas + indices
//! derivados; `read_meta` para el catalogo sin cargar nodos; remapeo de prefijo
//! cuando cambia el punto de montaje. No se toca `C.wcat` (v3).
use crate::index::Index;
use std::path::Path;

pub fn save(_ix: &Index, _path: &Path) -> Result<u64, String> {
    Err("snapshot .cns todavia no implementado (ver linux/PENDIENTE.md)".into())
}

pub fn load(_path: &Path) -> Result<Index, String> {
    Err("snapshot .cns todavia no implementado (ver linux/PENDIENTE.md)".into())
}
