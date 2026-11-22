//! Consultas v1: terminos libres, `path:`, `ext:`, `size:>`/`size:<`, orden y
//! limite. Todas las palabras deben aparecer en el nombre o en la ruta plegada.

use crate::index::{fold_bytes, path_prefix_matches, Index};

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
            "mtime" | "date" | "date-desc" | "newest" => Some(Sort::DateDesc),
            "date-asc" | "oldest" => Some(Sort::DateAsc),
            _ => None,
        }
    }
}

/// Alias historico del contrato.
pub type Query = Spec;

#[derive(Clone, Debug, Default)]
pub struct Spec {
    pub raw: String,
    pub terms: Vec<Vec<u8>>,
    pub path: Option<String>,
    pub ext: Option<String>,
    pub min_size: Option<u64>,
    pub size_gt: Option<u64>,
    pub size_lt: Option<u64>,
    pub sort: Sort,
    pub limit: usize,
    pub files_only: bool,
}

impl Spec {
    pub fn new(raw: &str) -> Self {
        let mut s = Self {
            raw: raw.to_string(),
            limit: 500,
            sort: Sort::NameAsc,
            ..Default::default()
        };
        for token in raw.split_whitespace() {
            if let Some(v) = token.strip_prefix("path:") {
                s.path = Some(crate::index::normalize_path(v));
            } else if let Some(v) = token.strip_prefix("ext:") {
                s.ext = Some(v.trim_start_matches('.').to_ascii_lowercase());
            } else if let Some(v) = token.strip_prefix("size:") {
                if let Some(n) = v.strip_prefix('>') {
                    s.size_gt = n.parse().ok();
                } else if let Some(n) = v.strip_prefix('<') {
                    s.size_lt = n.parse().ok();
                }
            } else {
                s.terms.push(fold_bytes(token.as_bytes()));
            }
        }
        s
    }

    pub fn query_text(&self) -> String {
        self.raw.clone()
    }
}

fn matches_name(ix: &Index, id: u32, terms: &[Vec<u8>]) -> bool {
    let name = ix.name_folded(id);
    terms
        .iter()
        .all(|t| t.is_empty() || memchr::memmem::find(name, t).is_some())
}

fn matches_path(ix: &Index, id: u32, terms: &[Vec<u8>]) -> bool {
    let path = ix.full_path(id);
    let folded = fold_bytes(path.as_bytes());
    terms
        .iter()
        .all(|t| t.is_empty() || memchr::memmem::find(&folded, t).is_some())
}

fn matches(ix: &Index, id: u32, spec: &Spec) -> bool {
    if ix.is_root(id) {
        return false;
    }
    if spec.files_only && ix.is_dir(id) {
        return false;
    }
    if let Some(ext) = &spec.ext {
        let name = String::from_utf8_lossy(ix.name_bytes(id)).to_ascii_lowercase();
        if !name.ends_with(&format!(".{ext}")) {
            return false;
        }
    }
    let size = ix.node_size(id);
    if let Some(min) = spec.min_size {
        if size < min {
            return false;
        }
    }
    if let Some(n) = spec.size_gt {
        if size <= n {
            return false;
        }
    }
    if let Some(n) = spec.size_lt {
        if size >= n {
            return false;
        }
    }
    if spec.terms.is_empty() {
        return true;
    }
    matches_name(ix, id, &spec.terms) || matches_path(ix, id, &spec.terms)
}

pub fn search(ix: &Index, spec: &Spec) -> (usize, Vec<u32>) {
    let range: Vec<u32> = if let Some(prefix) = &spec.path {
        match ix.find_dir(prefix) {
            Some(root) => (root..ix.tout[root as usize]).collect(),
            None => Vec::new(),
        }
    } else {
        (0..ix.len() as u32).collect()
    };
    let mut hits: Vec<u32> = range
        .into_iter()
        .filter(|&id| {
            spec.path
                .as_ref()
                .map(|p| path_prefix_matches(&ix.full_path(id), p))
                .unwrap_or(true)
                && matches(ix, id, spec)
        })
        .collect();
    sort_hits(ix, &mut hits, spec.sort);
    let total = hits.len();
    if spec.limit > 0 && hits.len() > spec.limit {
        hits.truncate(spec.limit);
    }
    (total, hits)
}

