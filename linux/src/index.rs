//! Indice en memoria: nodos planos + arena de nombres en bytes.
//!
//! Los ids se asignan en preorden, asi que el subarbol de `id` es exactamente
//! `id..tout[id]`. Eso hace que resolver una rama, ordenar por nombre y podar
//! un subarbol sean operaciones sobre rangos contiguos, sin recorrer padres.

use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::plataforma::{fnv_update, FileId, FNV_OFFSET};

pub const F_DIR: u16 = 0x0001;
pub const F_ROOT: u16 = 0x0002;
pub const F_SYMLINK: u16 = 0x0004;
pub const F_COMPLETE: u16 = 0x0008;
pub const F_HARDLINK: u16 = 0x0010;

const MAX_DEPTH: u32 = 512;

/// 64 bytes por nodo. El nombre vive en la arena, no aca.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Node {
    pub size: u64,
    /// Tamano asignado (bloques * 512), para "espacio real".
    pub alloc: u64,
    pub mtime_ns: i64,
    /// Creacion; 0 si el filesystem no soporta `STATX_BTIME`.
    pub btime_ns: i64,
    pub ino: u64,
    pub dev: u64,
    pub name_off: u32,
    pub parent: u32,
    pub name_len: u16,
    pub flags: u16,
    pub nlink: u16,
    pub mode: u16,
}

/// Lo minimo para buscar por nombre: 12 bytes en vez de 64.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct Compacto {
    pub off: u32,
    pub len: u16,
    pub flags: u16,
    pub parent: u32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct HostInfo {
    pub hostname: String,
    /// Etiqueta libre: casa, oficina, notebook...
    pub env: String,
    pub os: String,
    pub arch: String,
    pub version: String,
    /// Nombre DNS de Tailscale, si se pudo resolver al indexar.
    #[serde(default)]
    pub tailscale: String,
}

/// Una unidad indexada. `volume_id` es estable entre PC, para poder comparar
/// la misma unidad montada en rutas distintas.
#[derive(Clone, Debug, Default)]
pub struct Root {
    /// Id del nodo raiz en el indice.
    pub id: u32,
    pub volume_id: String,
    pub fs_uuid: String,
    /// Identidad de nube (`cloud_id`); vacia si no es nube.
    pub cloud_id: String,
    /// Ruta local donde estaba montada al indexar.
    pub path: String,
    pub fs: String,
    pub label: String,
    pub source: String,
    pub kind: String,
    pub total: u64,
    pub free: u64,
    pub scan_started: i64,
    pub scanned_at: i64,
    pub complete: bool,
    pub nodes: u64,
    /// Mayor mtime de archivo visto en esta unidad. Ayuda a desempatar
    /// versiones cuando los relojes de dos PC no coinciden.
    pub max_mtime: i64,
    pub method: String,
    pub subvol: String,
}

impl Root {
    pub fn display(&self) -> String {
        if !self.label.is_empty() {
            format!("{} ({})", self.label, self.path)
        } else {
            self.path.clone()
        }
    }

