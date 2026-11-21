//! Índice en memoria: nodos planos + arena de nombres UTF-16.

use std::collections::HashMap;

use crate::source::RawRecord;

pub const F_DIR: u16 = 0x1;
pub const F_ROOT: u16 = 0x2;

/// 48 bytes por nodo. El nombre vive en la arena, no acá.
#[derive(Clone, Copy, Debug, Default)]
pub struct Node {
    pub name_off: u32,
    pub name_len: u16,
    pub flags: u16,
    pub parent: u32,
    pub size: u64,
    pub mtime: i64,
    pub atime: i64,
    pub attrs: u32,
    pub frn: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Stats {
    pub records: u64,
    pub files: u64,
    pub dirs: u64,
    pub skipped: u64,
    /// Unidades UTF-16 ocupadas por los nombres.
    pub name_units: u64,
    pub build_ms: u128,
}

#[derive(Debug, Default)]
pub struct Index {
    /// Letra de la primera unidad (compatibilidad); ver `roots`.
    pub letter: char,
    pub root: u32,
    /// Un nodo raiz por unidad indexada (`C:`, `D:`, ...).
    pub roots: Vec<u32>,
    pub nodes: Vec<Node>,
    pub names: Vec<u16>,
    pub stats: Stats,
    pub built_at: i64,
}

pub struct Builder {
    nodes: Vec<Node>,
    names: Vec<u16>,
    parent_frns: Vec<u64>,
}

impl Builder {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            nodes: Vec::with_capacity(n),
            // ~24 unidades por nombre es un promedio razonable para reservar.
            names: Vec::with_capacity(n * 24),
            parent_frns: Vec::with_capacity(n),
        }
    }

    pub fn push(&mut self, r: RawRecord) -> u32 {
        let id = self.nodes.len() as u32;
        let name_off = self.names.len() as u32;
        self.names.extend(r.name.encode_utf16());
        let name_len = (self.names.len() as u32 - name_off) as u16;
        self.nodes.push(Node {
            name_off,
            name_len,
            flags: if r.is_dir { F_DIR } else { 0 },
            parent: u32::MAX,
            size: r.size,
            mtime: r.mtime,
            atime: r.atime,
            attrs: r.attrs,
            frn: r.frn,
        });
        self.parent_frns.push(r.parent_frn);
        id
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn finish(mut self, letter: char) -> Index {
        let mut map: HashMap<u64, u32> = HashMap::with_capacity(self.nodes.len());
        for (i, n) in self.nodes.iter().enumerate() {
            map.insert(n.frn, i as u32);
        }

        // El registro 5 es la raíz del volumen. Si no vino, se crea.
        let root = match map.get(&5) {
            Some(&i) => i,
            None => {
                let off = self.names.len() as u32;
                self.names
                    .extend(format!("{}:", letter.to_ascii_uppercase()).encode_utf16());
                self.nodes.push(Node {
                    name_off: off,
                    name_len: 2,
                    flags: F_DIR | F_ROOT,
                    parent: 0,
                    size: u64::MAX,
                    ..Default::default()
                });
                (self.nodes.len() - 1) as u32
            }
        };

        // La raíz se muestra como unidad: nombre "C:", sin padre.
        let root_name_off = self.names.len() as u32;
        self.names
            .extend(format!("{}:", letter.to_ascii_uppercase()).encode_utf16());
        self.nodes[root as usize].name_off = root_name_off;
        self.nodes[root as usize].name_len = 2;
        self.nodes[root as usize].flags |= F_DIR | F_ROOT;
        self.nodes[root as usize].parent = root;
        self.nodes[root as usize].size = u64::MAX;

        for i in 0..self.nodes.len() {
            if i as u32 == root {
                continue;
            }
            let pfrn = self.parent_frns[i];
            let parent = map.get(&pfrn).copied().unwrap_or(root);
            let parent = if parent as usize == i { root } else { parent };
            self.nodes[i].parent = parent;
        }

        let mut stats = Stats::default();
        for n in &self.nodes {
            if n.flags & F_DIR != 0 {
                stats.dirs += 1;
            } else {
                stats.files += 1;
            }
        }
        stats.name_units = self.names.len() as u64;

        Index {
            letter: letter.to_ascii_uppercase(),
            root,
            roots: vec![root],
            nodes: self.nodes,
            names: self.names,
            stats,
            built_at: 0,
        }
    }
}

