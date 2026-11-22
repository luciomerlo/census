//! Mapa de sólo lectura del snapshot `C.wcat`, sobre `memmap2`.
//!
//! Es la pieza que permite cargar el snapshot sin copiar las dos arenas
//! grandes (`names` y `folded`, ~156 MB juntas con un índice de 1,6 M de
//! nodos): el archivo se mapea y las arenas se leen directo del mapa.
//!
//! `Mapa` es `Send + Sync` y el índice lo retiene a través de un `Arc` (ver
//! `crate::index::Arena`), así que la memoria mapeada vive exactamente lo que
//! vive el índice: nadie puede quedarse con una vista colgada.

use std::fs::File;
use std::io;
use std::sync::Arc;

use memmap2::Mmap;

/// Archivo mapeado en sólo lectura, con vistas tipadas sobre rangos alineados.
pub struct Mapa {
    mmap: Mmap,
}

impl std::fmt::Debug for Mapa {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Mapa")
            .field("len", &self.mmap.len())
            .finish()
    }
}

impl Mapa {
    /// Mapea el archivo completo en sólo lectura.
    ///
    /// El archivo no debe truncarse ni reescribirse mientras viva el mapa; el
    /// snapshot se reemplaza por `rename`, así que el inodo viejo sigue entero
    /// hasta que cae el último `Arc`.
    pub fn abrir(archivo: &File) -> io::Result<Arc<Self>> {
        // SAFETY: `memmap2::Mmap::map` es `unsafe` por contrato de la crate (el
        // contenido puede cambiar bajo nuestros pies si otro proceso escribe
        // el archivo). Acá el archivo se abre sólo lectura y el snapshot se
        // publica con `save` vía `.tmp` + `rename`, así que el inodo mapeado no
        // se modifica ni se trunca mientras viva el mapa.
        let mmap = unsafe { Mmap::map(archivo)? };
        Ok(Arc::new(Mapa { mmap }))
    }

    /// Bytes completos del mapa (viven tanto como el `Mapa`).
    pub fn bytes(&self) -> &[u8] {
        &self.mmap
    }

    /// Largo del archivo mapeado en bytes.
    pub fn len(&self) -> usize {
        self.mmap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.mmap.is_empty()
    }

    /// Vista `&'static [u16]` sobre `count` valores que empiezan en `offset`.
    ///
    /// Devuelve `None` si el rango no está alineado a 2 bytes, se sale del
    /// mapa o hay desborde. El `'static` es deliberado: el llamador **debe**
    /// retener este `Mapa` (con un `Arc`) mientras use la vista; es lo que
    /// hace `crate::index::Arena::mapeado`.
    pub fn u16s(&self, offset: usize, count: usize) -> Option<&'static [u16]> {
        if !offset.is_multiple_of(std::mem::align_of::<u16>()) {
            return None;
        }
        let bytes = count.checked_mul(2)?;
        let end = offset.checked_add(bytes)?;
        if end > self.mmap.len() {
            return None;
        }
        let ptr = self.mmap.as_ptr().wrapping_add(offset).cast::<u16>();
        // SAFETY: `offset` es múltiplo de 2 y el mapa arranca en un límite de
        // página, así que `ptr` está alineado para `u16`; `end <= len` cubre
        // los `count` valores del rango. El mapeo es de sólo lectura y no se
        // libera mientras viva el `Arc<Mapa>` que la arena retiene, de modo
        // que el `'static` se cumple mientras el índice esté vivo.
        Some(unsafe { std::slice::from_raw_parts(ptr, count) })
    }
}
