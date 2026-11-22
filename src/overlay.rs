//! Capa de cambios del USN Journal sobre el índice principal (C2).
//!
//! El índice es una foto inmutable. Los archivos que el journal reporta como
//! creados o renombrados van a esta capa, y los borrados quedan marcados para
//! que las consultas los descarten. Es a propósito: aplicar cien cambios tiene
//! que costar microsegundos, no los ~600 ms que cuesta reconstruir los índices
//! derivados de 1,6 M de nodos.
//!
//! Los extras se comportan como filas más del índice: se les da un id sintético
//! `ix.nodes.len() + posición`, así la ventana, el CLI y el IPC los tratan
//! igual que a un nodo sin saber que están acá.

use std::collections::{HashMap, HashSet};

use crate::index::{fold, Index};
use crate::usn::Cambio;

/// Tope de saltos al armar la ruta: con datos raros no nos colgamos.
const MAX_SALTOS: usize = 64;

/// A partir de cuántos cambios conviene reindexar en vez de acumular.
const TOPE_COMPACTAR: u64 = 50_000;

/// Un archivo o carpeta que el journal conoce y el índice no (o que renombró).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extra {
    pub frn: u64,
    /// FRN de la carpeta contenedora.
    pub padre_frn: u64,
    pub nombre: Vec<u16>,
    /// Nombre plegado: mismas reglas que la arena del índice.
    pub plegado: Vec<u16>,
    pub carpeta: bool,
}

impl Extra {
    /// Arma el extra a partir de un registro del journal.
    fn desde(c: &Cambio) -> Self {
        Extra {
            frn: c.frn,
            padre_frn: c.padre,
            nombre: c.nombre.clone(),
            plegado: c.nombre.iter().map(|u| fold(*u)).collect(),
            carpeta: c.es_carpeta(),
        }
    }

    /// ¿El nombre está plegado igual que el del índice? Es barato y evita
    /// mostrar dos veces el mismo archivo si el journal repite un registro.
    pub fn mismo_nombre(&self, otro: &Extra) -> bool {
        self.plegado == otro.plegado && self.padre_frn == otro.padre_frn
    }
}

/// Cambios aplicados desde el último snapshot.
#[derive(Clone, Debug, Default)]
pub struct Overlay {
    extras: Vec<Extra>,
    por_frn: HashMap<u64, usize>,
    borrados: HashSet<u64>,
    /// FRN que el journal cambió y que también están en la foto: la foto
    /// quedó vieja para esos archivos, manda el overlay.
    pisados: HashSet<u64>,
    /// `(journal_id, next_usn)` que ya se aplicaron.
    pub watermark: Option<(u64, i64)>,
    /// Cuántos cambios se aplicaron desde la última compactación.
    pub aplicados: u64,
}

impl Overlay {
    /// Overlay vacío que arranca desde un marcador del journal.
    pub fn desde_watermark(journal_id: u64, next_usn: i64) -> Self {
        Overlay {
            watermark: Some((journal_id, next_usn)),
            ..Overlay::default()
        }
    }

    pub fn len(&self) -> usize {
        self.extras.len()
    }

    pub fn is_empty(&self) -> bool {
        self.extras.is_empty() && self.borrados.is_empty() && self.pisados.is_empty()
    }

    /// Aplica los cambios en orden y devuelve cuántos alteraron el estado.
    ///
    /// Un mismo FRN puede venir varias veces (crear, escribir, cerrar, borrar):
    /// gana siempre el último estado, que es lo que hace este recorrido.
    pub fn aplicar(&mut self, ix: &Index, cambios: &[Cambio]) -> usize {
        let mut alterados = 0usize;
        for c in cambios {
            if c.frn == 0 {
                continue;
            }
            if c.es_borrado() {
                self.sacar(c.frn);
                if self.borrados.insert(c.frn) {
                    alterados += 1;
                }
                self.aplicados += 1;
                continue;
            }
            let relevante = c.es_creacion() || c.es_renombre() || c.es_carpeta();
            if !relevante || c.nombre.is_empty() {
                continue;
            }
            if es_punto(&c.nombre) {
                continue;
            }
            let extra = Extra::desde(c);
            match self.por_frn.get(&c.frn).copied() {
                Some(pos) => {
                    if self.extras[pos] != extra {
                        self.extras[pos] = extra;
                        alterados += 1;
                    }
                }
                None => {
                    self.extras.push(extra);
                    self.por_frn.insert(c.frn, self.extras.len() - 1);
                    alterados += 1;
                }
            }
            self.borrados.remove(&c.frn);
            // Si la foto ya tenía ese FRN, hay que ocultarlo: Windows reusa
            // números de registro, así que un archivo nuevo puede heredar el
            // del viejo y aparecería dos veces.
            if ix.find_by_frn(c.frn).is_some() {
                self.pisados.insert(c.frn);
            }
            self.aplicados += 1;
        }
        alterados
    }

