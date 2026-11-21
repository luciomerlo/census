//! Tamaños recursivos por carpeta.
//!
//! El índice guarda el tamaño propio de cada archivo; las carpetas necesitan
//! la suma de todo lo que cuelga de ellas para que la vista se parezca a la de
//! WizTree (barra de uso, treemap y columna Tamaño con sentido en carpetas).
//! Se calcula una vez, en una pasada sobre los nodos ya ordenados por
//! profundidad.

use super::index::{Index, F_DIR, F_ROOT};

impl Index {
    /// Rellena `dir_bytes` (tamaño recursivo de cada carpeta).
    pub fn compute_dir_bytes(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            self.dir_bytes.clear();
            return;
        }
        // Profundidad de cada nodo, memoizada subiendo por la cadena de padres.
        // `u32::MAX` marca "sin calcular" para no confundir con las raíces.
        let mut depth: Vec<u32> = vec![u32::MAX; n];
        let mut max_depth = 0u32;
        for i in 0..n {
            if depth[i] != u32::MAX {
                continue;
            }
            let mut cadena: Vec<u32> = Vec::with_capacity(16);
            let mut cur = i as u32;
            let mut d;
            loop {
                if depth[cur as usize] != u32::MAX {
                    // `cur` ya tiene profundidad conocida y es el padre del
                    // último nodo de la cadena: ese hijo está un nivel más abajo.
                    d = depth[cur as usize] + 1;
                    break;
                }
                cadena.push(cur);
                let p = self.nodes[cur as usize].parent;
                if p == cur || p as usize >= n {
                    d = 0;
                    break;
                }
                cur = p;
            }
            // La cadena queda de hoja a ancestro: se completa al revés.
            for id in cadena.into_iter().rev() {
                depth[id as usize] = d;
                d += 1;
            }
        }
        for &d in &depth {
            if d != u32::MAX && d > max_depth {
                max_depth = d;
            }
        }

        let mut bytes: Vec<u64> = self
            .nodes
            .iter()
            .map(|node| {
                if node.flags & F_DIR != 0 {
                    0
                } else {
                    node.size
                }
            })
            .collect();
        // Cubetas por profundidad: de la más profunda a la raíz, así cuando una
        // carpeta suma a su padre ya tiene el total de sus hijos.
        let mut cubetas: Vec<Vec<u32>> = vec![Vec::new(); max_depth as usize + 1];
        for (i, &d) in depth.iter().enumerate() {
            if d != u32::MAX && (d as usize) < cubetas.len() {
                cubetas[d as usize].push(i as u32);
            }
        }
        for nivel in (0..cubetas.len()).rev() {
            for &id in &cubetas[nivel] {
                let p = self.nodes[id as usize].parent;
                if p != id && (p as usize) < n {
                    let suma = bytes[id as usize];
                    bytes[p as usize] = bytes[p as usize].saturating_add(suma);
                }
            }
        }
        self.dir_bytes = bytes;
    }

    /// Tamaño de un archivo, o el recursivo si es carpeta.
    pub fn recursive_size(&self, id: u32) -> u64 {
        match self.dir_bytes.get(id as usize) {
            Some(&b) => b,
            None => self.nodes.get(id as usize).map(|n| n.size).unwrap_or(0),
        }
    }
}

/// Identidad NTFS de cada nodo: número de serie del volumen + FRN.
impl Index {
    /// Completa `vol_serials` pidiendo el número de serie de cada unidad.
    /// No toca la MFT ni necesita elevación (es `GetVolumeInformationW`).
    #[cfg(windows)]
    pub fn fill_serials(&mut self) {
        self.vol_serials = self
            .roots
            .iter()
            .map(|&r| crate::volumes::volume_serial(self.letter_of(r)).unwrap_or(0))
            .collect();
    }

    /// Sin Win32 (tests sin unidad real) no hay número de serie.
    #[cfg(not(windows))]
    pub fn fill_serials(&mut self) {
        self.vol_serials = vec![0; self.roots.len()];
    }

    /// Raíz de la que cuelga un nodo.
    pub fn root_of(&self, id: u32) -> u32 {
        let mut cur = id;
        for _ in 0..=MAX_PARENT_HOPS {
            let Some(n) = self.nodes.get(cur as usize) else {
                return cur;
            };
            if n.flags & F_ROOT != 0 || n.parent == cur {
                return cur;
            }
            cur = n.parent;
        }
        cur
    }

    /// `(vol_serial, frn)` del nodo, o `None` si el volumen no tiene serie.
    pub fn identity_of(&self, id: u32) -> Option<(u64, u64)> {
        let frn = self.nodes.get(id as usize)?.frn;
        if frn == 0 {
            return None;
        }
        let root = self.root_of(id);
        let pos = self.roots.iter().position(|&r| r == root)?;
        let serial = *self.vol_serials.get(pos)?;
        if serial == 0 {
            return None;
        }
        Some((serial, frn))
    }

    /// ¿Hay identidad disponible para anunciarla por IPC?
    pub fn has_identity(&self) -> bool {
        self.vol_serials.iter().any(|&s| s != 0)
    }
}

/// Tope de saltos al subir por los padres (una ruta real no llega ni cerca).
const MAX_PARENT_HOPS: u32 = 256;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Node;

    /// Índice mínimo: C:\ (0) → Windows (1) → System32 (2) → a.sys (3), b.dll (4)
    fn chico() -> Index {
        let names: Vec<u16> = ["C", "Windows", "System32", "a.sys", "b.dll"]
            .iter()
            .flat_map(|n| n.encode_utf16().chain(std::iter::once(0)))
            .collect();
        let largo = |s: &str| s.encode_utf16().count() as u16;
        let off = |s: &str| -> u32 {
            let mut o = 0u32;
            for prev in ["C", "Windows", "System32", "a.sys"] {
                if prev == s {
                    break;
                }
                o += (largo(prev) + 1) as u32;
            }
            o
        };
        let dir = |name: &str, parent: u32| Node {
            name_off: off(name),
            name_len: largo(name),
            flags: F_DIR,
            parent,
            size: 0,
            ..Default::default()
        };
        let file = |name: &str, parent: u32, size: u64| Node {
            name_off: off(name),
            name_len: largo(name),
            flags: 0,
            parent,
            size,
            ..Default::default()
        };
        Index {
            letter: 'C',
            root: 0,
            roots: vec![0],
            nodes: vec![
                dir("C", 0),
                dir("Windows", 0),
                dir("System32", 1),
                file("a.sys", 2, 100),
                file("b.dll", 2, 50),
            ],
            names,
            ..Default::default()
        }
    }

    #[test]
    fn sums_children_up_to_the_root() {
        let mut ix = chico();
        ix.compute_dir_bytes();
        assert_eq!(ix.recursive_size(3), 100);
        assert_eq!(ix.recursive_size(4), 50);
        assert_eq!(ix.recursive_size(2), 150);
        assert_eq!(ix.recursive_size(1), 150);
        assert_eq!(ix.recursive_size(0), 150);
    }

    #[test]
    fn empty_index_does_not_panic() {
        let mut ix = Index::default();
        ix.compute_dir_bytes();
        assert!(ix.dir_bytes.is_empty());
        assert_eq!(ix.recursive_size(0), 0);
    }

    #[test]
    fn counted_twice_is_harmless() {
        let mut ix = chico();
        ix.compute_dir_bytes();
        let primera = ix.recursive_size(0);
        ix.compute_dir_bytes();
        assert_eq!(ix.recursive_size(0), primera, "recalcular no duplica");
    }
}
