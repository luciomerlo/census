//! Parseo y evaluación de las consultas que consumen las apps.

use crate::index::{fold, to_units_lower, Index, F_DIR};

/// Orden de los resultados. Los números coinciden con los del protocolo de
/// Everything (1..8), que es lo que piden las apps.
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
    /// 1 nombre↑, 2 nombre↓, 3 ruta↑, 4 ruta↓, 5 tamaño↑, 6 tamaño↓,
    /// 7 fecha↑, 8 fecha↓. Cualquier otro valor cae a nombre ascendente.
    pub fn from_proto(sort: u32) -> Self {
        match sort {
            2 => Sort::NameDesc,
            3 => Sort::PathAsc,
            4 => Sort::PathDesc,
            5 => Sort::SizeAsc,
            6 => Sort::SizeDesc,
            7 => Sort::DateAsc,
            8 => Sort::DateDesc,
            _ => Sort::NameAsc,
        }
    }

    pub fn to_proto(self) -> u32 {
        match self {
            Sort::NameAsc => 1,
            Sort::NameDesc => 2,
            Sort::PathAsc => 3,
            Sort::PathDesc => 4,
            Sort::SizeAsc => 5,
            Sort::SizeDesc => 6,
            Sort::DateAsc => 7,
            Sort::DateDesc => 8,
        }
    }

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

    /// Etiqueta corta para el botón de la ventana.
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

    /// Siguiente orden al hacer clic: nombre → tamaño → fecha → ruta → nombre.
    pub fn next(self) -> Self {
        match self {
            Sort::NameAsc => Sort::NameDesc,
            Sort::NameDesc => Sort::SizeDesc,
            Sort::SizeDesc => Sort::SizeAsc,
            Sort::SizeAsc => Sort::DateDesc,
            Sort::DateDesc => Sort::DateAsc,
            Sort::DateAsc => Sort::PathAsc,
            Sort::PathAsc => Sort::PathDesc,
            Sort::PathDesc => Sort::NameAsc,
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

/// Funciones de Everything que dejamos afuera a propósito. Se informan en vez
/// de convertirse en un término de búsqueda silencioso.
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

/// Interpreta la consulta contra un índice (para resolver `path:`).
/// Búsqueda de la ventana: **sólo fragmentos**, como busca Everything.
///
/// Cada palabra se busca como subcadena en el nombre del archivo o en alguna
/// carpeta de su ruta. No hay operadores: si alguien escribe `ext:pdf` o
/// `path:C:`, se busca ese texto tal cual. Los `*` y `?` se ignoran porque la
/// gente escribe `*.mp3` esperando los mp3, no un comodín.
pub fn parse_simple(raw: &str) -> Query {
    let mut q = Query {
        raw: raw.to_string(),
        ..Default::default()
    };
    for token in tokenize(raw) {
        let limpio: String = token.chars().filter(|c| *c != '*' && *c != '?').collect();
        let limpio = limpio.trim().trim_matches('"').trim();
        if limpio.is_empty() {
            continue;
        }
        q.terms.push(to_units_lower(limpio));
    }
    q
}

/// ¿El arena plegado contiene el término? Comparación directa, sin plegar.
fn contains_folded(haystack: &[u16], needle: &[u16]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > haystack.len() {
        return false;
    }
    let primero = needle[0];
    let ultimo = haystack.len() - needle.len();
    let mut i = 0usize;
    while i <= ultimo {
        // Salto rápido hasta el primer carácter del término.
        if haystack[i] != primero {
            match haystack[i + 1..=ultimo].iter().position(|u| *u == primero) {
                Some(salto) => {
                    i += salto + 1;
                    continue;
                }
                None => return false,
            }
        }
        if haystack[i..i + needle.len()] == *needle {
            return true;
        }
        i += 1;
    }
    false
}

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
            let v = lower.strip_prefix("dm:").unwrap_or("").to_string();
            let bounds = match v.as_str() {
                "today" => crate::time::today_bounds(),
                "yesterday" => crate::time::yesterday_bounds(),
                _ => crate::time::parse_local_stamp(&token[3..]).and_then(crate::time::day_bounds),
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
            let limpio = unquote(rest.trim());
            match crate::time::parse_local_stamp(limpio) {
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
    /// Filtros de la consulta que no dependen de los términos (tipo, ruta,
    /// fechas, tamaño, extensión).
    fn passes_filters(&self, q: &Query, id: u32) -> bool {
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
        if !q.exts.is_empty() {
            let name = self.name_folded(id);
            let mut dot = None;
            for (i, u) in name.iter().enumerate() {
                if *u == '.' as u16 {
                    dot = Some(i);
                }
            }
            let Some(dot) = dot else { return false };
            if dot + 1 >= name.len() || !q.exts.iter().any(|e| e == &name[dot + 1..]) {
                return false;
            }
        }
        true
    }

    /// ¿Todos los términos aparecen en el nombre? Compara sobre el arena
    /// plegado: no hay `fold()` en tiempo de consulta.
    fn name_has_all(&self, q: &Query, id: u32) -> bool {
        let name = self.name_folded(id);
        q.terms.iter().all(|t| contains_folded(name, t))
    }

    /// ¿Todos los términos aparecen, cada uno en el nombre o en alguna carpeta
    /// de la ruta? Un término puede estar en el nombre y otro en la carpeta.
    fn all_terms_match(&self, q: &Query, id: u32) -> bool {
        q.terms.iter().all(|t| self.term_matches(id, t))
    }

    /// ¿El término está en el nombre o en alguna carpeta ancestro?
    fn term_matches(&self, id: u32, term: &[u16]) -> bool {
        if contains_folded(self.name_folded(id), term) {
            return true;
        }
        let mut cur = id;
        for _ in 0..64 {
            let Some(p) = self.parent(cur) else { break };
            if p == cur {
                break;
            }
            if contains_folded(self.name_folded(p), term) {
                return true;
            }
            cur = p;
        }
        false
    }

    /// Recorre el índice en paralelo y devuelve los ids que cumplen `f`, en
    /// orden de índice. Trocear por bloques contiguos mantiene el orden.
    fn scan<F>(&self, f: F) -> Vec<u32>
    where
        F: Fn(u32) -> bool + Sync,
    {
        let total = self.nodes.len();
        if total == 0 {
            return Vec::new();
        }
        let hilos = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(4)
            .clamp(1, 16);
        let bloque = total.div_ceil(hilos);
        let mut partes: Vec<Vec<u32>> = Vec::with_capacity(hilos);
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(hilos);
            for k in 0..hilos {
                let ini = (k * bloque) as u32;
                let fin = (((k + 1) * bloque) as u32).min(total as u32);
                if ini >= fin {
                    break;
                }
                let f = &f;
                handles.push(s.spawn(move || {
                    let mut out = Vec::new();
                    for id in ini..fin {
                        if f(id) {
                            out.push(id);
                        }
                    }
                    out
                }));
            }
            for h in handles {
                if let Ok(v) = h.join() {
                    partes.push(v);
                }
            }
        });
        let mut out: Vec<u32> = Vec::with_capacity(partes.iter().map(Vec::len).sum());
        for p in partes {
            out.extend(p);
        }
        out
    }

    /// Resultados de una consulta, con los que coinciden por **nombre** al
    /// principio.
    ///
    /// Dos fases, como Everything: primero el nombre (barato); la carpeta sólo
    /// se mira si con el nombre no se llenó el tope. Ambas fases escanean en
    /// paralelo y comparan sobre el arena plegado.
    pub fn match_ids_split(&self, q: &Query, cap: usize) -> (Vec<u32>, usize) {
        let mut ids = self.scan(|id| self.passes_filters(q, id) && self.name_has_all(q, id));
        let cuantos = ids.len();
        if ids.len() < cap {
            let mut por_carpeta = self.scan(|id| {
                self.passes_filters(q, id)
                    && !self.name_has_all(q, id)
                    && self.all_terms_match(q, id)
            });
            ids.append(&mut por_carpeta);
        }
        ids.truncate(cap);
        (ids, cuantos.min(cap))
    }

    /// ¿Este id cumple la consulta? (filtros + todos los términos)
    pub fn matches_query(&self, q: &Query, id: u32) -> bool {
        self.passes_filters(q, id) && self.all_terms_match(q, id)
    }

    /// Filtra en el lugar una lista ya calculada. Lo usa la ventana para
    /// angostar resultados al agregar letras, en vez de reescanear el índice.
    pub fn keep_matching(&self, q: &Query, ids: &mut Vec<u32>) {
        ids.retain(|&id| self.matches_query(q, id));
    }

    /// Ids que cumplen la consulta, en orden de índice (sin ordenar).
    pub fn match_ids(&self, q: &Query) -> Vec<u32> {
        self.match_ids_capped(q, usize::MAX)
    }

    /// Igual que [`Index::match_ids`] pero corta en `cap`.
    pub fn match_ids_capped(&self, q: &Query, cap: usize) -> Vec<u32> {
        self.match_ids_split(q, cap).0
    }

    /// Compara dos nombres sin distinguir mayúsculas ni acentos.
    fn cmp_name(&self, a: u32, b: u32) -> std::cmp::Ordering {
        let na = self.name_utf16(a);
        let nb = self.name_utf16(b);
        let n = na.len().min(nb.len());
        for i in 0..n {
            let fa = fold(na[i]);
            let fb = fold(nb[i]);
            if fa != fb {
                return fa.cmp(&fb);
            }
        }
        na.len().cmp(&nb.len())
    }

    /// Ordena los ids según `sort`.
    ///
    /// Para tamaño, las carpetas (que no tienen tamaño propio) van siempre al
    /// final: en WizTree el árbol de carpetas se lee aparte del de archivos.
    pub fn sort_ids(&self, ids: &mut [u32], sort: Sort) {
        use std::cmp::Ordering;
        let size_of = |id: u32| -> Option<u64> {
            let n = &self.nodes[id as usize];
            if n.flags & F_DIR != 0 {
                None
            } else {
                Some(n.size)
            }
        };
        match sort {
            // Con los rangos precalculados, ordenar por nombre es comparar dos
            // enteros: no se toca la arena.
            Sort::NameAsc if !self.rango.is_empty() => {
                ids.sort_unstable_by_key(|&id| self.rank_of(id))
            }
            Sort::NameDesc if !self.rango.is_empty() => {
                ids.sort_unstable_by_key(|&id| std::cmp::Reverse(self.rank_of(id)))
            }
            Sort::NameAsc => ids.sort_unstable_by(|&a, &b| self.cmp_name(a, b)),
            Sort::NameDesc => ids.sort_unstable_by(|&a, &b| self.cmp_name(b, a)),
            Sort::PathAsc => ids.sort_unstable_by(|&a, &b| {
                self.nodes[a as usize]
                    .parent
                    .cmp(&self.nodes[b as usize].parent)
                    .then_with(|| self.cmp_name(a, b))
            }),
            Sort::PathDesc => ids.sort_unstable_by(|&a, &b| {
                self.nodes[b as usize]
                    .parent
                    .cmp(&self.nodes[a as usize].parent)
                    .then_with(|| self.cmp_name(b, a))
            }),
            Sort::SizeDesc => ids.sort_unstable_by(|&a, &b| match (size_of(a), size_of(b)) {
                (Some(x), Some(y)) => y.cmp(&x).then_with(|| self.cmp_name(a, b)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => self.cmp_name(a, b),
            }),
            Sort::SizeAsc => ids.sort_unstable_by(|&a, &b| match (size_of(a), size_of(b)) {
                (Some(x), Some(y)) => x.cmp(&y).then_with(|| self.cmp_name(a, b)),
                (Some(_), None) => Ordering::Less,
                (None, Some(_)) => Ordering::Greater,
                (None, None) => self.cmp_name(a, b),
            }),
            Sort::DateDesc => ids.sort_unstable_by(|&a, &b| {
                self.nodes[b as usize]
                    .mtime
                    .cmp(&self.nodes[a as usize].mtime)
                    .then_with(|| self.cmp_name(a, b))
            }),
            Sort::DateAsc => ids.sort_unstable_by(|&a, &b| {
                self.nodes[a as usize]
                    .mtime
                    .cmp(&self.nodes[b as usize].mtime)
                    .then_with(|| self.cmp_name(a, b))
            }),
        }
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
        assert_eq!(
            names(&ix, &ix.match_ids(&parse("ext:txt", &ix))),
            vec!["notas.txt"]
        );
        assert_eq!(
            names(&ix, &ix.match_ids(&parse("ext:.PDF", &ix))),
            vec!["informe.pdf"]
        );
        assert_eq!(
            names(&ix, &ix.match_ids(&parse("size:>1kb", &ix))),
            vec!["informe.pdf"]
        );
        assert_eq!(
            names(&ix, &ix.match_ids(&parse("size:<1kb", &ix))),
            vec!["notas.txt"]
        );
    }

    #[test]
    fn dm_today_uses_local_midnight() {
        let ix = ix();
        // Un archivo con mtime de hoy tiene que entrar; uno viejo no.
        let now = crate::time::now();
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

    #[test]
    fn a_plain_term_also_matches_the_folder() {
        let ix = ix();
        // "lucio" no está en ningún nombre de archivo: está en la carpeta.
        let q = parse("lucio", &ix);
        let encontrados = names(&ix, &ix.match_ids(&q));
        assert!(
            encontrados.contains(&"informe.pdf".to_string()),
            "{encontrados:?}"
        );
        assert!(encontrados.contains(&"notas.txt".to_string()));
        // Y también aparece la propia carpeta que coincide.
        assert!(encontrados.contains(&"Lucio".to_string()));
    }

    #[test]
    fn a_plain_term_matches_the_extension() {
        let ix = ix();
        let q = parse("pdf", &ix);
        let encontrados = names(&ix, &ix.match_ids(&q));
        assert_eq!(encontrados, vec!["informe.pdf".to_string()]);
    }

    #[test]
    fn fragments_are_substrings_and_nothing_else() {
        let ix = ix();
        // Sin comodines ni regex: "*" es un carácter literal y no matchea nada.
        assert!(ix.match_ids(&parse("*", &ix)).is_empty());
        assert!(ix.match_ids(&parse("informe.p", &ix)).len() == 1);
        assert!(ix.match_ids(&parse("INFORME", &ix)).len() == 1);
        // Un pedazo en el medio del nombre también sirve.
        assert!(ix.match_ids(&parse("form", &ix)).len() == 1);
    }

    #[test]
    fn several_fragments_must_all_match() {
        let ix = ix();
        // "lucio" está en la carpeta y "txt" en el nombre: la combinación filtra.
        let q = parse("lucio txt", &ix);
        let encontrados = names(&ix, &ix.match_ids(&q));
        assert_eq!(encontrados, vec!["notas.txt".to_string()]);
        assert!(ix.match_ids(&parse("lucio pdf", &ix)).len() == 1);
        assert!(ix.match_ids(&parse("lucio nada", &ix)).is_empty());
    }
}
