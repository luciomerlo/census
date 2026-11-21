//! Índices auxiliares: hijos por carpeta y orden por nombre.
//!
//! Los nodos quedan en orden de FRN, que es la identidad estable (USN, snapshot
//! y `parent` dependen de eso). Lo que se precalcula son dos vistas:
//!
//! - **CSR de hijos**: listar el contenido de una carpeta sin recorrer 1,2 M de
//!   nodos. Lo usan la ventana y, por IPC, Wiz y Sort.
//! - **Orden por nombre plegado** (`por_nombre`) y el rango de cada id: ordenar
//!   por nombre deja de comparar cadenas y pasa a comparar dos `u32`.

use super::index::Index;

impl Index {
    /// Recalcula hijos y orden por nombre. Se llama al construir o cargar el
    /// índice; es la única parte que cuesta (un `sort` de 1,2 M de ids).
    pub fn rebuild_indexes(&mut self) {
        self.build_children();
        self.build_euler();
        self.build_name_order();
    }

    /// Preorden y rango de subárbol de cada carpeta. Con esto, "todo lo que
    /// cuelga de esta carpeta" es un rango contiguo de `euler`.
    fn build_euler(&mut self) {
        let n = self.nodes.len();
        let mut tin = vec![u32::MAX; n];
        let mut euler: Vec<u32> = Vec::with_capacity(n);
        let roots = self.roots.clone();
        for raiz in roots {
            let mut pila = vec![raiz];
            while let Some(id) = pila.pop() {
                let i = id as usize;
                if i >= n || tin[i] != u32::MAX {
                    continue;
                }
                tin[i] = euler.len() as u32;
                euler.push(id);
                for &h in self.children_of(id) {
                    pila.push(h);
                }
            }
        }
        // Nodos que no cuelgan de ninguna raíz (no debería pasar): al final.
        for id in 0..n as u32 {
            if tin[id as usize] == u32::MAX {
                tin[id as usize] = euler.len() as u32;
                euler.push(id);
            }
        }
        // Tamaño de cada subárbol recorriendo el preorden al revés.
        let mut tam = vec![1u32; n];
        for pos in (0..euler.len()).rev() {
            let id = euler[pos] as usize;
            let i = euler[pos] as usize;
            let desde = self.hijos_inicio.get(i).copied().unwrap_or(0) as usize;
            let hasta = self.hijos_inicio.get(i + 1).copied().unwrap_or(0) as usize;
            let mut suma = 1u32;
            for &h in &self.hijos[desde..hasta] {
                suma = suma.saturating_add(tam[h as usize]);
            }
            tam[id] = suma;
        }
        let tout: Vec<u32> = tin
            .iter()
            .enumerate()
            .map(|(i, t)| {
                if *t == u32::MAX {
                    0
                } else {
                    (*t).saturating_add(tam[i]).min(euler.len() as u32)
                }
            })
            .collect();
        self.euler = euler;
        self.tin = tin;
        self.tout = tout;
    }

    /// Ids de la carpeta y todo lo que cuelga de ella.
    pub fn subtree(&self, id: u32) -> &[u32] {
        let i = id as usize;
        let (Some(&a), Some(&b)) = (self.tin.get(i), self.tout.get(i)) else {
            return &[];
        };
        if a == u32::MAX || b < a {
            return &[];
        }
        self.euler.get(a as usize..b as usize).unwrap_or(&[])
    }

    fn build_children(&mut self) {
        let n = self.nodes.len();
        let mut cuenta = vec![0u32; n + 1];
        for (i, nodo) in self.nodes.iter().enumerate() {
            let p = nodo.parent as usize;
            if p < n && p != i {
                cuenta[p] += 1;
            }
        }
        let mut inicio = vec![0u32; n + 1];
        let mut acumulado = 0u32;
        for i in 0..n {
            inicio[i] = acumulado;
            acumulado += cuenta[i];
        }
        inicio[n] = acumulado;
        let mut relleno = inicio.clone();
        let mut hijos = vec![0u32; acumulado as usize];
        for (i, nodo) in self.nodes.iter().enumerate() {
            let p = nodo.parent as usize;
            if p < n && p != i {
                let pos = relleno[p] as usize;
                hijos[pos] = i as u32;
                relleno[p] += 1;
            }
        }
        self.hijos_inicio = inicio;
        self.hijos = hijos;
    }