const MAX_DEPTH: usize = 256;

impl Index {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Nombre en UTF-16. Un id fuera de rango o un offset corrupto devuelven
    /// vacío en vez de paniquear: el índice también se carga de disco.
    pub fn name_utf16(&self, id: u32) -> &[u16] {
        let Some(n) = self.nodes.get(id as usize) else {
            return &[];
        };
        let off = n.name_off as usize;
        let end = off.saturating_add(n.name_len as usize);
        self.names.get(off..end).unwrap_or(&[])
    }

    pub fn name_string(&self, id: u32) -> String {
        String::from_utf16_lossy(self.name_utf16(id))
    }

    pub fn is_dir(&self, id: u32) -> bool {
        self.nodes
            .get(id as usize)
            .is_some_and(|n| n.flags & F_DIR != 0)
    }

    pub fn is_drive(&self, id: u32) -> bool {
        self.nodes
            .get(id as usize)
            .is_some_and(|n| n.flags & F_ROOT != 0)
    }

    /// Padre de un nodo, si el id existe.
    pub fn parent(&self, id: u32) -> Option<u32> {
        self.nodes.get(id as usize).map(|n| n.parent)
    }

    /// Carpeta contenedora. Vacía para la raíz y para ids inválidos.
    pub fn dir_path(&self, id: u32) -> String {
        if self.is_drive(id) {
            return String::new();
        }
        let mut chain: Vec<u32> = Vec::with_capacity(8);
        let Some(mut cur) = self.parent(id) else {
            return String::new();
        };
        let mut guard = 0;
        while !self.is_drive(cur) && guard < MAX_DEPTH {
            chain.push(cur);
            let Some(p) = self.parent(cur) else { break };
            cur = p;
            guard += 1;
        }
        let mut s = self.name_string(cur);
        for &p in chain.iter().rev() {
            s.push('\\');
            s.push_str(&self.name_string(p));
        }
        s
    }

    /// Ruta completa.
    pub fn full_path(&self, id: u32) -> String {
        if self.is_drive(id) {
            return self.name_string(id);
        }
        let dir = self.dir_path(id);
        format!("{dir}\\{}", self.name_string(id))
    }

    /// Letra de la unidad que representa una raiz ("C:" -> 'C').
    pub fn letter_of(&self, root: u32) -> char {
        self.name_string(root).chars().next().unwrap_or('C')
    }

    /// Raiz de la unidad `C:` (o la que sea).
    pub fn root_for(&self, letter: char) -> Option<u32> {
        let want = format!("{}:", letter.to_ascii_uppercase());
        self.roots
            .iter()
            .copied()
            .find(|&r| eq_ignore_case(self.name_utf16(r), &want))
    }

    /// Resuelve `C:` a un id de nodo, bajando desde la raiz de esa unidad.
    pub fn find_dir(&self, path: &str) -> Option<u32> {
        let trimmed = path.trim();
        let mut chars = trimmed.chars();
        let letter = chars.next()?;
        if !letter.is_ascii_alphabetic() {
            return None;
        }
        let after = chars.as_str();
        if !after.starts_with(':') {
            return None;
        }
        let mut cur = self.root_for(letter)?;
        for part in after[1..]
            .split([std::path::MAIN_SEPARATOR, '/'])
            .filter(|p| !p.is_empty())
        {
            let mut found = None;
            for i in 0..self.nodes.len() as u32 {
                if self.nodes[i as usize].parent == cur && eq_ignore_case(self.name_utf16(i), part)
                {
                    found = Some(i);
                    break;
                }
            }
            cur = found?;
        }
        Some(cur)
    }

