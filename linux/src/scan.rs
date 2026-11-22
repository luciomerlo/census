//! Fuente Linux: `getdents64` + `statx` en un pool de workers.
//!
//! PENDIENTE: implementacion completa (walk paralelo, ensamblado en preorden,
//! poda por mtime, `WalkSource` del contrato). El diseno esta en
//! `linux/PENDIENTE.md` y en `docs/PLATAFORMAS.md` §2.
use crate::index::Index;
use crate::volumes::Mount;
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct ScanOptions {
    pub full: bool,
    pub jobs: usize,
    pub excludes: Vec<PathBuf>,
    pub keep_other_roots: bool,
    pub host: crate::index::HostInfo,
    pub pc_id: String,
}

#[derive(Clone, Debug, Default)]
pub struct RootReport {
    pub path: String,
    pub volume_id: String,
    pub nodes: u64,
    pub files: u64,
    pub dirs: u64,
    pub complete: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub roots: Vec<RootReport>,
    pub nodes: u64,
    pub files: u64,
    pub dirs: u64,
    pub walk_ms: u128,
    pub assemble_ms: u128,
    pub derived_ms: u128,
}

pub fn index_roots(
    _roots: Vec<Mount>,
    _old: Option<Arc<Index>>,
    _opts: &ScanOptions,
) -> Result<(Index, ScanReport), String> {
    Err("scanner getdents64/statx todavia no implementado (ver linux/PENDIENTE.md)".into())
}