    /// Saca un FRN de la lista de vivos y reindexa las posiciones.
    fn sacar(&mut self, frn: u64) {
        if let Some(pos) = self.por_frn.remove(&frn) {
            self.extras.remove(pos);
            // Las posiciones siguientes corren un lugar: se rearma el mapa.
            self.por_frn.clear();
            for (i, e) in self.extras.iter().enumerate() {
                self.por_frn.insert(e.frn, i);
            }
        }
    }

    /// Extra por id sintético (`ix.nodes.len() + posición`).
    pub fn extra(&self, ix: &Index, id: u32) -> Option<&Extra> {
        self.extras
            .get(id.checked_sub(ix.nodes.len() as u32)? as usize)
    }

    /// Todos los ids sintéticos, en orden estable.
    pub fn ids(&self, ix: &Index) -> Vec<u32> {
        let base = ix.nodes.len() as u32;
        (0..self.extras.len() as u32).map(|k| base + k).collect()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Extra> {
        self.extras.iter()
    }

    /// ¿Este id del índice principal quedó viejo?
    ///
    /// Pasa por dos motivos: el archivo se borró, o el journal lo cambió y la
    /// foto ya no es cierta para ese FRN (ahí manda el overlay).
    pub fn id_borrado(&self, ix: &Index, id: u32) -> bool {
        if self.borrados.is_empty() && self.pisados.is_empty() {
            return false;
        }
        ix.nodes
            .get(id as usize)
            .is_some_and(|n| self.borrados.contains(&n.frn) || self.pisados.contains(&n.frn))
    }

    pub fn nombre_string(&self, e: &Extra) -> String {
        String::from_utf16_lossy(&e.nombre)
    }

    /// Ruta completa estilo Windows, subiendo por los FRN de los padres.
    ///
    /// Si el padre también vino del journal, se sigue por acá; si está en el
    /// índice principal, la parte de arriba la resuelve `Index::path_of_frn`.
    pub fn ruta(&self, ix: &Index, e: &Extra) -> String {
        let mut partes: Vec<String> = vec![self.nombre_string(e)];
        let mut padre = e.padre_frn;
        let mut saltos = 0;
        while padre != 0 && saltos < MAX_SALTOS {
            if let Some(pos) = self.por_frn.get(&padre) {
                let p = &self.extras[*pos];
                partes.push(self.nombre_string(p));
                padre = p.padre_frn;
                saltos += 1;
                continue;
            }
            if let Some(base) = ix.path_of_frn(padre) {
                partes.push(base);
                return unir(&partes);
            }
            break;
        }
        unir(&partes)
    }

    pub fn limpiar(&mut self) {
        self.extras.clear();
        self.por_frn.clear();
        self.borrados.clear();
        self.pisados.clear();
        self.aplicados = 0;
    }

    /// La decisión de reindexar la toma el llamador; acá sólo se avisa.
    pub fn compactar_si_hace_falta(&self) -> bool {
        self.compactar_con_tope(TOPE_COMPACTAR)
    }

    /// Igual que [`Overlay::compactar_si_hace_falta`] pero con otro tope, para
    /// poder probarlo sin aplicar cincuenta mil cambios.
    pub fn compactar_con_tope(&self, tope: u64) -> bool {
        self.aplicados > tope
    }

    /// Deja el overlay vacío conservando el watermark: se usa después de
    /// reindexar, cuando la foto ya incluye todo lo que había acá.
    pub fn vaciar_conservando_watermark(&mut self) {
        let marca = self.watermark;
        self.limpiar();
        self.watermark = marca;
    }
}

/// Une las partes de la ruta de la más profunda a la raíz.
fn unir(partes: &[String]) -> String {
    let mut s = String::new();
    for (i, p) in partes.iter().rev().enumerate() {
        if i > 0 && !p.ends_with('\\') {
            s.push('\\');
        }
        s.push_str(p);
    }
    s
}

fn es_punto(nombre: &[u16]) -> bool {
    nombre.len() == 1 && nombre[0] == u16::from(b'.')
        || nombre.len() == 2 && nombre[0] == u16::from(b'.') && nombre[1] == u16::from(b'.')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Index, Node, F_DIR};

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    fn cambio(frn: u64, padre: u64, motivo: u32, nombre: &str, carpeta: bool) -> Cambio {
        Cambio {
            frn,
            padre,
            nombre: utf16(nombre),
            motivo,
            attrs: if carpeta { 0x10 } else { 0x20 },
        }
    }