fn sort_hits(ix: &Index, hits: &mut [u32], sort: Sort) {
    match sort {
        Sort::NameAsc => hits.sort_unstable_by(|&a, &b| {
            ix.name_folded(a)
                .cmp(ix.name_folded(b))
                .then_with(|| a.cmp(&b))
        }),
        Sort::NameDesc => hits.sort_unstable_by(|&a, &b| {
            ix.name_folded(b)
                .cmp(ix.name_folded(a))
                .then_with(|| b.cmp(&a))
        }),
        Sort::PathAsc => hits.sort_unstable_by(|&a, &b| {
            ix.full_path(a)
                .cmp(&ix.full_path(b))
                .then_with(|| a.cmp(&b))
        }),
        Sort::PathDesc => hits.sort_unstable_by(|&a, &b| {
            ix.full_path(b)
                .cmp(&ix.full_path(a))
                .then_with(|| b.cmp(&a))
        }),
        Sort::SizeAsc => hits.sort_unstable_by(|&a, &b| {
            ix.node_size(a)
                .cmp(&ix.node_size(b))
                .then_with(|| a.cmp(&b))
        }),
        Sort::SizeDesc => hits.sort_unstable_by(|&a, &b| {
            ix.node_size(b)
                .cmp(&ix.node_size(a))
                .then_with(|| b.cmp(&a))
        }),
        Sort::DateAsc => hits.sort_unstable_by(|&a, &b| {
            ix.nodes[a as usize]
                .mtime_ns
                .cmp(&ix.nodes[b as usize].mtime_ns)
                .then_with(|| a.cmp(&b))
        }),
        Sort::DateDesc => hits.sort_unstable_by(|&a, &b| {
            ix.nodes[b as usize]
                .mtime_ns
                .cmp(&ix.nodes[a as usize].mtime_ns)
                .then_with(|| b.cmp(&a))
        }),
    }
}

pub struct Hit {
    pub id: u32,
    pub path: String,
    pub name: String,
    pub size: u64,
    pub alloc: u64,
    pub mtime_ns: i64,
    pub volume_id: String,
}

pub fn hit(ix: &Index, id: u32) -> Hit {
    Hit {
        id,
        path: ix.full_path(id),
        name: ix.name_string(id),
        size: ix.node_size(id),
        alloc: ix.node_alloc(id),
        mtime_ns: ix.nodes[id as usize].mtime_ns,
        volume_id: ix
            .root_of(id)
            .map(|r| r.volume_id.clone())
            .unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Builder, F_DIR, F_ROOT};

    fn sample() -> Index {
        let mut b = Builder::with_capacity(8);
        let root = b.push(b"/", F_DIR | F_ROOT, 0, 0, 0, 0, 0, 0, 1, 1, 0o755);
        b.nodes[root as usize].parent = root;
        let docs = b.push(b"Docs", F_DIR, root, 0, 0, 0, 0, 0, 2, 1, 0o755);
        b.push(b"nota.txt", 0, docs, 10, 512, 1, 0, 0, 3, 1, 0o644);
        b.push(b"foto.JPG", 0, root, 100, 512, 2, 0, 0, 4, 1, 0o644);
        b.roots.push(crate::index::Root {
            id: root,
            volume_id: "v".into(),
            path: "/".into(),
            ..Default::default()
        });
        b.finish()
    }

    #[test]
    fn free_terms_match_name_or_path() {
        let ix = sample();
        let (n, hits) = search(&ix, &Spec::new("docs"));
        assert_eq!(n, 2);
        assert!(hits.iter().any(|&id| ix.name_bytes(id) == b"nota.txt"));
        assert_eq!(search(&ix, &Spec::new("foto")).0, 1);
    }

    #[test]
    fn ext_size_and_path_filters() {
        let ix = sample();
        let mut s = Spec::new("txt");
        s.ext = Some("jpg".into());
        assert_eq!(search(&ix, &s).0, 0);
        assert_eq!(search(&ix, &Spec::new("path:/Docs")).0, 2);
        let mut s = Spec::new("");
        s.min_size = Some(50);
        let (n, hits) = search(&ix, &s);
        assert_eq!(n, 1);
        assert_eq!(ix.name_bytes(hits[0]), b"foto.JPG");
    }
}
