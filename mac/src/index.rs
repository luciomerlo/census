//! Índice en RAM: nodos planos, arena de nombres UTF-8 y registro compacto.
//!
//! El layout de `Compacto` (12 B: `off`, `len`, `flags`, `parent`) es el
//! acordado con Windows y Linux. En macOS `off`/`len` son bytes de la arena
//! plegada UTF-8; en Windows son unidades UTF-16. La consulta es idéntica, los
//! offsets no se comparan entre plataformas.

use std::collections::{HashMap, HashSet};

use crate::types::{
    fnv1a64, Entrada, FileId, FileKey, Kind, Marca, Provenance, Stats, Volume, FLAG_DIR,
    FLAG_HARDLINK, FLAG_HIDDEN, FLAG_ROOT, FLAG_SYMLINK,
};

/// 12 bytes por nodo para escanear por nombre.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Compacto {
    /// Offset en la arena plegada (bytes).
    pub off: u32,
    /// Largo en bytes.
    pub len: u16,
    pub flags: u16,
    /// Id del padre.
    pub parent: u32,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Node {
    pub name_off: u32,
    pub name_len: u16,
    pub flags: u16,
    pub parent: u32,
    pub size: u64,
    pub alloc: u64,
    pub mtime_ns: i64,
    pub btime_ns: i64,
    pub ino: u64,
    pub gen: u32,
    /// Índice en `Index::volumes`.
    pub vol: u16,
    pub nlink: u32,
}

impl Node {
    pub fn is_dir(&self) -> bool {
        self.flags & FLAG_DIR != 0
    }

    pub fn is_symlink(&self) -> bool {
        self.flags & FLAG_SYMLINK != 0
    }
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub roots: Vec<u32>,
    pub nodes: Vec<Node>,
    pub names: Vec<u8>,
    pub folded: Vec<u8>,
    pub compacto: Vec<Compacto>,
    pub dir_bytes: Vec<u64>,
    pub hijos_inicio: Vec<u32>,
    pub hijos: Vec<u32>,
    pub por_nombre: Vec<u32>,
    pub euler: Vec<u32>,
    pub tin: Vec<u32>,
    pub tout: Vec<u32>,
    pub volumes: Vec<Volume>,
    pub built_at: i64,
    pub generation: u64,
    pub marca: Marca,
    pub provenance: Provenance,
    pub stats: Stats,
    pub content_hash: u64,
}

impl Index {
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn name(&self, id: u32) -> &[u8] {
        let n = &self.nodes[id as usize];
        let off = n.name_off as usize;
        &self.names[off..off + n.name_len as usize]
    }

    pub fn folded_name(&self, id: u32) -> &[u8] {
        let c = self.compacto[id as usize];
        let off = c.off as usize;
        &self.folded[off..off + c.len as usize]
    }

    pub fn name_string_lossy(&self, id: u32) -> String {
        String::from_utf8_lossy(self.name(id)).into_owned()
    }

    pub fn file_id(&self, id: u32) -> FileId {
        let n = &self.nodes[id as usize];
        let vol = self.volumes.get(n.vol as usize);
        FileId::new(
            vol.map(|v| v.id.clone()).unwrap_or_default(),
            n.ino,
            n.gen,
            n.btime_ns,
        )
    }

    pub fn children(&self, id: u32) -> &[u32] {
        let a = self.hijos_inicio[id as usize] as usize;
        let b = self.hijos_inicio[id as usize + 1] as usize;
        &self.hijos[a..b]
    }

    pub fn is_under(&self, ancestor: u32, id: u32) -> bool {
        let tin = self.tin[ancestor as usize];
        let tout = self.tout[ancestor as usize];
        let t = self.tin[id as usize];
        t >= tin && t < tout
    }

    /// Ruta completa en POSIX. La raíz `/` no duplica la barra.
    pub fn path_of(&self, id: u32) -> String {
        let mut partes: Vec<u32> = Vec::new();
        let mut cur = id;
        let mut guard = 0usize;
        loop {
            partes.push(cur);
            let p = self.nodes[cur as usize].parent;
            if p == cur || guard > self.nodes.len() {
                break;
            }
            cur = p;
            guard += 1;
        }
        partes.reverse();
        let mut out = String::new();
        for (i, &n) in partes.iter().enumerate() {
            let name = self.name_string_lossy(n);
            if i == 0 {
                out.push_str(&name);
            } else {
                if !out.ends_with('/') {
                    out.push('/');
                }
                out.push_str(&name);
            }
        }
        if out.is_empty() {
            "/".into()
        } else {
            out
        }
    }