    /// Clave de version local. Entre PCs distintas la comparacion es por
    /// `built_at` + `max_mtime` + `content_hash` (acuerdo §4).
    pub fn version(&self) -> (i64, i64, u64) {
        (self.scan_started, self.max_mtime, self.nodes)
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Stats {
    pub nodes: u64,
    pub files: u64,
    pub dirs: u64,
    pub skipped: u64,
    pub errors: u64,
    pub reused_dirs: u64,
    pub btime_missing: u64,
    pub arena_bytes: u64,
    pub build_ms: u128,
    pub scan_ms: u128,
    pub assemble_ms: u128,
    pub derived_ms: u128,
}

#[derive(Clone, Debug, Default)]
pub struct Index {
    pub host: HostInfo,
    /// Identidad estable de esta PC (32 hex). La genera `config`.
    pub pc_id: String,
    /// Sube en cada reindexado. `Marca` = `l1:<generation>:<cambio_n>`.
    pub generation: u64,
    /// Lotes de watcher aplicados.
    pub cambio_n: u64,
    pub content_hash: u64,
    pub nodes: Vec<Node>,
    pub names: Vec<u8>,
    /// Arena plegada a minusculas. Mismos offsets que `names`.
    pub folded: Vec<u8>,
    pub compacto: Vec<Compacto>,
    /// Tamano recursivo aparente.
    pub dir_bytes: Vec<u64>,
    /// Tamano recursivo asignado (espacio real).
    pub dir_alloc: Vec<u64>,
    /// Fin del subarbol de cada nodo (exclusivo).
    pub tout: Vec<u32>,
    /// Posicion de cada nodo en el orden por nombre plegado.
    pub rango: Vec<u32>,
    /// Hijos en formato CSR.
    pub hijos_inicio: Vec<u32>,
    pub hijos: Vec<u32>,
    pub roots: Vec<Root>,
    pub built_at: i64,
    pub scan_started: i64,
    pub stats: Stats,
}

impl Index {
    pub fn marca(&self) -> String {
        format!("l1:{}:{}", self.generation, self.cambio_n)
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn name_bytes(&self, id: u32) -> &[u8] {
        let Some(n) = self.nodes.get(id as usize) else {
            return &[];
        };
        let off = n.name_off as usize;
        let end = off.saturating_add(n.name_len as usize);
        self.names.get(off..end).unwrap_or(&[])
    }

    pub fn name_folded(&self, id: u32) -> &[u8] {
        let Some(n) = self.nodes.get(id as usize) else {
            return &[];
        };
        let off = n.name_off as usize;
        let end = off.saturating_add(n.name_len as usize);
        self.folded.get(off..end).unwrap_or(&[])
    }

    pub fn name_string(&self, id: u32) -> String {
        String::from_utf8_lossy(self.name_bytes(id)).into_owned()
    }

    pub fn is_dir(&self, id: u32) -> bool {
        self.nodes
            .get(id as usize)
            .is_some_and(|n| n.flags & F_DIR != 0)
    }

    pub fn is_root(&self, id: u32) -> bool {
        self.nodes
            .get(id as usize)
            .is_some_and(|n| n.flags & F_ROOT != 0)
    }

    pub fn parent(&self, id: u32) -> Option<u32> {
        self.nodes.get(id as usize).map(|n| n.parent)
    }

    pub fn subtree_end(&self, id: u32) -> u32 {
        self.tout.get(id as usize).copied().unwrap_or(0)
    }

    pub fn subtree(&self, id: u32) -> std::ops::Range<u32> {
        id..self.subtree_end(id).max(id)
    }

    pub fn is_under(&self, ancestor: u32, id: u32) -> bool {
        if ancestor == id {
            return true;
        }
        if id as usize >= self.nodes.len() || ancestor as usize >= self.nodes.len() {
            return false;
        }
        ancestor <= id && id < self.subtree_end(ancestor)
    }

    pub fn root_index_of(&self, id: u32) -> Option<usize> {
        let pos = self.roots.partition_point(|r| r.id <= id);
        if pos == 0 {
            return None;
        }
        let i = pos - 1;
        if id < self.subtree_end(self.roots[i].id) {
            Some(i)
        } else {
            None
        }
    }

    pub fn root_of(&self, id: u32) -> Option<&Root> {
        self.root_index_of(id).map(|i| &self.roots[i])
    }

    pub fn file_id(&self, id: u32) -> Option<FileId> {
        let n = self.nodes.get(id as usize)?;
        let root = self.root_of(id)?;
        Some(FileId::linux(&root.volume_id, n.ino, n.btime_ns))
    }

    pub fn parent_id(&self, id: u32) -> Option<FileId> {
        let p = self.parent(id)?;
        self.file_id(p)
    }

    /// Camino desde la raiz hasta el nodo, sin el nombre del nodo.
    pub fn dir_path(&self, id: u32) -> String {
        if self.is_root(id) {
            return String::new();
        }
        let mut chain: Vec<u32> = Vec::with_capacity(8);
        let mut cur = id;
        let mut guard = 0;
        loop {
            let Some(p) = self.parent(cur) else {
                return String::new();
            };
            if p == cur || guard > MAX_DEPTH {
                break;
            }
            cur = p;
            guard += 1;
            if self.is_root(cur) {
                let root = self.root_of(id).map(|r| r.path.clone()).unwrap_or_default();
                let mut s = root;
                for &c in chain.iter().rev() {
                    s = join_path(&s, &self.name_string(c));
                }
                return s;
            }
            chain.push(cur);
        }
        String::new()
    }

    pub fn full_path(&self, id: u32) -> String {
        if self.is_root(id) {
            return self
                .root_of(id)
                .map(|r| r.path.clone())
                .unwrap_or_else(|| self.name_string(id));
        }
        join_path(&self.dir_path(id), &self.name_string(id))
    }

    /// Hijo directo por nombre exacto. Los hijos estan ordenados por bytes.
    pub fn child(&self, parent: u32, name: &[u8]) -> Option<u32> {
        let start = *self.hijos_inicio.get(parent as usize)? as usize;
        let end = *self.hijos_inicio.get(parent as usize + 1)? as usize;
        let slice = self.hijos.get(start..end)?;
        let mut lo = 0usize;
        let mut hi = slice.len();
        while lo < hi {
            let mid = (lo + hi) / 2;
            let id = slice[mid];
            match self.name_bytes(id).cmp(name) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(id),
            }
        }
        let folded = fold_bytes(name);
        for &id in slice {
            if self.name_folded(id) == folded.as_slice() {
                return Some(id);
            }
        }
        None
    }

    /// Resuelve una ruta del usuario a un nodo. Acepta `/`, `/home/x`,
    /// `~/x` y, en Windows (tests), `C:/...`.
    pub fn find_dir(&self, path: &str) -> Option<u32> {
        let norm = normalize_path(path);
        let mut best: Option<usize> = None;
        for (i, r) in self.roots.iter().enumerate() {
            if path_prefix_matches(&norm, &r.path) {
                let better = best
                    .map(|b| r.path.len() > self.roots[b].path.len())
                    .unwrap_or(true);
                if better {
                    best = Some(i);
                }
            }
        }
        let i = best?;
        let root = &self.roots[i];
        if norm == root.path {
            return Some(root.id);
        }
        let rel = norm
            .strip_prefix(&root.path)
            .unwrap_or("")
            .trim_start_matches('/');
        let mut cur = root.id;
        for part in rel.split('/').filter(|p| !p.is_empty()) {
            cur = self.child(cur, part.as_bytes())?;
            if !self.is_dir(cur) {
                return None;
            }
        }
        Some(cur)
    }

    pub fn root_for_path(&self, path: &str) -> Option<&Root> {
        let norm = normalize_path(path);
        self.roots
            .iter()
            .filter(|r| path_prefix_matches(&norm, &r.path))
            .max_by_key(|r| r.path.len())
    }

    pub fn root_for_volume(&self, volume_id: &str) -> Option<&Root> {
        self.roots.iter().find(|r| r.volume_id == volume_id)
    }

    pub fn node_size(&self, id: u32) -> u64 {
        if self.is_dir(id) {
            self.dir_bytes.get(id as usize).copied().unwrap_or(0)
        } else {
            self.nodes.get(id as usize).map(|n| n.size).unwrap_or(0)
        }
    }

    pub fn children(&self, id: u32) -> &[u32] {
        let Some(start) = self.hijos_inicio.get(id as usize) else {
            return &[];
        };
        let Some(end) = self.hijos_inicio.get(id as usize + 1) else {
            return &[];
        };
        self.hijos.get(*start as usize..*end as usize).unwrap_or(&[])
    }

    pub fn node_alloc(&self, id: u32) -> u64 {
        if self.is_dir(id) {
            self.dir_alloc.get(id as usize).copied().unwrap_or(0)
        } else {
            self.nodes.get(id as usize).map(|n| n.alloc).unwrap_or(0)
        }
    }

    /// FNV-1a sobre nodos + arena de nombres. Determinista y barato; es el
    /// `content_hash` que usa Atlas para desempatar versiones.
    pub fn compute_content_hash(&self) -> u64 {
        let mut h = FNV_OFFSET;
        for n in &self.nodes {
            h = fnv_update(h, &n.size.to_le_bytes());
            h = fnv_update(h, &n.alloc.to_le_bytes());
            h = fnv_update(h, &n.mtime_ns.to_le_bytes());
            h = fnv_update(h, &n.btime_ns.to_le_bytes());
            h = fnv_update(h, &n.ino.to_le_bytes());
            h = fnv_update(h, &n.dev.to_le_bytes());
            h = fnv_update(h, &n.name_off.to_le_bytes());
            h = fnv_update(h, &n.parent.to_le_bytes());
            h = fnv_update(h, &n.name_len.to_le_bytes());
            h = fnv_update(h, &n.flags.to_le_bytes());
        }
        h = fnv_update(h, &self.names);
        h
    }

    /// Reconstruye todos los indices derivados. Se llama al terminar un scan.
    pub fn rebuild(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            self.folded.clear();
            self.compacto.clear();
            self.dir_bytes.clear();
            self.dir_alloc.clear();
            self.tout.clear();
            self.rango.clear();
            self.hijos_inicio = vec![0];
            self.hijos.clear();
            self.stats = Stats::default();
            return;
        }

        self.folded.clear();
        self.folded.reserve(self.names.len());
        fold_into(&self.names, &mut self.folded);
        if self.folded.len() != self.names.len() {
            self.folded = self.names.clone();
        }

        self.compacto = self
            .nodes
            .iter()
            .map(|n| Compacto {
                off: n.name_off,
                len: n.name_len,
                flags: n.flags,
                parent: n.parent,
            })
            .collect();

        let mut counts = vec![0u32; n + 1];
        for (i, node) in self.nodes.iter().enumerate() {
            if node.parent as usize != i && (node.parent as usize) < n {
                counts[node.parent as usize + 1] += 1;
            }
        }
        for i in 1..=n {
            counts[i] += counts[i - 1];
        }
        self.hijos_inicio = counts.clone();
        let mut hijos = vec![0u32; counts[n] as usize];
        let mut cursor = counts;
        for (i, node) in self.nodes.iter().enumerate() {
            let p = node.parent as usize;
            if p != i && p < n {
                let slot = &mut cursor[p];
                hijos[*slot as usize] = i as u32;
                *slot += 1;
            }
        }
        self.hijos = hijos;

        let mut start = 0usize;
        for p in 0..n {
            let end = self.hijos_inicio[p + 1] as usize;
            if end > start {
                let slice = &mut self.hijos[start..end];
                slice.sort_unstable_by(|&a, &b| {
                    self.name_bytes(a)
                        .cmp(self.name_bytes(b))
                        .then_with(|| a.cmp(&b))
                });
            }
            start = end;
        }

        self.tout = vec![0; n];
        for id in (0..n).rev() {
            let mut end = id as u32 + 1;
            let s = self.hijos_inicio[id] as usize;
            let e = self.hijos_inicio[id + 1] as usize;
            for &c in &self.hijos[s..e] {
                let te = self.tout[c as usize];
                if te > end {
                    end = te;
                }
            }
            self.tout[id] = end;
        }

        let mut ids: Vec<u32> = (0..n as u32).collect();
        ids.par_sort_unstable_by(|&a, &b| {
            self.name_folded(a)
                .cmp(self.name_folded(b))
                .then_with(|| a.cmp(&b))
        });
        self.rango = vec![0; n];
        for (pos, &id) in ids.iter().enumerate() {
            self.rango[id as usize] = pos as u32;
        }

        let mut files = 0u64;
        let mut dirs = 0u64;
        for node in &self.nodes {
            if node.flags & F_DIR != 0 {
                dirs += 1;
            } else {
                files += 1;
            }
        }
        self.stats.files = files;
        self.stats.dirs = dirs;
        self.stats.nodes = n as u64;
        self.stats.arena_bytes = self.names.len() as u64;

        let mut agg = vec![0u64; n];
        let mut alloc = vec![0u64; n];
        let mut seen: HashSet<(u64, u64)> = HashSet::new();
        for id in (0..n).rev() {
            let node = self.nodes[id];
            let mut own = 0u64;
            let mut own_alloc = 0u64;
            if node.flags & F_DIR == 0 {
                let repeated = node.nlink > 1 && node.ino != 0;
                if !repeated || seen.insert((node.dev, node.ino)) {
                    own = node.size;
                    own_alloc = node.alloc;
                }
            }
            agg[id] = agg[id].saturating_add(own);
            alloc[id] = alloc[id].saturating_add(own_alloc);
            let p = node.parent as usize;
            if p != id && p < n {
                let c = agg[id];
                let ca = alloc[id];
                agg[p] = agg[p].saturating_add(c);
                alloc[p] = alloc[p].saturating_add(ca);
            }
        }
        self.dir_bytes = agg;
        self.dir_alloc = alloc;
    }

    pub fn mark_incomplete(&mut self, id: u32) {
        if let Some(n) = self.nodes.get_mut(id as usize) {
            n.flags &= !F_COMPLETE;
        }
    }
}

pub struct Builder {
    pub host: HostInfo,
    pub pc_id: String,
    pub generation: u64,
    pub cambio_n: u64,
    pub nodes: Vec<Node>,
    pub names: Vec<u8>,
    pub roots: Vec<Root>,
    pub scan_started: i64,
}

impl Builder {
    pub fn with_capacity(n: usize) -> Self {
        Self {
            host: HostInfo::default(),
            pc_id: String::new(),
            generation: 1,
            cambio_n: 0,
            nodes: Vec::with_capacity(n),
            names: Vec::with_capacity(n.saturating_mul(28)),
            roots: Vec::new(),
            scan_started: crate::time::now_ns(),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        name: &[u8],
        flags: u16,
        parent: u32,
        size: u64,
        alloc: u64,
        mtime_ns: i64,
        btime_ns: i64,
        dev: u64,
        ino: u64,
        nlink: u16,
        mode: u16,
    ) -> u32 {
        let id = self.nodes.len() as u32;
        let off = self.names.len() as u32;
        self.names.extend_from_slice(name);
        self.nodes.push(Node {
            size,
            alloc,
            mtime_ns,
            btime_ns,
            ino,
            dev,
            name_off: off,
            parent,
            name_len: name.len().min(u16::MAX as usize) as u16,
            flags,
            nlink,
            mode,
        });
        id
    }

    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn finish(mut self) -> Index {
        let mut ix = Index {
            host: std::mem::take(&mut self.host),
            pc_id: self.pc_id,
            generation: self.generation,
            cambio_n: self.cambio_n,
            content_hash: 0,
            nodes: self.nodes,
            names: self.names,
            folded: Vec::new(),
            compacto: Vec::new(),
            dir_bytes: Vec::new(),
            dir_alloc: Vec::new(),
            tout: Vec::new(),
            rango: Vec::new(),
            hijos_inicio: Vec::new(),
            hijos: Vec::new(),
            roots: self.roots,
            built_at: crate::time::now_ns(),
            scan_started: self.scan_started,
            stats: Stats::default(),
        };
        ix.rebuild();
        ix.content_hash = ix.compute_content_hash();
        ix
    }
}

/// Pliega un byte ASCII a minusculas. Latin-1 se maneja en `fold_into`.
pub fn fold(b: u8) -> u8 {
    if b.is_ascii_uppercase() {
        b + 32
    } else {
        b
    }
}

pub fn fold_bytes(src: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(src.len());
    fold_into(src, &mut out);
    out
}

/// Plegado que conserva offsets: en UTF-8, las mayusculas Latin-1
/// (U+00C0..U+00DE, sin U+00D7) cambian solo el byte de continuacion.
pub fn fold_into(src: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < src.len() {
        let b = src[i];
        if b == 0xC3 && i + 1 < src.len() {
            let c = src[i + 1];
            if (0x80..=0x9E).contains(&c) && c != 0x97 {
                out.push(0xC3);
                out.push(c + 0x20);
                i += 2;
                continue;
            }
            out.push(b);
            out.push(c);
            i += 2;
            continue;
        }
        out.push(fold(b));
        i += 1;
    }
}

pub fn path_string(p: &Path) -> String {
    #[cfg(windows)]
    {
        p.to_string_lossy().replace('\\', "/")
    }
    #[cfg(not(windows))]
    {
        p.to_string_lossy().into_owned()
    }
}

pub fn join_path(base: &str, name: &str) -> String {
    if base.is_empty() {
        return name.to_string();
    }
    if base == "/" {
        return format!("/{name}");
    }
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Normaliza una ruta escrita por el usuario: separadores, `.`, `..`, `~`
/// y barras repetidas. No toca el disco.
pub fn normalize_path(input: &str) -> String {
    #[cfg(windows)]
    let input = input.replace('\\', "/");
    let input = input.trim();
    if input.is_empty() {
        return String::new();
    }
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_default();
    let expanded;
    let s = if input == "~" {
        expanded = home;
        expanded.as_str()
    } else if let Some(rest) = input.strip_prefix("~/") {
        expanded = format!("{home}/{rest}");
        expanded.as_str()
    } else {
        input
    };
    let absolute = s.starts_with('/') || (s.len() > 1 && s.as_bytes()[1] == b':');
    let mut parts: Vec<&str> = Vec::new();
    for part in s.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    let mut out = String::new();
    if absolute {
        out.push('/');
    }
    out.push_str(&parts.join("/"));
    if out.is_empty() {
        if absolute {
            "/".into()
        } else {
            ".".into()
        }
    } else {
        out
    }
}

pub fn path_prefix_matches(path: &str, prefix: &str) -> bool {
    if path == prefix {
        return true;
    }
    if prefix == "/" {
        return path.starts_with('/');
    }
    path.len() > prefix.len()
        && path.starts_with(prefix)
        && path.as_bytes().get(prefix.len()) == Some(&b'/')
}

pub fn contains_folded(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() {
        return true;
    }
    memchr::memmem::find(hay, needle).is_some()
}

/// Indice compartido entre consultas y recarga en caliente.
pub type SharedIndex = Arc<Index>;

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Index {
        let mut b = Builder::with_capacity(16);
        let root = b.push(b"/", F_DIR | F_ROOT | F_COMPLETE, 0, 0, 0, 0, 0, 1, 1, 1, 0o755);
        b.nodes[root as usize].parent = root;
        let home = b.push(
            b"home",
            F_DIR | F_COMPLETE,
            root,
            0,
            0,
            0,
            0,
            1,
            2,
            1,
            0o755,
        );
        let user = b.push(
            b"Lucio",
            F_DIR | F_COMPLETE,
            home,
            0,
            0,
            0,
            0,
            1,
            3,
            1,
            0o755,
        );
        b.push(
            b"informe.pdf",
            0,
            user,
            4096,
            8192,
            1_700_000_000_000_000_000,
            1_650_000_000_000_000_000,
            1,
            4,
            1,
            0o644,
        );
        b.push(
            b"Notas.txt",
            0,
            user,
            10,
            4096,
            1_600_000_000_000_000_000,
            0,
            1,
            5,
            1,
            0o644,
        );
        let mut ix = b.finish();
        ix.roots.push(Root {
            id: root,
            volume_id: "vol-test".into(),
            fs_uuid: "uuid:test".into(),
            path: "/".into(),
            fs: "ext4".into(),
            ..Default::default()
        });
        ix
    }

    #[test]
    fn node_is_64_bytes() {
        assert_eq!(std::mem::size_of::<Node>(), 64);
        assert_eq!(std::mem::size_of::<Compacto>(), 12);
    }

    #[test]
    fn paths_resolve() {
        let ix = sample();
        let id = ix.find_dir("/home/Lucio").unwrap();
        assert_eq!(ix.full_path(id), "/home/Lucio");
        assert_eq!(ix.dir_path(id), "/home");
        let f = ix.find_dir("/home/lucio").unwrap();
        assert_eq!(ix.name_string(f), "Lucio");
        assert!(ix.find_dir("/home/nope").is_none());
    }

    #[test]
    fn preorder_and_subtrees() {
        let ix = sample();
        let root = ix.roots[0].id;
        assert_eq!(ix.subtree_end(root), ix.len() as u32 - 0);
        let home = ix.find_dir("/home").unwrap();
        assert_eq!(
            ix.subtree(home).end,
            ix.find_dir("/home/Lucio").unwrap() + 3
        );
        assert!(ix.is_under(root, 4));
        assert!(!ix.is_under(home, root));
    }

    #[test]
    fn sizes_are_recursive() {
        let ix = sample();
        let root = ix.roots[0].id;
        assert_eq!(ix.node_size(root), 4106);
        assert_eq!(ix.node_alloc(root), 12288);
        assert_eq!(ix.node_size(ix.find_dir("/home").unwrap()), 4106);
    }

    #[test]
    fn folded_keeps_offsets_with_accents() {
        let src = "ÑANDÚ.txt".as_bytes();
        let out = fold_bytes(src);
        assert_eq!(out.len(), src.len());
        assert_eq!(out, "ñandú.txt".as_bytes());
        assert!(contains_folded(&out, b"ñand"));
    }

    #[test]
    fn normalize_paths() {
        assert_eq!(normalize_path("/home//x/./y/"), "/home/x/y");
        assert_eq!(normalize_path("/home/x/../y"), "/home/y");
        assert_eq!(normalize_path("/"), "/");
    }

    #[test]
    fn join_paths() {
        assert_eq!(join_path("/", "home"), "/home");
        assert_eq!(join_path("/mnt/usb", "a"), "/mnt/usb/a");
        assert_eq!(join_path("C:/x", "a"), "C:/x/a");
    }

    #[test]
    fn hardlinks_count_once() {
        let mut b = Builder::with_capacity(8);
        let root = b.push(b"/", F_DIR | F_ROOT, 0, 0, 0, 0, 0, 1, 1, 1, 0o755);
        b.nodes[root as usize].parent = root;
        b.push(b"a", F_HARDLINK, root, 100, 4096, 0, 0, 1, 10, 2, 0o644);
        b.push(b"b", F_HARDLINK, root, 100, 4096, 0, 0, 1, 10, 2, 0o644);
        let ix = b.finish();
        assert_eq!(ix.node_size(root), 100);
        assert_eq!(ix.node_alloc(root), 4096);
    }

    #[test]
    fn file_id_uses_volume_and_btime() {
        let ix = sample();
        let f = ix.find_dir("/home/Lucio").unwrap();
        let id = ix.file_id(f).unwrap();
        assert_eq!(id.key.volume_id, "vol-test");
        assert_eq!(id.key.ino, 3);
        assert_eq!(id.btime_ns, 0);
    }
}
