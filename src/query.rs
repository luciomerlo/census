//! Parseo y evaluación de las consultas que consumen las apps.

use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};

use crate::index::{contains_ignore_case, fold, to_units_lower, Index, F_DIR};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Sort {
    #[default]
    NameAsc,
    NameDesc,
}

impl Sort {
    pub fn from_proto(sort: u32) -> Self {
        match sort {
            2 => Sort::NameDesc,
            _ => Sort::NameAsc,
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "name" | "name-asc" | "asc" => Some(Sort::NameAsc),
            "name-desc" | "desc" => Some(Sort::NameDesc),
            _ => None,
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
    /// Términos por subcadena, ya plegados a minúsculas.
    pub terms: Vec<Vec<u16>>,
    /// Carpeta resuelta a id de nodo cuando la consulta trae `path:`.
    pub dir: Option<u32>,
    pub files_only: bool,
    pub folders_only: bool,
    pub exts: Vec<Vec<u16>>,
    pub min_mtime: Option<i64>,
    pub max_mtime: Option<i64>,
    pub size: Option<(Cmp, u64)>,
    /// Términos que no se pudieron interpretar (se ignoran, pero se informan).
    pub unknown: Vec<String>,
    /// `path:` que no resuelve a ninguna carpeta: no puede devolver nada.
    pub nonexistent_dir: Option<String>,
}

fn tokenize(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
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
    let s = s.trim().replace('/', "\\");
    let trimmed = s.trim_end_matches('\\');
    if trimmed.is_empty() {
        s
    } else if trimmed.len() == 2 && trimmed.ends_with(':') {
        format!("{trimmed}\\")
    } else {
        trimmed.to_string()
    }
}

fn local_date_bounds(date: NaiveDate) -> Option<(i64, i64)> {
    let start = Local
        .from_local_datetime(&date.and_hms_opt(0, 0, 0)?)
        .single()?
        .timestamp();
    Some((start, start + 86_400))
}

fn parse_local_stamp(s: &str) -> Option<i64> {
    let s = unquote(s.trim());
    const DT: [&str; 5] = [
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y/%m/%d %H:%M:%S",
    ];
    for f in DT {
        if let Ok(dt) = NaiveDateTime::parse_from_str(s, f) {
            return Local.from_local_datetime(&dt).single().map(|d| d.timestamp());
        }
    }
    for f in ["%Y-%m-%d", "%Y/%m/%d"] {
        if let Ok(d) = NaiveDate::parse_from_str(s, f) {
            return local_date_bounds(d).map(|(a, _)| a);
        }
    }
    None
}

/// Funciones de Everything que dejamos afuera a propósito. Se informan en vez
/// de convertirse en un término de búsqueda silencioso.
const UNSUPPORTED: [&str; 24] = [
    "content", "regex", "case", "wholeword", "wfn", "diacritics", "dc", "da", "dr", "attrib", "dupe", "empty",
    "parent", "child", "sibling", "ancestor", "len", "depth", "startwith", "endwith", "type", "run",
    "recentchange", "wildcards",
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
    let digits_end = rest.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(rest.len());
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

/// Interpreta la consulta contra un índice (para resolver `path:`).
pub fn parse(raw: &str, ix: &Index) -> Query {
    let mut q = Query {
        raw: raw.to_string(),
        ..Default::default()
    };
    let mut not_dir: Option<String> = None;
    for token in tokenize(raw) {
        let lower = token.to_ascii_lowercase();
        if lower.starts_with("path:") {
            let value = normalize_path(unquote(&token[5..]));
            match ix.find_dir(&value) {
                Some(id) => q.dir = Some(id),
                None => {
                    // Carpeta inexistente: la consulta no puede devolver nada.
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
                    q.exts.push(to_units_lower(e));
                }
            }
        } else if lower.starts_with("dm:") {
            let v = lower[3..].to_string();
            let now = Local::now().date_naive();
            let bounds = match v.as_str() {
                "today" => local_date_bounds(now),
                "yesterday" => now.pred_opt().and_then(local_date_bounds),
                _ => parse_local_stamp(&token[3..]).and_then(|t| {
                    let d = Local.timestamp_opt(t, 0).single()?;
                    local_date_bounds(d.date_naive())
                }),
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
            match parse_local_stamp(rest) {
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
            let _ = fun;
        } else {
            let term = unquote(&token);
            if !term.is_empty() {
                q.terms.push(to_units_lower(term));
            }
        }
    }
    q.nonexistent_dir = not_dir;
    q
}

impl Index {
    fn matches(&self, q: &Query, id: u32) -> bool {
        let node = &self.nodes[id as usize];
        let is_dir = node.flags & F_DIR != 0;

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
            if node.mtime < min {
                return false;
            }
        }
        if let Some(max) = q.max_mtime {
            if node.mtime >= max {
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
        let name = self.name_utf16(id);
        for t in &q.terms {
            if !contains_ignore_case(name, t) {
                return false;
            }
        }
        if !q.exts.is_empty() {
            // Extensión = lo que sigue al último punto del nombre.
            let mut dot = None;
            for (i, u) in name.iter().enumerate() {
                if *u == '.' as u16 {
                    dot = Some(i);
                }
            }
            let Some(dot) = dot else { return false };
            let ext: Vec<u16> = name[dot + 1..].iter().map(|u| fold(*u)).collect();
            if ext.is_empty() || !q.exts.iter().any(|e| *e == ext) {
                return false;
            }
        }
        true
    }

    /// Ids que cumplen la consulta, en orden de índice (sin ordenar).
    pub fn match_ids(&self, q: &Query) -> Vec<u32> {
        self.match_ids_capped(q, usize::MAX)
    }

    /// Igual que [`Index::match_ids`] pero corta en `cap`. La ventana usa un
    /// tope para no ordenar un millón y medio de filas en cada tecla.
    pub fn match_ids_capped(&self, q: &Query, cap: usize) -> Vec<u32> {
        let mut out = Vec::with_capacity(cap.min(4096));
        for id in 0..self.nodes.len() as u32 {
            if self.matches(q, id) {
                out.push(id);
                if out.len() >= cap {
                    break;
                }
            }
        }
        out
    }

    pub fn sort_ids(&self, ids: &mut [u32], sort: Sort) {
        let asc = sort == Sort::NameAsc;
        ids.sort_unstable_by(|&a, &b| {
            let na = self.name_utf16(a);
            let nb = self.name_utf16(b);
            let n = na.len().min(nb.len());
            for i in 0..n {
                let fa = fold(na[i]);
                let fb = fold(nb[i]);
                if fa != fb {
                    return if asc { fa.cmp(&fb) } else { fb.cmp(&fa) };
                }
            }
            let ord = na.len().cmp(&nb.len());
            if asc {
                ord
            } else {
                ord.reverse()
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::RawRecord;

    fn rec(frn: u64, parent: u64, name: &str, dir: bool, size: u64, mtime: i64) -> RawRecord {
        RawRecord {
            frn,
            parent_frn: parent,
            name: name.into(),
            size: if dir { u64::MAX } else { size },
            is_dir: dir,
            attrs: 0,
            mtime,
            atime: 0,
        }
    }

    fn ix() -> Index {
        let mut b = crate::index::Builder::with_capacity(16);
        b.push(rec(5, 5, ".", true, 0, 0));
        b.push(rec(10, 5, "Users", true, 0, 0));
        b.push(rec(11, 10, "Lucio", true, 0, 0));
        b.push(rec(12, 11, "informe.pdf", false, 4096, 1_700_000_000));
        b.push(rec(13, 11, "notas.txt", false, 10, 1_600_000_000));
        b.push(rec(14, 5, "Windows", true, 0, 0));
        b.finish('C')
    }

    fn names(ix: &Index, ids: &[u32]) -> Vec<String> {
        ids.iter().map(|&i| ix.name_string(i)).collect()
    }

    #[test]
    fn empty_query_matches_everything() {
        let ix = ix();
        let q = parse("", &ix);
        let ids = ix.match_ids(&q);
        assert_eq!(ids.len(), ix.len());
    }

    #[test]
    fn file_and_folder_filters() {
        let ix = ix();
        let files = ix.match_ids(&parse("file:", &ix));
        assert!(files.iter().all(|&i| !ix.is_dir(i)));
        assert_eq!(names(&ix, &files), vec!["informe.pdf", "notas.txt"]);

        let folders = ix.match_ids(&parse("folder:", &ix));
        assert!(folders.iter().all(|&i| ix.is_dir(i)));
    }

    #[test]
    fn path_filter() {
        let ix = ix();
        let ids = ix.match_ids(&parse("path:\"C:\\Users\\Lucio\"", &ix));
        assert_eq!(names(&ix, &ids), vec!["Lucio", "informe.pdf", "notas.txt"]);
    }

    #[test]
    fn path_filter_normalizes_slashes_and_case() {
        let ix = ix();
        let a = ix.match_ids(&parse("path:c:/users/lucio/", &ix));
        let b = ix.match_ids(&parse("path:\"C:\\Users\\Lucio\"", &ix));
        assert_eq!(a, b);
    }

    #[test]
    fn nonexistent_path_returns_nothing() {
        let ix = ix();
        let ids = ix.match_ids(&parse("path:\"C:\\NoExiste\"", &ix));
        assert!(ids.is_empty());
    }

    #[test]
    fn ext_and_size_filters() {
        let ix = ix();
        assert_eq!(names(&ix, &ix.match_ids(&parse("ext:txt", &ix))), vec!["notas.txt"]);
        assert_eq!(names(&ix, &ix.match_ids(&parse("ext:.PDF", &ix))), vec!["informe.pdf"]);
        assert_eq!(names(&ix, &ix.match_ids(&parse("size:>1kb", &ix))), vec!["informe.pdf"]);
        assert_eq!(names(&ix, &ix.match_ids(&parse("size:<1kb", &ix))), vec!["notas.txt"]);
    }

    #[test]
    fn dm_today_uses_local_midnight() {
        let ix = ix();
        // Un archivo con mtime de hoy tiene que entrar; uno viejo no.
        let now = Local::now().timestamp();
        let mut b = crate::index::Builder::with_capacity(4);
        b.push(rec(5, 5, ".", true, 0, 0));
        b.push(rec(9, 5, "hoy.txt", false, 1, now));
        b.push(rec(10, 5, "viejo.txt", false, 1, now - 400 * 86_400));
        let ix2 = b.finish('C');
        let ids = ix2.match_ids(&parse("file: dm:today", &ix2));
        assert_eq!(names(&ix2, &ids), vec!["hoy.txt"]);
        let _ = ix;
    }

    #[test]
    fn rc_ge_parses_the_stamp_wizeverything_writes() {
        let ix = ix();
        let q = parse("file: rc:>=2023-11-14T00:00:00", &ix);
        assert!(q.min_mtime.is_some());
        let ids = ix.match_ids(&q);
        // notas.txt es de 2020: queda afuera del rango.
        assert_eq!(names(&ix, &ids), vec!["informe.pdf"]);
    }

    #[test]
    fn unknown_functions_are_reported_not_matched() {
        let ix = ix();
        let q = parse("content:hola", &ix);
        assert_eq!(q.unknown, vec!["content (no soportada)"]);
        assert!(q.terms.is_empty());
    }

    #[test]
    fn quoted_terms_can_have_spaces() {
        let ix = ix();
        let q = parse("\"forme.p\"", &ix);
        assert_eq!(q.terms.len(), 1);
        assert_eq!(names(&ix, &ix.match_ids(&q)), vec!["informe.pdf"]);
    }

    #[test]
    fn sorts_by_name_in_both_directions() {
        let ix = ix();
        let mut ids = ix.match_ids(&parse("file:", &ix));
        ix.sort_ids(&mut ids, Sort::NameAsc);
        assert_eq!(names(&ix, &ids), vec!["informe.pdf", "notas.txt"]);
        ix.sort_ids(&mut ids, Sort::NameDesc);
        assert_eq!(names(&ix, &ids), vec!["notas.txt", "informe.pdf"]);
    }
}
