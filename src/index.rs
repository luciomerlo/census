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
    pub names: Arena<u16>,
    pub stats: Stats,
    pub built_at: i64,
    /// Tamaño recursivo por nodo (archivos: su tamaño; carpetas: la suma).
    pub dir_bytes: Vec<u64>,
    /// Número de serie del volumen de cada raíz (paralelo a `roots`).
    pub vol_serials: Vec<u64>,
    /// Copia de `names` en minúsculas y sin acentos: buscar no vuelve a plegar
    /// carácter por carácter en cada consulta.
    pub folded: Arena<u16>,
    /// Hijos por carpeta en formato CSR: los hijos de `id` son
    /// `hijos[hijos_inicio[id]..hijos_inicio[id+1]]`.
    pub hijos_inicio: Vec<u32>,
    pub hijos: Vec<u32>,
    /// Ids ordenados por nombre plegado, y la posición de cada id en ese orden.
    pub por_nombre: Vec<u32>,
    pub rango: Vec<u32>,
    /// Recorrido en preorden (ids) y rango de cada subárbol: los descendientes
    /// de `id` son `euler[tin[id]..tout[id]]`. Es lo que permite buscar por
    /// carpeta sin recorrer el índice entero.
    pub euler: Vec<u32>,
    pub tin: Vec<u32>,
    pub tout: Vec<u32>,
    /// Registro mínimo para escanear por nombre: 12 bytes en vez de 48. La
    /// consulta lee 3 veces menos memoria y eso se nota en cada búsqueda.
    pub compacto: Vec<Compacto>,
    /// `true` si `nodes` está ordenado por FRN (lo está cuando viene de la MFT).
    pub frn_sorted: bool,
    /// Cambios aplicados desde el snapshot (USN Journal, C2). Va detrás de un
    /// cerrojo porque el hilo del journal lo actualiza mientras la ventana y
    /// el IPC consultan. Se toma **una vez por consulta**, nunca por nodo.
    pub overlay: std::sync::Arc<std::sync::RwLock<crate::overlay::Overlay>>,
}

/// Lo mínimo para buscar por nombre: dónde está, cuánto mide, qué es y quién es
/// su padre. Se arma una vez y se usa en cada consulta.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Compacto {
    /// Offset en la arena plegada.
    pub off: u32,
    /// Largo en unidades UTF-16.
    pub len: u16,
    pub flags: u16,
    /// Id del padre.
    pub parent: u32,
}

/// Arena de valores: propia o prestada de un archivo mapeado.
///
/// El snapshot se mapea en memoria y las dos arenas grandes (`names` y
/// `folded`, 156 MB juntas) se leen directo del mapa: no se copian. El resto
/// (nodos y bloques chicos) se sigue copiando porque es más simple y se toca
/// distinto.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Datos<T: Copy> {
    #[default]
    Vacio,
    Propio(*const T, usize),
    Mapeado(*const T, usize),
}

/// # Safety
/// `Datos` sólo se usa dentro de `Arena`, que garantiza que el puntero sigue
/// vivo: o apunta a un `Vec` que la arena posee, o a un mapa que se filtra a
/// propósito para que dure lo que el proceso.
#[derive(Debug)]
pub struct Arena<T: Copy> {
    datos: Datos<T>,
    /// `Vec` dueño cuando la arena es propia.
    propio: Option<Vec<T>>,
}

impl<T: Copy> Default for Arena<T> {
    fn default() -> Self {
        Arena {
            datos: Datos::Vacio,
            propio: None,
        }
    }
}

impl<T: Copy> Arena<T> {
    pub fn propio(v: Vec<T>) -> Self {
        Arena {
            datos: Datos::Vacio,
            propio: Some(v),
        }
    }

    /// Arena prestada del mapa. El mapa tiene que vivir más que el índice
    /// (se filtra a propósito al cargar el snapshot).
    pub fn prestado(slice: &'static [T]) -> Self {
        Arena {
            datos: Datos::Vacio,
            propio: None,
        }
        .con(slice)
    }

    fn con(mut self, slice: &'static [T]) -> Self {
        self.datos = Datos::Mapeado(slice.as_ptr(), slice.len());
        self
    }

    pub fn as_slice(&self) -> &[T] {
        if let Some(v) = &self.propio {
            return v;
        }
        match self.datos {
            Datos::Mapeado(p, n) | Datos::Propio(p, n) if !p.is_null() => {
                // SAFETY: el puntero viene de un `Vec` propio o de un mapa
                // filtrado; `n` es su largo exacto.
                unsafe { std::slice::from_raw_parts(p, n) }
            }
            _ => &[],
        }
    }
}

// Las arenas son de sólo lectura una vez construidas: se comparten entre los
// hilos de consulta sin sincronización. Los punteros apuntan a memoria que vive
// tanto como el índice (Vec propio o mapa filtrado).
unsafe impl<T: Copy + Send> Send for Arena<T> {}
unsafe impl<T: Copy + Sync> Sync for Arena<T> {}

impl<T: Copy> std::ops::Deref for Arena<T> {
    type Target = [T];
    fn deref(&self) -> &[T] {
        self.as_slice()
    }
}