    /// Une varios volumenes en un solo indice. Los FRN son por volumen, asi que
    /// cada uno entra con sus parents ya resueltos.
    pub fn merge(vols: Vec<Index>) -> Index {
        let mut with_data: Vec<Index> = vols.into_iter().filter(|v| !v.nodes.is_empty()).collect();
        if with_data.len() == 1 {
            return with_data.remove(0);
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut names: Vec<u16> = Vec::new();
        let mut roots: Vec<u32> = Vec::new();
        let mut stats = Stats::default();
        let mut letter = '\0';
        let mut built_at = 0i64;
        for ix in with_data {
            if letter == '\0' {
                letter = ix.letter;
            }
            built_at = built_at.max(ix.built_at);
            let node_base = nodes.len() as u32;
            let name_base = names.len() as u32;
            roots.push(node_base + ix.root);
            stats.records += ix.stats.records;
            stats.files += ix.stats.files;
            stats.dirs += ix.stats.dirs;
            stats.skipped += ix.stats.skipped;
            stats.name_units += ix.stats.name_units;
            stats.build_ms += ix.stats.build_ms;
            names.extend_from_slice(&ix.names);
            for mut n in ix.nodes {
                n.name_off += name_base;
                n.parent += node_base;
                nodes.push(n);
            }
        }
        let root = roots.first().copied().unwrap_or(0);
        Index {
            letter,
            root,
            roots,
            nodes,
            names,
            stats,
            built_at,
        }
    }

    pub fn is_under(&self, ancestor: u32, id: u32) -> bool {
        if ancestor == id {
            return true;
        }
        if id as usize >= self.nodes.len() {
            return false;
        }
        let mut cur = id;
        let mut guard = 0;
        while !self.is_drive(cur) && guard < MAX_DEPTH {
            let Some(p) = self.parent(cur) else {
                return false;
            };
            if p == ancestor {
                return true;
            }
            cur = p;
            guard += 1;
        }
        false
    }
}

#[inline]
pub fn fold(u: u16) -> u16 {
    match u {
        0x41..=0x5a => u + 32,
        // Latin-1: Á..Þ menos ×
        0xc0..=0xde if u != 0xd7 => u + 32,
        _ => u,
    }
}

/// `hay` contiene `needle` sin distinguir mayúsculas, sin asignar memoria.
pub fn contains_ignore_case(hay: &[u16], needle: &[u16]) -> bool {
    if needle.is_empty() {
        return true;
    }
    if needle.len() > hay.len() {
        return false;
    }
    let last = hay.len() - needle.len();
    for i in 0..=last {
        if fold(hay[i]) == needle[0]
            && hay[i..i + needle.len()]
                .iter()
                .zip(needle)
                .all(|(a, b)| fold(*a) == *b)
        {
            return true;
        }
    }
    false
}

pub fn eq_ignore_case(a: &[u16], b: &str) -> bool {
    let b: Vec<u16> = b.encode_utf16().collect();
    a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| fold(*x) == fold(*y))
}

