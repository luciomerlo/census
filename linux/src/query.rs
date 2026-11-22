//! Consultas estilo Everything: terminos, `path:`, `ext:`, `size:`, fechas.
//!
//! PENDIENTE: parseo + matching paralelo sobre `compacto` + orden/paginacion.
//! El contrato de sintaxis es el mismo de Windows (`src/query.rs`).

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sort {
    #[default]
    NameAsc,
    NameDesc,
    PathAsc,
    PathDesc,
    SizeAsc,
    SizeDesc,
    DateAsc,
    DateDesc,
}

impl Sort {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "name" | "name-asc" | "asc" => Some(Sort::NameAsc),
            "name-desc" | "desc" => Some(Sort::NameDesc),
            "path" | "path-asc" => Some(Sort::PathAsc),
            "path-desc" => Some(Sort::PathDesc),
            "size" | "size-desc" | "largest" => Some(Sort::SizeDesc),
            "size-asc" | "smallest" => Some(Sort::SizeAsc),
            "date" | "date-desc" | "newest" => Some(Sort::DateDesc),
            "date-asc" | "oldest" => Some(Sort::DateAsc),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct Query {
    pub raw: String,
}

pub fn parse(raw: &str, _ix: &crate::index::Index) -> Query {
    Query {
        raw: raw.to_string(),
    }
}