impl<T: Copy + PartialEq> PartialEq for Arena<T> {
    fn eq(&self, otro: &Self) -> bool {
        self.as_slice() == otro.as_slice()
    }
}

impl<T: Copy + Eq> Eq for Arena<T> {}

impl<T: Copy> From<Vec<T>> for Arena<T> {
    fn from(v: Vec<T>) -> Self {
        Arena::propio(v)
    }
}

impl<'a, T: Copy> IntoIterator for &'a Arena<T> {
    type Item = &'a T;
    type IntoIter = std::slice::Iter<'a, T>;
    fn into_iter(self) -> Self::IntoIter {
        self.as_slice().iter()
    }
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

        let mut ix = Index {
            letter: letter.to_ascii_uppercase(),
            root,
            roots: vec![root],
            nodes: self.nodes,
            names: Arena::propio(self.names),
            stats,
            built_at: 0,
            dir_bytes: Vec::new(),
            vol_serials: Vec::new(),
            folded: Arena::default(),
            hijos_inicio: Vec::new(),
            hijos: Vec::new(),
            por_nombre: Vec::new(),
            rango: Vec::new(),
            euler: Vec::new(),
            tin: Vec::new(),
            tout: Vec::new(),
            compacto: Vec::new(),
            frn_sorted: false,
            overlay: std::sync::Arc::new(
                std::sync::RwLock::new(crate::overlay::Overlay::default()),
            ),
        };
        ix.compute_dir_bytes();
        ix.compute_folded();
        ix.rebuild_indexes();
        ix
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

    /// Nombre plegado (minúsculas, sin acentos) para comparar rápido.
    pub fn name_folded(&self, id: u32) -> &[u16] {
        let Some(n) = self.nodes.get(id as usize) else {
            return &[];
        };
        let off = n.name_off as usize;
        let end = off.saturating_add(n.name_len as usize);
        self.folded.get(off..end).unwrap_or(&[])
    }

    /// Construye el arena plegado, con exactamente los mismos offsets.
    pub fn compute_folded(&mut self) {
        self.folded = Arena::propio(self.names.iter().map(|u| fold(*u)).collect());
    }

    pub fn name_string(&self, id: u32) -> String {
        String::from_utf16_lossy(self.name_utf16(id))
    }

    pub fn is_dir(&self, id: u32) -> bool {
        if let Some(e) = self.overlay_extra(id) {
            return e.carpeta;
        }
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

    /// Cerrojo de lectura del overlay (los cerrojos no se envenenan acá: si un
    /// hilo murió con el lock tomado, seguimos igual).
    pub fn overlay(&self) -> std::sync::RwLockReadGuard<'_, crate::overlay::Overlay> {
        self.overlay.read().unwrap_or_else(|e| e.into_inner())
    }

    /// Cerrojo de escritura del overlay (lo usa el hilo del journal).
    pub fn overlay_mut(&self) -> std::sync::RwLockWriteGuard<'_, crate::overlay::Overlay> {
        self.overlay.write().unwrap_or_else(|e| e.into_inner())
    }

    /// Nombre del extra si el id es sintético.
    pub fn overlay_extra(&self, id: u32) -> Option<crate::overlay::Extra> {
        if id as usize >= self.nodes.len() {
            let ov = self.overlay();
            return ov.extra(self, id).cloned();
        }
        None
    }

    /// Busca un nodo por su FRN. Aprovecha que la MFT llega ordenada; si el
    /// índice se armó por recorrido (sin elevación) no lo está y se busca a
    /// mano. Lo usan los cambios del journal, que vienen por FRN.
    pub fn find_by_frn(&self, frn: u64) -> Option<u32> {
        if frn == 0 {
            return None;
        }
        if self.frn_sorted {
            return self
                .nodes
                .binary_search_by_key(&frn, |n| n.frn)
                .ok()
                .map(|p| p as u32);
        }
        self.nodes
            .iter()
            .position(|n| n.frn == frn)
            .map(|p| p as u32)
    }

    /// Ruta completa de la carpeta con ese FRN, si está en el índice.
    pub fn path_of_frn(&self, frn: u64) -> Option<String> {
        self.find_by_frn(frn).map(|id| self.full_path(id))
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
        let ov = self.overlay();
        if let Some(e) = ov.extra(self, id) {
            return ov.ruta(self, e);
        }
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
        let mut ix = Index {
            letter,
            root,
            roots,
            nodes,
            names: Arena::propio(names),
            stats,
            built_at,
            dir_bytes: Vec::new(),
            vol_serials: Vec::new(),
            folded: Arena::default(),
            hijos_inicio: Vec::new(),
            hijos: Vec::new(),
            por_nombre: Vec::new(),
            rango: Vec::new(),
            euler: Vec::new(),
            tin: Vec::new(),
            tout: Vec::new(),
            compacto: Vec::new(),
            frn_sorted: false,
            overlay: std::sync::Arc::new(
                std::sync::RwLock::new(crate::overlay::Overlay::default()),
            ),
        };
        ix.compute_dir_bytes();
        ix.compute_folded();
        ix.rebuild_indexes();
        ix
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