    /// Carpeta que contiene a `id` (sin la barra final), o `/` en la raíz.
    pub fn dir_path(&self, id: u32) -> String {
        let parent = self.nodes[id as usize].parent;
        if parent == id {
            return self.path_of(id);
        }
        self.path_of(parent)
    }

    pub fn kind(&self, id: u32) -> Kind {
        let f = self.nodes[id as usize].flags;
        if f & FLAG_DIR != 0 {
            Kind::Dir
        } else if f & FLAG_SYMLINK != 0 {
            Kind::Symlink
        } else {
            Kind::File
        }
    }

    pub fn is_hidden(&self, id: u32) -> bool {
        self.nodes[id as usize].flags & FLAG_HIDDEN != 0
    }

    /// Busca una carpeta por ruta absoluta. Elige la raíz más específica
    /// (`/Volumes/USB` antes que `/`) y tolera mayúsculas distintas.
    pub fn find_dir(&self, path: &str) -> Option<u32> {
        let path = normalize_dir(path);
        let mut best: Option<(usize, u32, &str)> = None;
        for &root in &self.roots {
            let mount = self.path_of(root);
            let m = normalize_dir(&mount);
            let matches = if m == "/" {
                path.starts_with('/')
            } else {
                path == m || path.starts_with(&format!("{m}/"))
            };
            if matches {
                let len = m.len();
                if best.as_ref().map_or(true, |(l, _, _)| len > *l) {
                    best = Some((len, root, mount.as_str()));
                }
            }
        }
        let (_, root, _) = best?;
        if path == normalize_dir(&self.path_of(root)) {
            return Some(root);
        }
        let base = normalize_dir(&self.path_of(root));
        let rest = path
            .strip_prefix(&base)
            .unwrap_or(&path)
            .trim_start_matches('/');
        let mut cur = root;
        for comp in rest.split('/').filter(|c| !c.is_empty()) {
            let found = self.find_child(cur, comp.as_bytes())?;
            if !self.nodes[found as usize].is_dir() {
                return None;
            }
            cur = found;
        }
        Some(cur)
    }

    fn find_child(&self, parent: u32, name: &[u8]) -> Option<u32> {
        let children = self.children(parent);
        for &c in children {
            if self.name(c) == name {
                return Some(c);
            }
        }
        // macOS es case-insensitive por defecto en APFS/HFS+: segundo intento.
        let folded = crate::query::fold_bytes(name);
        for &c in children {
            if self.folded_name(c) == folded.as_slice() {
                return Some(c);
            }
        }
        None
    }
}

pub fn normalize_dir(path: &str) -> String {
    let mut p = path.trim().replace('\\', "/");
    while p.len() > 1 && p.ends_with('/') {
        p.pop();
    }
    if p.is_empty() {
        "/".into()
    } else if p.starts_with('/') {
        p
    } else {
        format!("/{p}")
    }
}

/// Constructor incremental. Consume lotes y no guarda `Entrada` completas.
pub struct Builder {
    nodes: Vec<Node>,
    names: Vec<u8>,
    folded: Vec<u8>,
    volumes: Vec<Volume>,
    roots: Vec<u32>,
    por_clave: HashMap<FileKey, u32>,
    pendientes: Vec<Entrada>,
    stats: Stats,
    skipped: u64,
    orphans: u64,
    started: std::time::Instant,
}

impl Builder {
    pub fn new(volumes: Vec<Volume>) -> Self {
        Builder {
            nodes: Vec::new(),
            names: Vec::new(),
            folded: Vec::new(),
            volumes,
            roots: Vec::new(),
            por_clave: HashMap::new(),
            pendientes: Vec::new(),
            stats: Stats::default(),
            skipped: 0,
            orphans: 0,
            started: std::time::Instant::now(),
        }
    }

    pub fn push_batch(&mut self, lote: &[Entrada]) {
        for e in lote {
            self.push_one(e.clone());
        }
    }

