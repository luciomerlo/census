//! Índice instantáneo de archivos para Windows.

pub mod ai;
#[cfg(feature = "gui")]
pub mod gui;
pub mod index;
pub mod mcp;
pub mod query;
pub mod service;
pub mod snapshot;
#[cfg(feature = "gui")]
pub mod theme;
pub mod time;
pub mod volumes;

#[cfg(windows)]
pub mod runs;
#[cfg(windows)]
pub mod source;
#[cfg(windows)]
pub mod win;

pub use index::{Index, Node, Stats};
pub use query::{Query, Sort};
pub use service::Service;
pub use volumes::DriveKind;

use std::path::Path;
use std::sync::Arc;

/// Cómo se llenó el índice de una unidad.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Method {
    /// Tabla maestra de archivos: lo que hace Everything, sólo en NTFS local.
    Mft,
    /// Recorrido de directorios: la única opción en red, nube y exFAT.
    Walk,
}

impl Method {
    pub fn label(&self) -> &'static str {
        match self {
            Method::Mft => "mft",
            Method::Walk => "recorrido",
        }
    }
}

/// Lo que pasó con una unidad: sirve para la barra de estado y para `census drives`.
#[derive(Clone, Debug)]
pub struct DriveReport {
    pub name: String,
    pub kind: DriveKind,
    pub fs: String,
    pub method: Method,
    pub nodes: usize,
    pub ms: u128,
    /// Si la MFT falló y hubo que recorrer, o si falló del todo.
    pub note: Option<String>,
}

impl DriveReport {
    pub fn summary(&self) -> String {
        format!(
            "{} {} ({}, {}) {} nodos/{} ms",
            self.name,
            self.kind.label(),
            if self.fs.is_empty() { "-" } else { &self.fs },
            self.method.label(),
            self.nodes,
            self.ms
        )
    }
}

/// Carga el índice de un volumen NTFS leyendo su MFT. Requiere elevación.
#[cfg(windows)]
fn build_volume(letter: char) -> Result<(Index, source::ScanStats), String> {
    let t0 = std::time::Instant::now();
    let mut b = index::Builder::with_capacity(0);
    let stats = source::scan_mft(letter, |r| {
        b.push(r);
    })?;
    let mut ix = b.finish(letter);
    ix.stats.records = stats.records;
    ix.stats.skipped = stats.skipped;
    ix.stats.build_ms = t0.elapsed().as_millis();
    ix.built_at = chrono::Local::now().timestamp();
    Ok((ix, stats))
}

#[cfg(windows)]
pub fn index_volume(letter: char) -> Result<(Arc<Index>, source::ScanStats), String> {
    build_volume(letter).map(|(ix, st)| (Arc::new(ix), st))
}

/// Índice de un árbol de directorios, con la raíz de la unidad que lo contiene.
#[cfg(windows)]
fn build_dir(root: &Path) -> Result<(Index, source::ScanStats), String> {
    let t0 = std::time::Instant::now();
    let mut b = index::Builder::with_capacity(0);
    let stats = source::scan_dir(root, |r| {
        b.push(r);
    })?;
    let letter = root
        .components()
        .next()
        .map(|c| {
            c.as_os_str()
                .to_string_lossy()
                .chars()
                .next()
                .unwrap_or('C')
        })
        .unwrap_or('C');
    let mut ix = b.finish(letter);
    ix.stats.records = stats.records;
    ix.stats.build_ms = t0.elapsed().as_millis();
    ix.built_at = chrono::Local::now().timestamp();
    Ok((ix, stats))
}

#[cfg(windows)]
pub fn index_dir(root: &Path) -> Result<(Arc<Index>, source::ScanStats), String> {
    build_dir(root).map(|(ix, st)| (Arc::new(ix), st))
}

/// Índice de **todas** las unidades disponibles: locales por MFT, y de red, nube
/// o exFAT recorriendo carpetas. Si la MFT falla (sin elevación) se cae al
/// recorrido en vez de dejar la unidad afuera.
#[cfg(windows)]
pub fn index_all() -> Result<(Arc<Index>, Vec<DriveReport>), String> {
    let drives = volumes::selected();
    if drives.is_empty() {
        return Err("no hay unidades indexables".into());
    }
    let mut vols = Vec::with_capacity(drives.len());
    let mut reports = Vec::with_capacity(drives.len());
    for d in drives {
        let mut note = None;
        let (ix, method, nodes, ms) = if d.uses_mft() {
            match build_volume(d.letter()) {
                Ok((ix, st)) => {
                    let n = ix.len();
                    (ix, Method::Mft, n, st.elapsed_ms)
                }
                Err(e) => {
                    note = Some(format!("mft no disponible ({e}); se recorre"));
                    match build_dir(Path::new(&format!("{}:\\", d.letter()))) {
                        Ok((ix, st)) => {
                            let n = ix.len();
                            (ix, Method::Walk, n, st.elapsed_ms)
                        }
                        Err(e2) => {
                            reports.push(DriveReport {
                                name: d.name.clone(),
                                kind: d.kind(),
                                fs: d.fs.clone(),
                                method: Method::Walk,
                                nodes: 0,
                                ms: 0,
                                note: Some(format!("no se pudo indexar: {e2}")),
                            });
                            continue;
                        }
                    }
                }
            }
        } else {
            match build_dir(Path::new(&format!("{}:\\", d.letter()))) {
                Ok((ix, st)) => {
                    let n = ix.len();
                    (ix, Method::Walk, n, st.elapsed_ms)
                }
                Err(e) => {
                    reports.push(DriveReport {
                        name: d.name.clone(),
                        kind: d.kind(),
                        fs: d.fs.clone(),
                        method: Method::Walk,
                        nodes: 0,
                        ms: 0,
                        note: Some(format!("no se pudo indexar: {e}")),
                    });
                    continue;
                }
            }
        };
        vols.push(ix);
        reports.push(DriveReport {
            name: d.name.clone(),
            kind: d.kind(),
            fs: d.fs.clone(),
            method,
            nodes,
            ms,
            note,
        });
    }
    if vols.is_empty() {
        return Err(format!(
            "ninguna unidad se pudo indexar: {}",
            reports
                .iter()
                .map(|r| format!("{} {}", r.name, r.note.clone().unwrap_or_default()))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }
    let mut merged = Index::merge(vols);
    merged.built_at = chrono::Local::now().timestamp();
    Ok((Arc::new(merged), reports))
}

#[cfg(not(windows))]
pub fn index_all() -> Result<(Arc<Index>, Vec<DriveReport>), String> {
    Err("Census sólo indexa volúmenes en Windows".into())
}