    /// Índice mínimo: raíz `C:` (id 0) y nada más.
    fn ix_chico() -> Index {
        let nombres = utf16("C:");
        let n = nombres.len() as u16;
        Index {
            nodes: vec![Node {
                name_off: 0,
                name_len: n,
                flags: F_DIR | crate::index::F_ROOT,
                parent: 0,
                size: 0,
                mtime: 0,
                atime: 0,
                attrs: 0,
                frn: 5,
            }],
            names: nombres,
            roots: vec![0],
            ..Index::default()
        }
    }

    #[test]
    fn crear_da_id_sintetico() {
        let ix = ix_chico();
        let mut ov = Overlay::default();
        let n = ov.aplicar(
            &ix,
            &[cambio(
                100,
                5,
                crate::usn::REASON_CREATE,
                "informe.txt",
                false,
            )],
        );
        assert_eq!(n, 1);
        let ids = ov.ids(&ix);
        assert_eq!(ids, vec![ix.nodes.len() as u32]);
        let e = ov.extra(&ix, ids[0]).unwrap();
        assert_eq!(ov.nombre_string(e), "informe.txt");
        assert_eq!(e.plegado, utf16("informe.txt"));
        assert!(!e.carpeta);
    }

    #[test]
    fn borrar_marca_y_saca_de_vivos() {
        let ix = ix_chico();
        let mut ov = Overlay::default();
        ov.aplicar(
            &ix,
            &[cambio(100, 5, crate::usn::REASON_CREATE, "a.txt", false)],
        );
        ov.aplicar(
            &ix,
            &[cambio(100, 5, crate::usn::REASON_DELETE, "a.txt", false)],
        );
        assert_eq!(ov.len(), 0, "el borrado no deja vivos");
        assert!(ov.borrados.contains(&100));
        // La raíz del índice tiene otro FRN: no la marca.
        assert!(!ov.id_borrado(&ix, 0));
    }

    #[test]
    fn renombre_no_duplica() {
        let ix = ix_chico();
        let mut ov = Overlay::default();
        ov.aplicar(
            &ix,
            &[cambio(100, 5, crate::usn::REASON_CREATE, "a.txt", false)],
        );
        ov.aplicar(
            &ix,
            &[cambio(
                100,
                5,
                crate::usn::REASON_RENAME_NEW,
                "b.txt",
                false,
            )],
        );
        assert_eq!(ov.len(), 1);
        let e = ov.extra(&ix, ix.nodes.len() as u32).unwrap();
        assert_eq!(ov.nombre_string(e), "b.txt");
    }

    #[test]
    fn crear_y_borrar_el_mismo_frn_queda_borrado() {
        let ix = ix_chico();
        let mut ov = Overlay::default();
        ov.aplicar(&ix, &[cambio(7, 5, crate::usn::REASON_CREATE, "x", false)]);
        ov.aplicar(&ix, &[cambio(7, 5, crate::usn::REASON_DELETE, "x", false)]);
        assert_eq!(ov.len(), 0);
        assert!(ov.borrados.contains(&7));
        assert!(ov.extra(&ix, ix.nodes.len() as u32).is_none());
    }

    #[test]
    fn ruta_arma_la_jerarquia_del_overlay() {
        let ix = ix_chico();
        let mut ov = Overlay::default();
        ov.aplicar(
            &ix,
            &[
                cambio(20, 5, crate::usn::REASON_CREATE, "carpeta", true),
                cambio(21, 20, crate::usn::REASON_CREATE, "archivo.txt", false),
            ],
        );
        let e = ov.extra(&ix, ix.nodes.len() as u32 + 1).unwrap();
        let ruta = ov.ruta(&ix, e);
        assert!(ruta.ends_with("carpeta\\archivo.txt"), "ruta: {ruta}");
        assert!(ruta.starts_with("C:"), "ruta: {ruta}");
    }

    #[test]
    fn ignora_punto_y_punto_punto() {
        let ix = ix_chico();
        let mut ov = Overlay::default();
        let n = ov.aplicar(
            &ix,
            &[
                cambio(30, 5, crate::usn::REASON_CREATE, ".", true),
                cambio(31, 5, crate::usn::REASON_CREATE, "..", true),
            ],
        );
        assert_eq!(n, 0);
        assert!(ov.is_empty());
    }

    #[test]
    fn compactar_avisa_despues_del_tope() {
        let mut ov = Overlay {
            aplicados: TOPE_COMPACTAR,
            ..Overlay::default()
        };
        assert!(!ov.compactar_si_hace_falta());
        ov.aplicados += 1;
        assert!(ov.compactar_si_hace_falta());
        assert!(ov.compactar_con_tope(2));
    }
}
