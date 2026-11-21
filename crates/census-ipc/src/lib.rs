//! Protocolo y transporte IPC de Census.
//!
//! - [`protocol`]: contrato de bytes (QUERY2W) compartido con las apps.
//! - [`server`]: ventana oculta que responde consultas.
//! - [`client`]: cliente que consulta a Census o al Everything real.

pub mod client;
pub mod protocol;
pub mod server;

pub use client::Client;
pub use protocol::{Reply, ReplyItem, Request};
pub use server::{Searcher, CLASS, COMPAT_CLASS};