    fn push_one(&mut self, e: Entrada) {
        if e.name.is_empty() {
            self.skipped += 1;
            return;
        }
        let key = e.id.key();
        if self.por_clave.contains_key(&key) {
            self.skipped += 1;
            return;
        }
        let parent_key = e.parent_id.key();
        let is_root = parent_key == key;
        let parent = if is_root {
            0
        } else {
            match self.por_clave.get(&parent_key) {
                Some(&p) => p,
                None => {
                    self.pendientes.push(e);
                    return;
                }
            }
        };
        let id = self.nodes.len() as u32;
        let off = self.names.len() as u32;
        if off == u32::MAX {
            self.skipped += 1;
            return;
        }
        let mut flags = e.normalized_flags();
        if is_root {
            flags |= FLAG_ROOT;
        }
        if e.name.first() == Some(&b'.') {
            flags |= FLAG_HIDDEN;
        }
        let folded = crate::query::fold_bytes(&e.name);
        self.names.extend_from_slice(&e.name);
        self.folded.extend_from_slice(&folded);
        let vol = self.vol_index(&e.id);
        self.nodes.push(Node {
            name_off: off,
            name_len: e.name.len().min(u16::MAX as usize) as u16,
            flags,
            parent,
            size: e.size,
            alloc: e.alloc,
            mtime_ns: e.mtime_ns,
            btime_ns: e.btime_ns,
            ino: e.id.key.ino,
            gen: e.id.key.gen,
            vol,
            nlink: e.nlink,
        });
        if is_root {
            // El padre de la raíz es ella misma, como en Windows.
            self.nodes[id as usize].parent = id;
            self.roots.push(id);
        }
        self.por_clave.insert(key, id);
        if flags & FLAG_DIR != 0 {
            self.stats.dirs += 1;
        } else {
            self.stats.files += 1;
        }
        self.stats.records += 1;
        if self.stats.records % 1_000_000 == 0 {
            eprintln!("  {} nodos...", self.stats.records);
        }
    }

    fn vol_index(&self, id: &FileId) -> u16 {
        self.volumes
            .iter()
            .position(|v| &*v.id == id.volume_id())
            .unwrap_or(0) as u16
    }

    fn flush_pending(&mut self) {
        // Un padre puede llegar después que un hijo si hay dos workers. Se
        // reintenta hasta que no haya progreso; lo que quede se cuelga de la
        // raíz y se cuenta, no se pierde.
        loop {
            if self.pendientes.is_empty() {
                break;
            }
            let antes = self.pendientes.len();
            let lote = std::mem::take(&mut self.pendientes);
            for e in lote {
                self.push_one(e);
            }
            if self.pendientes.len() >= antes {
                break;
            }
        }
        if !self.pendientes.is_empty() {
            let pend = std::mem::take(&mut self.pendientes);
            self.orphans += pend.len() as u64;
            for mut e in pend {
                if let Some(&root) = self.roots.first() {
                    let key = e.id.key();
                    let off = self.names.len() as u32;
                    let folded = crate::query::fold_bytes(&e.name);
                    self.names.extend_from_slice(&e.name);
                    self.folded.extend_from_slice(&folded);
                    let mut flags = e.normalized_flags();
                    if e.name.first() == Some(&b'.') {
                        flags |= FLAG_HIDDEN;
                    }
                    let id = self.nodes.len() as u32;
                    let vol = self.vol_index(&e.id);
                    self.nodes.push(Node {
                        name_off: off,
                        name_len: e.name.len().min(u16::MAX as usize) as u16,
                        flags,
                        parent: root,
                        size: e.size,
                        alloc: e.alloc,
                        mtime_ns: e.mtime_ns,
                        btime_ns: e.btime_ns,
                        ino: e.id.key.ino,
                        gen: e.id.key.gen,
                        vol,
                        nlink: e.nlink,
                    });
                    self.por_clave.insert(key, id);
                    if flags & FLAG_DIR != 0 {
                        self.stats.dirs += 1;
                    } else {
                        self.stats.files += 1;
                    }
                    self.stats.records += 1;
                    e.name.clear();
                }
            }
        }
    }

