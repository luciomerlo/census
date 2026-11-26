//! Census para Linux.
//!
//! Indice de archivos en RAM con snapshot binario, consultas estilo Everything,
//! catalogo de indices de varias PC por Tailscale y servidor MCP.
//!
//! El diseno es el mismo de Census/Windows: nodos planos, arena de nombres,
//! registro compacto para escanear y rangos de subarbol para resolver rutas.
//! Cambian tres cosas porque Linux no es NTFS:
//!
//! * los nombres son bytes (no UTF-16) y pueden no ser UTF-8;
//! * no hay MFT: cada punto de montaje se recorre en paralelo y se puede podar
//!   por `mtime` cuando ya hay un indice anterior;
//! * cada raiz lleva una identidad de disco estable (UUID) para que dos PC
//!   puedan comparar la misma unidad aunque este montada en otra ruta.

pub mod ai;
pub mod api;
pub mod catalog;
pub mod cloud;
pub mod config;
pub mod devname;
pub mod diskid;
pub mod fmt;
pub mod http;
pub mod index;
pub mod mcp;
pub mod mirrors;
pub mod plataforma;
pub mod query;
pub mod scan;
pub mod snapshot;
pub mod tailscale;
pub mod time;
pub mod volumes;
pub mod watch;

pub use index::{Index, Node, Root, Stats};
pub use query::{Query, Sort};

/// Como se lleno el indice de una unidad.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Method {
    /// Recorrido de directorios paralelo. La via universal.
    #[default]
    Walk,
    /// Enumeracion nativa de un filesystem, cuando exista y haya permisos.
    Native,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Method::Walk => "walk",
            Method::Native => "native",
        }
    }
}
