//! Census para macOS (Intel y Apple silicon).
//!
//! Índice de archivos en RAM con snapshot binario, consultas estilo
//! Everything, catálogo de índices de varias PC por Tailscale y servidor MCP.
//!
//! El diseño sigue el contrato de `docs/PLATAFORMAS.md`: nodos planos, arena
//! de nombres, `Compacto` de 12 B y rangos de subárbol. macOS aporta
//! `getattrlistbulk` para enumerar y FSEvents para actualizar en caliente.
//!
//! Regla de producto: indexar y consultar es **read-only**. No se borra nada.

pub mod config;
pub mod fmt;
pub mod fuente_mac;
pub mod index;
pub mod query;
pub mod source;
pub mod time;
pub mod types;
pub mod volumes;

pub use index::{Compacto, Index, Node, Stats};
pub use query::{Query, Sort};
pub use types::{
    Cambio, CambioKind, Encoding, Entrada, FileId, FileKey, Kind, Marca, Provenance, Volume,
};

/// Cómo se llenó el índice de un volumen.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Method {
    /// Recorrido de directorios portable. La vía universal.
    #[default]
    Walk,
    /// `getattrlistbulk` en macOS.
    Getattrlistbulk,
    /// `C.wcat` importado de otra plataforma.
    Wcat,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::Walk => "walk",
            Method::Getattrlistbulk => "getattrlistbulk",
            Method::Wcat => "wcat",
        }
    }
}

pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

pub fn platform_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else {
        "linux"
    }
}

pub fn arch_name() -> &'static str {
    std::env::consts::ARCH
}