    pub fn finish(
        mut self,
        built_at: i64,
        generation: u64,
        marca: Marca,
        provenance: Provenance,
    ) -> Index {
        self.flush_pending();
        let n = self.nodes.len();
        // Hijos por carpeta en CSR.
        let mut counts = vec![0u32; n + 1];
        for node in &self.nodes {
            counts[node.parent as usize + 1] += 1;
        }
        for i in 0..n {
            counts[i + 1] += counts[i];
        }
        let mut hijos_inicio = counts.clone();
        let mut hijos = vec![0u32; n];
        let mut cursor = counts;
        for (id, node) in self.nodes.iter().enumerate() {
            if node.parent as usize != id {
                let p = node.parent as usize;
                hijos[cursor[p] as usize] = id as u32;
                cursor[p] += 1;
            }
        }
        // Compacto para escanear sin tocar Node.
        let compacto: Vec<Compacto> = self
            .nodes
            .iter()
            .map(|node| Compacto {
                off: node.name_off,
                len: node.name_len,
                flags: node.flags,
                parent: node.parent,
            })
            .collect();
        // Orden por nombre plegado, desempate estable por id.
        let mut por_nombre: Vec<u32> = (0..n as u32).collect();
        por_nombre.sort_unstable_by(|&a, &b| {
            let ca = compacto[a as usize];
            let cb = compacto[b as usize];
            let na = &self.folded[ca.off as usize..ca.off as usize + ca.len as usize];
            let nb = &self.folded[cb.off as usize..cb.off as usize + cb.len as usize];
            na.cmp(nb).then(a.cmp(&b))
        });
        // Tamaño recursivo. Un hardlink cuenta una vez; los clones APFS se
        // cuentan dos veces (se marcan aparte en una fase posterior).
        let mut dir_bytes = vec![0u64; n];
        let mut vistos: HashSet<(u16, u64)> = HashSet::new();
        for id in (0..n).rev() {
            let node = &self.nodes[id];
            if node.flags & FLAG_DIR != 0 {
                continue;
            }
            let mut cuenta = true;
            if node.flags & FLAG_HARDLINK != 0 {
                cuenta = vistos.insert((node.vol, node.ino));
            }
            if cuenta {
                dir_bytes[id] = node.alloc.max(node.size);
            }
        }
        for id in (0..n).rev() {
            let p = self.nodes[id].parent;
            if p as usize != id && p < n as u32 {
                let v = dir_bytes[id];
                dir_bytes[p as usize] = dir_bytes[p as usize].saturating_add(v);
            }
        }
        // Euler: preorden y rango de subárbol, iterativo.
        let mut euler = Vec::with_capacity(n);
        let mut tin = vec![u32::MAX; n];
        let mut tout = vec![u32::MAX; n];
        let roots = self.roots.clone();
        for &root in &roots {
            let mut stack: Vec<(u32, bool)> = vec![(root, false)];
            while let Some((id, salida)) = stack.pop() {
                if salida {
                    tout[id as usize] = euler.len() as u32;
                    continue;
                }
                if tin[id as usize] != u32::MAX {
                    continue;
                }
                tin[id as usize] = euler.len() as u32;
                euler.push(id);
                stack.push((id, true));
                let a = hijos_inicio[id as usize] as usize;
                let b = hijos_inicio[id as usize + 1] as usize;
                for &c in hijos[a..b].iter().rev() {
                    stack.push((c, false));
                }
            }
        }
        self.stats.name_bytes = self.names.len() as u64;
        self.stats.build_ms = self.started.elapsed().as_millis() as u64;
        self.stats.skipped = self.skipped + self.orphans;
        let content_hash = fnv1a64(&[&self.names, &self.folded]);
        Index {
            roots,
            nodes: self.nodes,
            names: self.names,
            folded: self.folded,
            compacto,
            dir_bytes,
            hijos_inicio,
            hijos,
            por_nombre,
            euler,
            tin,
            tout,
            volumes: self.volumes,
            built_at,
            generation,
            marca,
            provenance,
            stats: self.stats,
            content_hash,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{dir_volume_id, volume_id, Encoding, Volume};

    fn vol(id: &str) -> Volume {
        Volume {
            id: dir_volume_id("test", id),
            name: id.into(),
            mount: id.into(),
            fs: "apfs".into(),
            local: true,
            method: "test".into(),
            ..Default::default()
        }
    }

    fn ent(id: &str, parent: &str, ino: u64, name: &str, dir: bool, size: u64) -> Entrada {
        let v = dir_volume_id("test", "/");
        Entrada {
            id: FileId::new(v.clone(), ino, 0, 1000 + ino as i64),
            parent_id: FileId::new(v, ino_of(parent), 0, 0),
            dev_id: 1,
            kind: if dir { Kind::Dir } else { Kind::File },
            size,
            alloc: size,
            mtime_ns: 1_700_000_000_000_000_000,
            btime_ns: 1000 + ino as i64,
            nlink: 1,
            flags: 0,
            name: name.as_bytes().to_vec(),
            encoding: Encoding::Utf8,
        }
    }

    fn ino_of(s: &str) -> u64 {
        match s {
            "/" => 1,
            "Users" => 2,
            "Lucio" => 3,
            "informe.pdf" => 10,
            "Ñandú.txt" => 11,
            "foto.jpg" => 12,
            "dup.txt" => 13,
            _ => 99,
        }
    }

    fn sample() -> Index {
        let mut b = Builder::new(vec![vol("/")]);
        let root = FileId::new(dir_volume_id("test", "/"), 1, 0, 1001);
        let mut r = ent("/", "/", 1, "/", true, 0);
        r.id = root.clone();
        r.parent_id = root.clone();
        b.push_batch(&[r]);
        let mut users = ent("/", "/", 2, "Users", true, 0);
        users.parent_id = root.clone();
        b.push_batch(&[users]);
        let mut lucio = ent("Users", "Users", 3, "Lucio", true, 0);
        lucio.parent_id = FileId::new(dir_volume_id("test", "/"), 2, 0, 0);
        b.push_batch(&[lucio]);
        let mut f = ent("Lucio", "Lucio", 10, "informe.pdf", false, 4096);
        f.parent_id = FileId::new(dir_volume_id("test", "/"), 3, 0, 0);
        let mut n = ent("Lucio", "Lucio", 11, "Ñandú.txt", false, 10);
        n.parent_id = f.parent_id.clone();
        b.push_batch(&[f, n]);
        let mut d1 = ent("Lucio", "Lucio", 13, "dup.txt", false, 100);
        d1.parent_id = FileId::new(dir_volume_id("test", "/"), 3, 0, 0);
        d1.nlink = 2;
        let mut d2 = ent("Lucio", "Lucio", 13, "dup-enlace.txt", false, 100);
        d2.parent_id = FileId::new(dir_volume_id("test", "/"), 3, 0, 0);
        d2.nlink = 2;
        b.push_batch(&[d1, d2]);
        b.finish(1_700_000_000, 1, Marca("m1:test:1".into()), Provenance::default())
    }

    #[test]
    fn builds_tree_paths_and_sizes() {
        let ix = sample();
        assert_eq!(ix.roots.len(), 1);
        let root = ix.roots[0];
        assert_eq!(ix.path_of(root), "/");
        let users = ix.find_dir("/Users").unwrap();
        assert_eq!(ix.path_of(users), "/Users");
        let lucio = ix.find_dir("/Users/Lucio").unwrap();
        assert_eq!(ix.path_of(lucio), "/Users/Lucio");
        let f = ix.find_dir("/users/lucio").unwrap();
        assert_eq!(f, lucio, "APFS suele ser case-insensitive");
        // 4096 + 10 + 100 (el hardlink cuenta una vez).
        assert_eq!(ix.dir_bytes[lucio as usize], 4206);
        assert_eq!(ix.dir_bytes[root as usize], 4206);
        assert_eq!(ix.stats.files, 4);
        assert_eq!(ix.stats.dirs, 3);
    }

    #[test]
    fn compacto_is_12_bytes() {
        assert_eq!(std::mem::size_of::<Compacto>(), 12);
        let ix = sample();
        assert_eq!(ix.compacto.len(), ix.nodes.len());
        let id = ix.find_dir("/Users/Lucio").unwrap();
        let c = ix.compacto[id as usize];
        assert_eq!(&ix.folded[c.off as usize..c.off as usize + c.len as usize], b"lucio");
    }

    #[test]
    fn subtree_ranges() {
        let ix = sample();
        let users = ix.find_dir("/Users").unwrap();
        let root = ix.roots[0];
        assert!(ix.is_under(root, users));
        assert!(!ix.is_under(users, root));
        assert_eq!(ix.tout[users as usize] - ix.tin[users as usize], 5);
    }

    #[test]
    fn root_parent_is_self() {
        let ix = sample();
        let r = ix.roots[0];
        assert_eq!(ix.nodes[r as usize].parent, r);
    }
}