    /// Ordena los ids por nombre en paralelo: cada hilo ordena su bloque y
    /// después se fusionan. Ordenar 1,8 M de nombres en un solo hilo costaba
    /// ~600 ms de los ~1 s de carga.
    fn build_name_order(&mut self) {
        let n = self.nodes.len();
        if n == 0 {
            self.por_nombre.clear();
            self.rango.clear();
            return;
        }
        let hilos = std::thread::available_parallelism()
            .map(|v| v.get())
            .unwrap_or(4)
            .clamp(1, 16);
        let bloque = n.div_ceil(hilos);
        let mut partes: Vec<Vec<u32>> = Vec::with_capacity(hilos);
        std::thread::scope(|s| {
            let mut handles = Vec::with_capacity(hilos);
            for k in 0..hilos {
                let ini = k * bloque;
                let fin = ((k + 1) * bloque).min(n);
                if ini >= fin {
                    break;
                }
                let yo = &*self;
                handles.push(s.spawn(move || {
                    let mut trozo: Vec<u32> = (ini as u32..fin as u32).collect();
                    trozo.sort_unstable_by(|a, b| yo.name_folded(*a).cmp(yo.name_folded(*b)));
                    trozo
                }));
            }
            for h in handles {
                if let Ok(v) = h.join() {
                    partes.push(v);
                }
            }
        });
        // Fusión por pares hasta quedarse con una sola lista.
        while partes.len() > 1 {
            let mut siguiente: Vec<Vec<u32>> = Vec::with_capacity(partes.len() / 2 + 1);
            let mut it = partes.into_iter();
            while let Some(a) = it.next() {
                match it.next() {
                    Some(b) => siguiente.push(self.fusionar(a, b)),
                    None => siguiente.push(a),
                }
            }
            partes = siguiente;
        }
        let orden = partes.pop().unwrap_or_default();
        let mut rango = vec![0u32; n];
        for (pos, id) in orden.iter().enumerate() {
            rango[*id as usize] = pos as u32;
        }
        self.por_nombre = orden;
        self.rango = rango;
    }

    /// Fusión de dos listas ya ordenadas por nombre.
    fn fusionar(&self, a: Vec<u32>, b: Vec<u32>) -> Vec<u32> {
        let mut out = Vec::with_capacity(a.len() + b.len());
        let (mut i, mut j) = (0usize, 0usize);
        while i < a.len() && j < b.len() {
            if self.name_folded(a[i]) <= self.name_folded(b[j]) {
                out.push(a[i]);
                i += 1;
            } else {
                out.push(b[j]);
                j += 1;
            }
        }
        out.extend_from_slice(&a[i..]);
        out.extend_from_slice(&b[j..]);
        out
    }

    /// Hijos directos de una carpeta, en el orden en que están en el índice.
    pub fn children_of(&self, id: u32) -> &[u32] {
        let i = id as usize;
        let (Some(&desde), Some(&hasta)) = (self.hijos_inicio.get(i), self.hijos_inicio.get(i + 1))
        else {
            return &[];
        };
        self.hijos
            .get(desde as usize..hasta as usize)
            .unwrap_or(&[])
    }

    /// Posición del id en el orden alfabético. Sirve para ordenar por nombre
    /// comparando enteros en vez de cadenas.
    pub fn rank_of(&self, id: u32) -> u32 {
        self.rango.get(id as usize).copied().unwrap_or(u32::MAX)
    }
}
