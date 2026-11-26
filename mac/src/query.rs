//! Consultas estilo Everything y plegado normativo.
//!
//! Plegado (contrato §6 ter): ASCII `A-Z` y Latin-1 `U+00C0..=U+00DE` menos
//! `U+00D7`, +0x20. Nada más: no se quitan acentos, no se tocan ligaduras y no
//! se usa `to_lowercase()` de Unicode. En nombres que no son UTF-8 se pliega
//! sólo ASCII.

use crate::index::Index;
use crate::time;
use crate::types::FLAG_DIR;

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

    pub fn label(self) -> &'static str {
        match self {
            Sort::NameAsc => "Nombre A-Z",
            Sort::NameDesc => "Nombre Z-A",
            Sort::PathAsc => "Ruta A-Z",
            Sort::PathDesc => "Ruta Z-A",
            Sort::SizeAsc => "Tamaño ↑",
            Sort::SizeDesc => "Tamaño ↓",
            Sort::DateAsc => "Fecha ↑",
            Sort::DateDesc => "Fecha ↓",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmp {
    Lt,
    Le,
    Gt,
    Ge,
    Eq,
}

#[derive(Debug, Clone, Default)]
pub struct Query {
    pub raw: String,
    pub terms: Vec<Vec<u8>>,
    pub dir: Option<u32>,
    pub files_only: bool,
    pub folders_only: bool,
    pub exts: Vec<Vec<u8>>,
    pub min_mtime: Option<i64>,
    pub max_mtime: Option<i64>,
    pub size: Option<(Cmp, u64)>,
    pub unknown: Vec<String>,
    pub nonexistent_dir: Option<String>,
}

/// Pliega un escalar. Debe coincidir con `fold()` de Windows.
#[inline]
pub fn fold_char(c: char) -> char {
    match c {
        'A'..='Z' => char::from_u32(c as u32 + 32).unwrap_or(c),
        '\u{00C0}'..='\u{00DE}' if c != '\u{00D7}' => char::from_u32(c as u32 + 32).unwrap_or(c),
        _ => c,
    }
}

/// Plegado de un nombre en UTF-8. Si hay bytes inválidos, sólo se pliega
/// ASCII y el resto queda intacto: no se pierde nada.
pub fn fold_bytes(name: &[u8]) -> Vec<u8> {
    match std::str::from_utf8(name) {
        Ok(s) => {
            let mut out = String::with_capacity(s.len());
            for c in s.chars() {
                out.push(fold_char(c));
            }
            out.into_bytes()
        }
        Err(_) => name
            .iter()
            .map(|b| if b.is_ascii_uppercase() { b + 32 } else { *b })
            .collect(),
    }
}

pub fn fold_str(s: &str) -> Vec<u8> {
    fold_bytes(s.as_bytes())
}

pub fn contains_folded(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    memchr::memmem::find(hay, needle).is_some()
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut in_q = false;
    for c in s.chars() {
        match c {
            '"' => {
                in_q = !in_q;
                cur.push(c);
            }
            c if c.is_whitespace() && !in_q => {
                if !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                }
            }
            c => cur.push(c),
        }
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

fn unquote(s: &str) -> &str {
    let s = s.trim();
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

fn normalize_path(s: &str) -> String {
    let s = unquote(s.trim());
    if let Some(rest) = s.strip_prefix('~') {
        if let Some(home) = std::env::var_os("HOME") {
            return format!("{}{}", home.to_string_lossy(), rest);
        }
    }
    crate::index::normalize_dir(s)
}

const UNSUPPORTED: [&str; 24] = [
    "content",
    "regex",
    "case",
    "wholeword",
    "wfn",
    "diacritics",
    "dc",
    "da",
    "dr",
    "attrib",
    "dupe",
    "empty",
    "parent",
    "child",
    "sibling",
    "ancestor",
    "len",
    "depth",
    "startwith",
    "endwith",
    "type",
    "run",
    "recentchange",
    "wildcards",
];

fn unknown_function(token_lower: &str) -> Option<&'static str> {
    let (prefix, _) = token_lower.split_once(':')?;
    UNSUPPORTED.iter().copied().find(|f| *f == prefix)
}

fn parse_size(s: &str) -> Option<(Cmp, u64)> {
    let s = unquote(s.trim());
    let (cmp, rest) = if let Some(r) = s.strip_prefix(">=") {
        (Cmp::Ge, r)
    } else if let Some(r) = s.strip_prefix("<=") {
        (Cmp::Le, r)
    } else if let Some(r) = s.strip_prefix('>') {
        (Cmp::Gt, r)
    } else if let Some(r) = s.strip_prefix('<') {
        (Cmp::Lt, r)
    } else {
        (Cmp::Eq, s)
    };
    let rest = rest.trim();
    let digits_end = rest
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(rest.len());
    let number: f64 = rest[..digits_end].parse().ok()?;
    let unit = rest[digits_end..].trim().to_ascii_lowercase();
    let mult: f64 = match unit.as_str() {
        "" | "b" => 1.0,
        "kb" => 1024.0,
        "mb" => 1024.0 * 1024.0,
        "gb" => 1024.0 * 1024.0 * 1024.0,
        "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((cmp, (number * mult) as u64))
}

/// Búsqueda simple de la ventana: cada palabra es una subcadena del nombre.
pub fn parse_simple(raw: &str) -> Query {
    let mut q = Query {
        raw: raw.to_string(),
        ..Default::default()
    };
    for token in tokenize(raw) {
        let limpio: String = token.chars().filter(|c| *c != '*' && *c != '?').collect();
        let limpio = limpio.trim().trim_matches('"').trim();
        if !limpio.is_empty() {
            q.terms.push(fold_str(limpio));
        }
    }
    q
}

/// Consulta completa con la sintaxis de CensusIndex.
pub fn parse(raw: &str, ix: &Index) -> Query {
    let mut q = Query {
        raw: raw.to_string(),
        ..Default::default()
    };
    let mut not_dir: Option<String> = None;
    for token in tokenize(raw) {
        let lower = token.to_ascii_lowercase();
        if lower.starts_with("path:") {
            let value = normalize_path(&token[5..]);
            match ix.find_dir(&value) {
                Some(id) => q.dir = Some(id),
                None => {
                    q.dir = None;
                    not_dir = Some(value);
                }
            }
        } else if lower == "file:" || lower == "files:" {
            q.files_only = true;
        } else if lower == "folder:" || lower == "folders:" {
            q.folders_only = true;
        } else if lower.starts_with("ext:") {
            for e in token[4..].split(',') {
                let e = e.trim().trim_start_matches('.');
                if !e.is_empty() {
                    q.exts.push(fold_str(e));
                }
            }
        } else if lower.starts_with("dm:") {
            let v = lower.strip_prefix("dm:").unwrap_or("").to_string();
            let bounds = match v.as_str() {
                "today" | "hoy" => Some(time::today_bounds()),
                "yesterday" | "ayer" => Some(time::yesterday_bounds()),
                _ => time::parse_range(&token[3..]),
            };
            if let Some((a, b)) = bounds {
                q.min_mtime = Some(q.min_mtime.map_or(a, |x| x.max(a)));
                q.max_mtime = Some(q.max_mtime.map_or(b, |x| x.min(b)));
            } else {
                q.unknown.push(token.clone());
            }
        } else if lower.starts_with("rc:") {
            let body = &token[3..];
            let (cmp, rest) = if let Some(r) = body.strip_prefix(">=") {
                (Cmp::Ge, r)
            } else if let Some(r) = body.strip_prefix("<=") {
                (Cmp::Le, r)
            } else if let Some(r) = body.strip_prefix('>') {
                (Cmp::Gt, r)
            } else if let Some(r) = body.strip_prefix('<') {
                (Cmp::Lt, r)
            } else {
                (Cmp::Ge, body)
            };
            match time::parse_when(unquote(rest.trim())) {
                Some(t) => match cmp {
                    Cmp::Ge => q.min_mtime = Some(q.min_mtime.map_or(t, |x| x.max(t))),
                    Cmp::Gt => q.min_mtime = Some(q.min_mtime.map_or(t + 1, |x| x.max(t + 1))),
                    Cmp::Le => q.max_mtime = Some(q.max_mtime.map_or(t + 1, |x| x.min(t + 1))),
                    Cmp::Lt => q.max_mtime = Some(q.max_mtime.map_or(t, |x| x.min(t))),
                    Cmp::Eq => {
                        q.min_mtime = Some(t);
                        q.max_mtime = Some(t + 1);
                    }
                },
                None => q.unknown.push(token.clone()),
            }
        } else if lower.starts_with("size:") {
            match parse_size(&token[5..]) {
                Some(v) => q.size = Some(v),
                None => q.unknown.push(token.clone()),
            }
        } else if let Some(fun) = unknown_function(&lower) {
            q.unknown.push(format!("{fun} (no soportada)"));
        } else {
            let term = unquote(&token);
            if !term.is_empty() {
                q.terms.push(fold_str(term));
            }
        }
    }
    q.nonexistent_dir = not_dir;
    q
}

impl Index {
    fn passes_filters(&self, q: &Query, id: u32) -> bool {
        let node = &self.nodes[id as usize];
        let is_dir = node.flags & FLAG_DIR != 0;
        if q.files_only && is_dir {
            return false;
        }
        if q.folders_only && !is_dir {
            return false;
        }
        if q.nonexistent_dir.is_some() {
            return false;
        }
        if let Some(dir) = q.dir {
            if !self.is_under(dir, id) {
                return false;
            }
        }
        if let Some(min) = q.min_mtime {
            if node.mtime_ns < min {
                return false;
            }
        }
        if let Some(max) = q.max_mtime {
            if node.mtime_ns >= max {
                return false;
            }
        }
        if let Some((cmp, want)) = q.size {
            if is_dir {
                return false;
            }
            let hit = match cmp {
                Cmp::Lt => node.size < want,
                Cmp::Le => node.size <= want,
                Cmp::Gt => node.size > want,
                Cmp::Ge => node.size >= want,
                Cmp::Eq => node.size == want,
            };
            if !hit {
                return false;
            }
        }
        if !q.exts.is_empty() {
            let name = self.folded_name(id);
            let dot = name.iter().rposition(|&b| b == b'.');
            let Some(dot) = dot else {
                return false;
            };
            if dot + 1 >= name.len() {
                return false;
            }
            let ext = &name[dot + 1..];
            if !q.exts.iter().any(|e| e == ext) {
                return false;
            }
        }
        true
    }

    fn matches_terms(&self, q: &Query, id: u32) -> bool {
        if q.terms.is_empty() {
            return true;
        }
        let c = self.compacto[id as usize];
        let name = &self.folded[c.off as usize..c.off as usize + c.len as usize];
        q.terms.iter().all(|t| contains_folded(name, t))
    }

    /// Ids que cumplen la consulta, en el orden pedido y paginados.
    pub fn search(&self, q: &Query, sort: Sort, offset: usize, max: usize) -> Vec<u32> {
        let mut out: Vec<u32> = Vec::new();
        let limite = offset.saturating_add(max);
        if sort == Sort::NameAsc {
            for &id in &self.por_nombre {
                if self.matches_terms(q, id) && self.passes_filters(q, id) {
                    out.push(id);
                    if out.len() >= limite {
                        break;
                    }
                }
            }
        } else if let Some(dir) = q.dir {
            let a = self.tin[dir as usize] as usize;
            let b = self.tout[dir as usize] as usize;
            for &id in &self.euler[a..b] {
                if self.matches_terms(q, id) && self.passes_filters(q, id) {
                    out.push(id);
                }
            }
        } else {
            for id in 0..self.nodes.len() as u32 {
                if self.matches_terms(q, id) && self.passes_filters(q, id) {
                    out.push(id);
                }
            }
        }
        out.sort_by(|&a, &b| self.compare(sort, a, b));
        out.into_iter().skip(offset).take(max).collect()
    }

    pub fn count(&self, q: &Query) -> usize {
        let mut n = 0usize;
        if let Some(dir) = q.dir {
            let a = self.tin[dir as usize] as usize;
            let b = self.tout[dir as usize] as usize;
            for &id in &self.euler[a..b] {
                if self.matches_terms(q, id) && self.passes_filters(q, id) {
                    n += 1;
                }
            }
        } else {
            for id in 0..self.nodes.len() as u32 {
                if self.matches_terms(q, id) && self.passes_filters(q, id) {
                    n += 1;
                }
            }
        }
        n
    }

    fn compare(&self, sort: Sort, a: u32, b: u32) -> std::cmp::Ordering {
        match sort {
            Sort::NameAsc => self.folded_name(a).cmp(self.folded_name(b)).then(a.cmp(&b)),
            Sort::NameDesc => self.folded_name(b).cmp(self.folded_name(a)).then(b.cmp(&a)),
            Sort::PathAsc => self.path_of(a).cmp(&self.path_of(b)),
            Sort::PathDesc => self.path_of(b).cmp(&self.path_of(a)),
            Sort::SizeAsc => self.nodes[a as usize]
                .size
                .cmp(&self.nodes[b as usize].size)
                .then(a.cmp(&b)),
            Sort::SizeDesc => self.nodes[b as usize]
                .size
                .cmp(&self.nodes[a as usize].size)
                .then(b.cmp(&a)),
            Sort::DateAsc => self.nodes[a as usize]
                .mtime_ns
                .cmp(&self.nodes[b as usize].mtime_ns)
                .then(a.cmp(&b)),
            Sort::DateDesc => self.nodes[b as usize]
                .mtime_ns
                .cmp(&self.nodes[a as usize].mtime_ns)
                .then(b.cmp(&a)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fold_is_the_windows_table() {
        assert_eq!(fold_str("ABC"), b"abc");
        assert_eq!(fold_str("ÑANDÚ"), "ñandú".as_bytes());
        assert_eq!(fold_str("Á"), "á".as_bytes());
        assert_eq!(fold_str("×"), "×".as_bytes());
        // No se quitan acentos ni se tocan ligaduras.
        assert_eq!(fold_str("café"), "café".as_bytes());
        assert_eq!(fold_str("Æ"), "æ".as_bytes());
        // La tabla pliega la caja de Æ, pero no la expande a "ae".
        assert_ne!(fold_str("Æ"), b"ae");
    }

    #[test]
    fn invalid_utf8_only_folds_ascii() {
        assert_eq!(fold_bytes(&[b'A', 0xff, b'B']), vec![b'a', 0xff, b'b']);
    }

    #[test]
    fn size_parser() {
        assert_eq!(parse_size("10MB"), Some((Cmp::Eq, 10 * 1024 * 1024)));
        assert_eq!(
            parse_size(">=1.5GB"),
            Some((Cmp::Ge, 3 * 1024 * 1024 * 1024 / 2))
        );
        assert_eq!(parse_size("nada"), None);
    }

    #[test]
    fn tokenizer_keeps_quotes() {
        assert_eq!(tokenize("a \"b c\" d"), vec!["a", "\"b c\"", "d"]);
    }
}