pub fn to_units_lower(s: &str) -> Vec<u16> {
    s.encode_utf16().map(fold).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(frn: u64, parent: u64, name: &str, dir: bool, size: u64) -> RawRecord {
        RawRecord {
            frn,
            parent_frn: parent,
            name: name.into(),
            size: if dir { u64::MAX } else { size },
            is_dir: dir,
            attrs: 0,
            mtime: 1_700_000_000,
            atime: 1_600_000_000,
        }
    }

    fn sample() -> Index {
        let mut b = Builder::with_capacity(8);
        b.push(rec(5, 5, ".", true, 0));
        b.push(rec(10, 5, "Users", true, 0));
        b.push(rec(11, 10, "Lucio", true, 0));
        b.push(rec(12, 11, "informe.pdf", false, 4096));
        b.push(rec(13, 11, "Ñandú.txt", false, 10));
        b.push(rec(14, 5, "Windows", true, 0));
        b.finish('C')
    }

    #[test]
    fn root_is_the_drive() {
        let ix = sample();
        assert!(ix.is_drive(ix.root));
        assert_eq!(ix.name_string(ix.root), "C:");
        assert_eq!(ix.dir_path(ix.root), "");
    }

    #[test]
    fn paths_resolve() {
        let ix = sample();
        let i = ix.find_dir("C:\\Users\\Lucio").unwrap();
        assert_eq!(ix.full_path(i), "C:\\Users\\Lucio");
        assert_eq!(ix.dir_path(i), "C:\\Users");
    }

    #[test]
    fn file_dir_path_has_no_trailing_backslash() {
        let ix = sample();
        let i = ix.find_dir("C:\\Users\\Lucio").unwrap();
        let mut found = Vec::new();
        for id in 0..ix.len() as u32 {
            if ix.nodes[id as usize].parent == i && !ix.is_dir(id) {
                found.push(ix.dir_path(id));
            }
        }
        assert_eq!(
            found,
            vec![
                "C:\\Users\\Lucio".to_string(),
                "C:\\Users\\Lucio".to_string()
            ]
        );
    }

    #[test]
    fn node_is_48_bytes() {
        assert_eq!(std::mem::size_of::<Node>(), 48);
    }

    #[test]
    fn orphans_attach_to_root() {
        let mut b = Builder::with_capacity(4);
        b.push(rec(5, 5, ".", true, 0));
        b.push(rec(99, 4242, "huerfano.txt", false, 1));
        let ix = b.finish('D');
        let id = ix.find_dir("D:").unwrap();
        let orphan = (0..ix.len() as u32)
            .find(|&i| ix.name_string(i) == "huerfano.txt")
            .unwrap();
        assert_eq!(ix.nodes[orphan as usize].parent, id);
        assert_eq!(ix.full_path(orphan), "D:\\huerfano.txt");
    }

    #[test]
    fn substring_matching() {
        let hay = to_units_lower("Informe Final.PDF");
        assert!(contains_ignore_case(&hay, &to_units_lower("pdf")));
        assert!(contains_ignore_case(&hay, &to_units_lower("INFORME")));
        assert!(contains_ignore_case(&hay, &to_units_lower("nal.p")));
        assert!(!contains_ignore_case(&hay, &to_units_lower("docx")));
        assert!(contains_ignore_case(&hay, &[]));
    }

    #[test]
    fn merge_keeps_one_root_per_volume() {
        // C: con una carpeta y un archivo, D: con un archivo distinto.
        let mut c = Builder::with_capacity(4);
        c.push(rec(5, 5, ".", true, 0));
        c.push(rec(10, 5, "Users", true, 1));
        c.push(rec(11, 10, "nota.txt", false, 2));
        let c = c.finish('C');

        let mut d = Builder::with_capacity(4);
        d.push(rec(5, 5, ".", true, 0));
        d.push(rec(20, 5, "datos.bin", false, 1));
        let d = d.finish('D');

        let ix = Index::merge(vec![c, d]);
        assert_eq!(ix.roots.len(), 2);
        assert_eq!(ix.len(), 5);
        assert_eq!(ix.root_for('C').unwrap(), ix.root);
        assert_ne!(ix.root_for('D').unwrap(), ix.root);
        assert_eq!(ix.root_for('Z'), None);

        // Cada unidad conserva su nombre de raiz y arma sus propias rutas.
        assert_eq!(ix.name_string(ix.root_for('C').unwrap()), "C:");
        assert_eq!(ix.name_string(ix.root_for('D').unwrap()), "D:");
        assert_eq!(ix.find_dir("C:"), ix.root_for('C'));
        let sep = std::path::MAIN_SEPARATOR;
        let nota = (0..ix.len() as u32)
            .find(|&i| ix.name_string(i) == "nota.txt")
            .unwrap();
        assert_eq!(ix.full_path(nota), format!("C:{sep}Users{sep}nota.txt"));
        assert_eq!(ix.dir_path(nota), format!("C:{sep}Users"));
        let datos = (0..ix.len() as u32)
            .find(|&i| ix.name_string(i) == "datos.bin")
            .unwrap();
        assert_eq!(ix.full_path(datos), format!("D:{sep}datos.bin"));
        assert_eq!(ix.dir_path(datos), "D:");

        // find_dir baja desde la raiz correcta, no desde la primera.
        let users = ix.find_dir(&format!("C:{sep}Users")).unwrap();
        assert_eq!(ix.name_string(users), "Users");
        assert!(ix.find_dir(&format!("D:{sep}Users")).is_none());
    }

    #[test]
    fn merge_of_one_volume_is_that_volume() {
        let mut b = Builder::with_capacity(2);
        b.push(rec(5, 5, ".", true, 0));
        b.push(rec(6, 5, "x.log", false, 1));
        let ix = Index::merge(vec![b.finish('E')]);
        assert_eq!(ix.roots.len(), 1);
        assert_eq!(ix.len(), 2);
        let sep = std::path::MAIN_SEPARATOR;
        assert_eq!(ix.full_path(1), format!("E:{sep}x.log"));
    }

    #[test]
    fn accented_folding() {
        let hay = to_units_lower("ÑANDÚ.txt");
        assert!(contains_ignore_case(&hay, &to_units_lower("ñandú")));
        assert_eq!(fold('Á' as u16), 'á' as u16);
    }
}
