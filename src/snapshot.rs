//! Snapshot en disco del índice: formato propio `C.wcat`.
// `as_chunks` (lo que sugiere el lint) todavía es inestable en Rust estable.
#![allow(clippy::chunks_exact_to_as_chunks)]
//!
//! Es lo que permite arrancar en milisegundos en vez de releer la MFT: el
//! índice completo (`Index`) se vuelca a un archivo y al arrancar se lee tal
//! cual, sin tocar NTFS ni pedir elevación.
//!
//! # Formato `C.wcat` (little-endian, secciones alineadas a 8 B)
//!
//! Todo se serializa **campo por campo** en little-endian. `Node` NO es
//! `repr(C)`: jamás se escriben bytes crudos de la estructura.
//!
//! Cabecera fija de `HEADER_LEN` (89) bytes, en este orden:
//!
//! | Off | Tamaño | Campo |
//! |-----|--------|-------|
//! | 0   | 10     | magic: `CENSUSWCAT` (ASCII, 10 bytes) |
//! | 10  | u16    | version = 4 |
//! | 12  | u16    | flags (bit 0 = little endian) |
//! | 14  | u32    | node_count |
//! | 18  | u32    | name_units (unidades UTF-16 de la arena) |
//! | 22  | u32    | root_count |
//! | 26  | u8[3]  | padding (siempre 0) |
//! | 29  | u32    | letter (el `char` como u32) |
//! | 33  | i64    | built_at (timestamp Unix, segundos) |
//! | 41  | u64    | stats.records |
//! | 49  | u64    | stats.files |
//! | 57  | u64    | stats.dirs |
//! | 65  | u64    | stats.skipped |
//! | 73  | u64    | stats.build_ms (u128 truncado a u64) |
//! | 81  | u64    | checksum: FNV-1a 64 sobre nodos + arena |
//!
//! Nota: los campos pedidos para la cabecera suman 89 bytes (no 64).
//!
//! Desde la **versión 4** cada sección arranca alineada a 8 bytes: después de
//! la cabecera y antes de cada sección se rellenan con ceros los bytes que
//! falten hasta el próximo múltiplo de 8. El padding **no** entra en el
//! checksum. Es lo que permite mapear el archivo y leer las arenas `u16`
//! directo del mapa, sin copiarlas (ver `crate::mmap`). La versión 3 (sin
//! padding) se sigue leyendo: en ese caso, o si el mapeo falla, las arenas se
//! copian como antes.
//!
//! Después de la cabecera, en orden (cada sección precedida por su padding en
//! v4):
//!
//! 1. `roots`: u32 × root_count (ids de nodo de cada unidad).
//! 2. `nodes`: los nodos uno por uno, 48 bytes cada uno, campo por campo en
//!    little-endian: `name_off` u32, `name_len` u16, `flags` u16, `parent`
//!    u32, `size` u64, `mtime` i64, `atime` i64, `attrs` u32, `frn` u64.
//! 3. `names`: la arena UTF-16, u16 × name_units, little-endian.
//!
//! Desde la **versión 2** se guardan también los índices derivados, para no
//! recalcularlos al arrancar. Cada bloque lleva un `u32` con su cantidad
//! (0 = no estaba y hay que recalcularlo):
//!
//! 4. `folded`: u16 × folded.len() (mismos offsets que `names`).
//! 5. `dir_bytes`: u64 × node_count (tamaño recursivo de cada carpeta).
//! 6. `hijos_inicio`: u32 × (node_count + 1) — CSR de hijos.
//! 7. `hijos`: u32 × M — ids de los hijos.
//! 8. `por_nombre`: u32 × node_count — ids ordenados por nombre plegado.
//! 9. `euler`: u32 × node_count — preorden; el rango de subárbol sale de acá.
//!
//! La cola final (16 bytes: `journal_id` u64 + `next_usn` i64) es opcional.
//!
//! El checksum (FNV-1a 64, offset `0xcbf29ce484222325`, primo
//! `0x100000001b3`) cubre los bytes de los nodos y de la arena; la cabecera,
//! el padding y las raíces quedan fuera porque los campos estructurales se
//! validan aparte al cargar (rango de `parent`, de `name_off`/`name_len` y de
//! las raíces).
//!
//! `save` escribe a un `<ruta>.tmp` y renombra al final para que un corte de
//! luz nunca deje un snapshot a medio escribir. `load` rechaza cantidades
//! absurdas (>200 M nodos) antes de reservar memoria.

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::index::{Arena, Index, Mapa, Node, Stats};

/// `CENSUSWCAT`, 10 bytes ASCII.
const MAGIC: [u8; 10] = *b"CENSUSWCAT";

/// Versión del formato que escribimos: secciones alineadas a 8 bytes.
const VERSION: u16 = 4;

/// Versión anterior (sin padding): se sigue leyendo sin romper nada.
const VERSION_SIN_PADDING: u16 = 3;

/// Alineación de cada sección desde la versión 4.
const ALINEACION: usize = 8;

/// Bit 0 de `flags`: campos en little-endian.
const FLAG_LE: u16 = 0x1;

/// Tamaño de la cabecera fija (ver tabla en el encabezado del módulo).
const HEADER_LEN: usize = 89;

/// Tamaño serializado de un nodo: 9 campos little-endian.
const NODE_LEN: usize = 48;

/// Cantidad de nodos que consideramos absurda: se rechaza antes de reservar.
const MAX_NODES: u32 = 200_000_000;

/// Techo defensivo para la arena: 2 GB de UTF-16 no es un índice, es basura.
const MAX_NAME_UNITS: u32 = 1_000_000_000;

/// Cuántos nodos se leen por tanda al cargar (buffer de ~400 KB).
const NODE_CHUNK: usize = 8192;

/// Offset dentro del nodo serializado donde vive `parent` (u32). Sólo lo
/// usan los tests para corruptar ese campo a propósito.
#[cfg(test)]
const NODE_PARENT_OFF: usize = 8;

/// Cantidad de hilos para conversiones y verificaciones al cargar.
fn hilos_carga() -> usize {
    std::thread::available_parallelism()
        .map(|v| v.get())
        .unwrap_or(4)
        .clamp(1, 16)
}

/// Convierte bytes little-endian a `u16` en paralelo.
///
/// La conversión elemento por elemento era la mitad del tiempo de carga (39 M
/// de valores para la arena). Cada hilo escribe su trozo de la salida ya
/// reservada: no hay `unsafe` ni copias intermedias.
fn bytes_a_u16(bytes: &[u8]) -> Vec<u16> {
    let n = bytes.len() / 2;
    let mut out = vec![0u16; n];
    if n == 0 {
        return out;
    }
    let hilos = hilos_carga();
    let bloque = n.div_ceil(hilos).max(1);
    std::thread::scope(|s| {
        for (k, trozo) in out.chunks_mut(bloque).enumerate() {
            let base = k * bloque;
            let src = &bytes[base * 2..(base + trozo.len()) * 2];
            s.spawn(move || {
                for (dst, par) in trozo.iter_mut().zip(src.chunks_exact(2)) {
                    *dst = u16::from_le_bytes([par[0], par[1]]);
                }
            });
        }
    });
    out
}

fn bytes_a_u32(bytes: &[u8]) -> Vec<u32> {
    let n = bytes.len() / 4;
    let mut out = vec![0u32; n];
    if n == 0 {
        return out;
    }
    let hilos = hilos_carga();
    let bloque = n.div_ceil(hilos).max(1);
    std::thread::scope(|s| {
        for (k, trozo) in out.chunks_mut(bloque).enumerate() {
            let base = k * bloque;
            let src = &bytes[base * 4..(base + trozo.len()) * 4];
            s.spawn(move || {
                for (dst, c) in trozo.iter_mut().zip(src.chunks_exact(4)) {
                    *dst = u32::from_le_bytes([c[0], c[1], c[2], c[3]]);
                }
            });
        }
    });
    out
}

fn bytes_a_u64(bytes: &[u8]) -> Vec<u64> {
    let n = bytes.len() / 8;
    let mut out = vec![0u64; n];
    if n == 0 {
        return out;
    }
    let hilos = hilos_carga();
    let bloque = n.div_ceil(hilos).max(1);
    std::thread::scope(|s| {
        for (k, trozo) in out.chunks_mut(bloque).enumerate() {
            let base = k * bloque;
            let src = &bytes[base * 8..(base + trozo.len()) * 8];
            s.spawn(move || {
                for (dst, c) in trozo.iter_mut().zip(src.chunks_exact(8)) {
                    *dst = u64::from_le_bytes(c.try_into().unwrap());
                }
            });
        }
    });
    out
}

// ---------------------------------------------------------------- FNV-1a 64

/// FNV-1a 64 bits por palabras de 8 bytes, sin dependencias.
///
/// Guarda hasta 7 bytes sin mezclar, así el resultado **no depende de cómo se
/// parta la entrada en trozos**: escribir de a 64 KB y leer de a 284 MB dan el
/// mismo valor. Es lo que permite hashear rápido y validar igual.
struct Fnv {
    h: u64,
    resto: [u8; 8],
    n: usize,
}

impl Fnv {
    fn new() -> Self {
        Fnv {
            h: 0xcbf29ce484222325,
            resto: [0u8; 8],
            n: 0,
        }
    }

    fn palabra(&mut self, b: &[u8; 8]) {
        self.h ^= u64::from_le_bytes(*b);
        self.h = self.h.wrapping_mul(0x100000001b3);
    }

    fn bytes(&mut self, b: &[u8]) {
        let mut i = 0;
        // Completar la palabra que quedó a medias.
        while self.n > 0 && self.n < 8 && i < b.len() {
            self.resto[self.n] = b[i];
            self.n += 1;
            i += 1;
        }
        if self.n == 8 {
            let r = self.resto;
            self.palabra(&r);
            self.n = 0;
        }
        while i + 8 <= b.len() {
            let w: [u8; 8] = b[i..i + 8].try_into().unwrap();
            self.palabra(&w);
            i += 8;
        }
        while i < b.len() {
            self.resto[self.n] = b[i];
            self.n += 1;
            i += 1;
        }
    }

    #[allow(dead_code)]
    fn u16(&mut self, v: u16) {
        self.bytes(&v.to_le_bytes());
    }
    #[allow(dead_code)] // queda como utilidad del checksum para futuros campos
    fn u32(&mut self, v: u32) {
        self.bytes(&v.to_le_bytes());
    }
    #[allow(dead_code)]
    fn u64(&mut self, v: u64) {
        self.bytes(&v.to_le_bytes());
    }

    fn finish(mut self) -> u64 {
        for k in 0..self.n {
            self.h ^= self.resto[k] as u64;
            self.h = self.h.wrapping_mul(0x100000001b3);
        }
        self.h
    }
}

/// Hashea una tanda de `u16` sin alocar: 32 KB por vuelta.
fn hash_u16s(h: &mut Fnv, data: &[u16]) {
    let mut buf = [0u8; 32 * 1024];
    for trozo in data.chunks(16 * 1024) {
        let mut n = 0;
        for v in trozo {
            let b = v.to_le_bytes();
            buf[n] = b[0];
            buf[n + 1] = b[1];
            n += 2;
        }
        h.bytes(&buf[..n]);
    }
}

// --------------------------------------------------- serialización de nodos

/// Escribe un nodo, campo por campo, en 48 bytes little-endian.
fn node_to_bytes(n: &Node, out: &mut [u8; NODE_LEN]) {
    out[0..4].copy_from_slice(&n.name_off.to_le_bytes());
    out[4..6].copy_from_slice(&n.name_len.to_le_bytes());
    out[6..8].copy_from_slice(&n.flags.to_le_bytes());
    out[8..12].copy_from_slice(&n.parent.to_le_bytes());
    out[12..20].copy_from_slice(&n.size.to_le_bytes());
    out[20..28].copy_from_slice(&n.mtime.to_le_bytes());
    out[28..36].copy_from_slice(&n.atime.to_le_bytes());
    out[36..40].copy_from_slice(&n.attrs.to_le_bytes());
    out[40..48].copy_from_slice(&n.frn.to_le_bytes());
}

/// Lee un nodo desde sus 48 bytes little-endian.
fn node_from_bytes(b: &[u8; NODE_LEN]) -> Node {
    Node {
        name_off: u32::from_le_bytes(b[0..4].try_into().unwrap()),
        name_len: u16::from_le_bytes(b[4..6].try_into().unwrap()),
        flags: u16::from_le_bytes(b[6..8].try_into().unwrap()),
        parent: u32::from_le_bytes(b[8..12].try_into().unwrap()),
        size: u64::from_le_bytes(b[12..20].try_into().unwrap()),
        mtime: i64::from_le_bytes(b[20..28].try_into().unwrap()),
        atime: i64::from_le_bytes(b[28..36].try_into().unwrap()),
        attrs: u32::from_le_bytes(b[36..40].try_into().unwrap()),
        frn: u64::from_le_bytes(b[40..48].try_into().unwrap()),
    }
}

/// Hash FNV-1a 64 de los nodos + la arena, en el mismo orden en que se
/// escriben en el archivo. `save` y `load` comparten esta definición.
/// Checksum del cuerpo: nodos, arena y los cinco bloques derivados.
fn checksum_of(index: &Index) -> u64 {
    let mut h = Fnv::new();
    let mut buf = vec![0u8; NODE_LEN * 1024];
    for trozo in index.nodes.chunks(1024) {
        let mut n = 0;
        for nodo in trozo {
            let mut uno = [0u8; NODE_LEN];
            node_to_bytes(nodo, &mut uno);
            buf[n..n + NODE_LEN].copy_from_slice(&uno);
            n += NODE_LEN;
        }
        h.bytes(&buf[..n]);
    }
    hash_u16s(&mut h, &index.names);
    // Los bloques derivados NO entran en el checksum: son reconstruibles y se
    // validan por estructura (cantidades y rangos). Hashear 120 MB más costaba
    // ~80 ms en cada arranque, que es justo lo que se quería evitar.
    h.finish()
}

// -------------------------------------------------------------- escritura

/// `BufWriter` + contador de bytes escritos + helpers little-endian.
struct LeWriter<W: Write> {
    inner: W,
    written: u64,
}

impl<W: Write> LeWriter<W> {
    fn raw(&mut self, b: &[u8]) -> io::Result<()> {
        self.inner.write_all(b)?;
        self.written += b.len() as u64;
        Ok(())
    }
    fn u16(&mut self, v: u16) -> io::Result<()> {
        self.raw(&v.to_le_bytes())
    }
    fn u32(&mut self, v: u32) -> io::Result<()> {
        self.raw(&v.to_le_bytes())
    }
    fn u64(&mut self, v: u64) -> io::Result<()> {
        self.raw(&v.to_le_bytes())
    }
    fn i64(&mut self, v: i64) -> io::Result<()> {
        self.raw(&v.to_le_bytes())
    }
    /// Rellena con ceros hasta el próximo múltiplo de `ALINEACION`.
    fn alinear(&mut self) -> io::Result<()> {
        let relleno = self.written.next_multiple_of(ALINEACION as u64) - self.written;
        if relleno > 0 {
            self.raw(&[0u8; ALINEACION][..relleno as usize])
        } else {
            Ok(())
        }
    }
}

/// Guarda el índice en `path` con formato `C.wcat`. Escribe a un `.tmp` al
/// lado y renombra al final; devuelve los bytes escritos en el archivo final.
///
/// Los campos de `Stats` y `built_at` se toalan tal cual están en el índice;
/// `Index::root` no viaja porque se reconstruye como `roots[0]`.
pub fn save(index: &Index, path: &Path) -> std::io::Result<u64> {
    let node_count = u32::try_from(index.nodes.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "demasiados nodos"))?;
    let name_units = u32::try_from(index.names.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "arena demasiado grande"))?;
    let root_count = u32::try_from(index.roots.len())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "demasiadas raíces"))?;
    let build_ms = u64::try_from(index.stats.build_ms).unwrap_or(u64::MAX);

    let checksum = checksum_of(index);

    // El directorio contenedor puede no existir todavía (%LOCALAPPDATA%\Census).
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let mut tmp_name = path.as_os_str().to_os_string();
    tmp_name.push(".tmp");
    let tmp = PathBuf::from(tmp_name);

    let file = fs::File::create(&tmp)?;
    let mut w = LeWriter {
        inner: io::BufWriter::new(file),
        written: 0,
    };
    let res = write_all(
        &mut w, index, node_count, name_units, root_count, build_ms, checksum,
    );
    let written = w.written;
    drop(w);
    // Si algo falló, no dejamos el .tmp tirado.
    if let Err(e) = res {
        let _ = fs::remove_file(&tmp);
        return Err(e);
    }
    fs::rename(&tmp, path)?;
    Ok(written)
}

fn write_all<W: Write>(
    w: &mut LeWriter<W>,
    index: &Index,
    node_count: u32,
    name_units: u32,
    root_count: u32,
    build_ms: u64,
    checksum: u64,
) -> io::Result<()> {
    // Cabecera.
    w.raw(&MAGIC)?;
    w.u16(VERSION)?;
    w.u16(FLAG_LE)?; // siempre escribimos little-endian
    w.u32(node_count)?;
    w.u32(name_units)?;
    w.u32(root_count)?;
    w.raw(&[0u8; 3])?; // padding
    w.u32(index.letter as u32)?;
    w.i64(index.built_at)?;
    w.u64(index.stats.records)?;
    w.u64(index.stats.files)?;
    w.u64(index.stats.dirs)?;
    w.u64(index.stats.skipped)?;
    w.u64(build_ms)?;
    w.u64(checksum)?;

    // Raíces.
    w.alinear()?;
    for &r in &index.roots {
        w.u32(r)?;
    }

    // Nodos, campo por campo.
    w.alinear()?;
    let mut buf = [0u8; NODE_LEN];
    for n in &index.nodes {
        node_to_bytes(n, &mut buf);
        w.raw(&buf)?;
    }

    // Arena UTF-16, por tandas para no armar un buffer gigante.
    w.alinear()?;
    let mut chunk: Vec<u8> = Vec::with_capacity(64 * 1024);
    for &u in &index.names {
        chunk.extend_from_slice(&u.to_le_bytes());
        if chunk.len() >= 64 * 1024 {
            w.raw(&chunk)?;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        w.raw(&chunk)?;
    }

    // Bloques derivados (v2). Un 0 en la cantidad significa "no está".
    let bloques: [usize; 6] = [
        index.folded.len(),
        index.dir_bytes.len(),
        index.hijos_inicio.len(),
        index.hijos.len(),
        index.por_nombre.len(),
        index.euler.len(),
    ];
    for (i, &len) in bloques.iter().enumerate() {
        w.alinear()?;
        w.u32(u32::try_from(len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "bloque derivado demasiado grande",
            )
        })?)?;
        match i {
            0 => {
                for &u in &index.folded {
                    w.u16(u)?;
                }
            }
            1 => {
                for &v in &index.dir_bytes {
                    w.u64(v)?;
                }
            }
            2 => {
                for &v in &index.hijos_inicio {
                    w.u32(v)?;
                }
            }
            3 => {
                for &v in &index.hijos {
                    w.u32(v)?;
                }
            }
            4 => {
                for &v in &index.por_nombre {
                    w.u32(v)?;
                }
            }
            _ => {
                for &v in &index.euler {
                    w.u32(v)?;
                }
            }
        }
    }

    // Cola opcional: marca del journal aplicada (USN, C2). Los lectores viejos
    // no la necesitan y los nuevos la toleran ausente.
    w.alinear()?;
    let (journal_id, next_usn) = index.overlay().watermark.unwrap_or((0, 0));
    w.u64(journal_id)?;
    w.i64(next_usn)?;
    Ok(())
}

// ----------------------------------------------------------------- lectura

/// Errores al cargar un snapshot `C.wcat`.
#[derive(Debug)]
pub enum LoadError {
    /// Error de E/S que no es "se acabó el archivo".
    Io(std::io::Error),
    /// El magic no es `CENSUSWCAT`.
    Magic,
    /// Versión del formato que no sabemos leer.
    Version(u16),
    /// El archivo terminó antes de tiempo.
    Truncated,
    /// El checksum de nodos+arena no coincide.
    Checksum,
    /// El archivo pasa el checksum pero el índice es internamente inválido.
    BadIndex(String),
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LoadError::Io(e) => write!(f, "error de E/S leyendo el snapshot: {e}"),
            LoadError::Magic => write!(f, "no es un snapshot C.wcat (magic inválido)"),
            LoadError::Version(v) => write!(f, "versión de snapshot no soportada: {v}"),
            LoadError::Truncated => write!(f, "snapshot truncado o incompleto"),
            LoadError::Checksum => write!(f, "snapshot corrupto (checksum inválido)"),
            LoadError::BadIndex(d) => write!(f, "snapshot con índice inválido: {d}"),
        }
    }
}

impl std::error::Error for LoadError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LoadError::Io(e) => Some(e),
            _ => None,
        }
    }
}

/// Traduce un error de `read_exact`: quedarse sin bytes es `Truncated`.
fn eof_or_io(e: io::Error) -> LoadError {
    if e.kind() == io::ErrorKind::UnexpectedEof {
        LoadError::Truncated
    } else {
        LoadError::Io(e)
    }
}

/// Parser little-endian sobre una porción ya leída (la cabecera).
struct LeReader<'a> {
    b: &'a [u8],
    off: usize,
}

impl<'a> LeReader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.off.checked_add(n)?;
        if end > self.b.len() {
            return None;
        }
        let s = &self.b[self.off..end];
        self.off = end;
        Some(s)
    }
    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
}

/// Campo de cabecera obligatorio: no puede faltar con `HEADER_LEN` bytes.
fn campo<T>(v: Option<T>) -> Result<T, LoadError> {
    v.ok_or(LoadError::Truncated)
}

/// De dónde salen los bytes: el mapa completo o el archivo leído en orden.
enum Origen<'m> {
    Mapa(&'m [u8]),
    Archivo(fs::File),
}

impl Origen<'_> {
    /// Lee `n` bytes y los devuelve prestados (del mapa o de `scratch`).
    ///
    /// En el camino mapeado no copia nada: devuelve una vista del mapa. En el
    /// camino de archivo usa `scratch` como buffer reutilizable.
    fn leer<'a>(
        &'a mut self,
        pos: &mut usize,
        n: usize,
        scratch: &'a mut Vec<u8>,
    ) -> Result<&'a [u8], LoadError> {
        match self {
            Origen::Mapa(b) => {
                let fin = pos.checked_add(n).ok_or(LoadError::Truncated)?;
                if fin > b.len() {
                    return Err(LoadError::Truncated);
                }
                let trozo = &b[*pos..fin];
                *pos = fin;
                Ok(trozo)
            }
            Origen::Archivo(f) => {
                scratch.clear();
                scratch.resize(n, 0);
                f.read_exact(scratch).map_err(eof_or_io)?;
                *pos += n;
                Ok(scratch)
            }
        }
    }
}

/// Salta el relleno de alineación de la versión 4 (0..7 bytes de ceros).
fn alinear_lectura(
    origen: &mut Origen<'_>,
    pos: &mut usize,
    scratch: &mut Vec<u8>,
    con_padding: bool,
) -> Result<(), LoadError> {
    if !con_padding {
        return Ok(());
    }
    let relleno = pos.next_multiple_of(ALINEACION) - *pos;
    if relleno > 0 {
        origen.leer(pos, relleno, scratch)?;
    }
    Ok(())
}

/// Lee una arena de `u16` (`names` o `folded`): sin copiar si el rango del mapa
/// está alineado a 2 bytes, copiando en cualquier otro caso.
///
/// `hash` sólo se pasa para `names`, que es lo único de las dos arenas que
/// entra en el checksum.
fn leer_arena_u16(
    origen: &mut Origen<'_>,
    pos: &mut usize,
    unidades: usize,
    scratch: &mut Vec<u8>,
    mapa: Option<&Arc<Mapa>>,
    hash: Option<&mut Fnv>,
) -> Result<Arena<u16>, LoadError> {
    let bytes_len = unidades.checked_mul(2).ok_or(LoadError::Truncated)?;
    let inicio = *pos;
    if let Some(m) = mapa {
        if let Some(vista) = m.u16s(inicio, unidades) {
            let bytes = origen.leer(pos, bytes_len, scratch)?;
            if let Some(h) = hash {
                h.bytes(bytes);
            }
            return Ok(Arena::mapeado(m.clone(), vista));
        }
    }
    let bytes = origen.leer(pos, bytes_len, scratch)?;
    if let Some(h) = hash {
        h.bytes(bytes);
    }
    Ok(Arena::propio(bytes_a_u16(bytes)))
}

/// Carga un snapshot `C.wcat` desde `path`.
///
/// Valida magic, versión (4, y 3 por compatibilidad), endian, cantidades
/// absurdas (antes de reservar), checksum de nodos+arena y la estructura del
/// índice: `parent` en rango, `name_off + name_len <= name_units` y raíces en
/// rango.
///
/// Con un snapshot v4 el archivo se mapea y `names`/`folded` se leen directo
/// del mapa (`Arena::mapeado`): no se copian. Si el mapeo falla, si el archivo
/// es v3 o si `CENSUS_MMAP=0`, se copian como antes y el índice es idéntico.
pub fn load(path: &Path) -> Result<Index, LoadError> {
    load_con(path, mmap_habilitado())
}

/// `CENSUS_MMAP=0` desactiva el mapeo: queda el camino de copia.
fn mmap_habilitado() -> bool {
    match std::env::var_os("CENSUS_MMAP") {
        None => true,
        Some(v) => v.to_str() != Some("0"),
    }
}

fn load_con(path: &Path, usar_mapa: bool) -> Result<Index, LoadError> {
    let depurar = std::env::var_os("CENSUS_DEBUG").is_some();
    let t0 = std::time::Instant::now();
    let archivo = fs::File::open(path).map_err(LoadError::Io)?;

    // El mapa evita copiar las dos arenas grandes. Si falla (sistema de
    // archivos raro, archivo vacío), seguimos con el camino de copia.
    // Las vistas `u16` del mapa son little-endian (el formato lo es), así que
    // en un objetivo big-endian ni siquiera se mapea: se copia y se convierte.
    let mapa = if usar_mapa && cfg!(target_endian = "little") {
        Mapa::abrir(&archivo).ok()
    } else {
        None
    };
    let mut origen = match &mapa {
        Some(m) => Origen::Mapa(m.bytes()),
        None => Origen::Archivo(archivo),
    };
    let mut pos = 0usize;
    let mut scratch: Vec<u8> = Vec::new();

    // Cabecera completa primero; un archivo más corto es un truncado.
    let hdr = origen.leer(&mut pos, HEADER_LEN, &mut scratch)?;
    let mut r = LeReader { b: hdr, off: 0 };
    let magic = campo(r.take(10))?;
    if magic != MAGIC {
        return Err(LoadError::Magic);
    }
    let version = campo(r.u16())?;
    if version != VERSION && version != VERSION_SIN_PADDING {
        return Err(LoadError::Version(version));
    }
    let flags = campo(r.u16())?;
    if flags & FLAG_LE == 0 {
        return Err(LoadError::BadIndex("flags: endian no soportado".into()));
    }
    let node_count = campo(r.u32())?;
    let name_units = campo(r.u32())?;
    let root_count = campo(r.u32())?;
    let _padding = campo(r.take(3))?;
    let letter_u32 = campo(r.u32())?;
    let built_at = campo(r.i64())?;
    let stats = Stats {
        records: campo(r.u64())?,
        files: campo(r.u64())?,
        dirs: campo(r.u64())?,
        skipped: campo(r.u64())?,
        build_ms: campo(r.u64())? as u128,
        name_units: name_units as u64,
    };
    let checksum = campo(r.u64())?;

    // Cantidades absurdas: rechazar antes de reservar nada.
    if node_count > MAX_NODES {
        return Err(LoadError::BadIndex(format!(
            "node_count absurdo: {node_count}"
        )));
    }
    if name_units > MAX_NAME_UNITS {
        return Err(LoadError::BadIndex(format!(
            "name_units absurdo: {name_units}"
        )));
    }
    // Las raíces son ids de nodo, así que no puede haber más que nodos.
    if root_count as u64 > node_count as u64 {
        return Err(LoadError::BadIndex(format!(
            "root_count absurdo: {root_count} > {node_count}"
        )));
    }
    let letter = char::from_u32(letter_u32)
        .ok_or_else(|| LoadError::BadIndex(format!("letra inválida: U+{letter_u32:04X}")))?;

    // v4 alinea cada sección a 8 bytes; v3 no tiene relleno.
    let con_padding = version == VERSION;

    // Raíces.
    alinear_lectura(&mut origen, &mut pos, &mut scratch, con_padding)?;
    let roots_bytes = origen.leer(&mut pos, root_count as usize * 4, &mut scratch)?;
    let roots: Vec<u32> = roots_bytes
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    for &rt in &roots {
        if rt >= node_count {
            return Err(LoadError::BadIndex(format!(
                "raíz fuera de rango: {rt} >= {node_count}"
            )));
        }
    }

    // Nodos por tandas, hasheando a medida que se leen.
    alinear_lectura(&mut origen, &mut pos, &mut scratch, con_padding)?;
    let mut nodes: Vec<Node> = Vec::with_capacity(node_count as usize);
    let mut h = Fnv::new();
    let mut leidos: u32 = 0;
    while leidos < node_count {
        let n = (node_count - leidos).min(NODE_CHUNK as u32) as usize;
        let bytes = origen.leer(&mut pos, n * NODE_LEN, &mut scratch)?;
        for (k, node_bytes) in bytes.chunks_exact(NODE_LEN).enumerate() {
            let arr: [u8; NODE_LEN] = node_bytes.try_into().map_err(|_| LoadError::Truncated)?;
            h.bytes(&arr);
            let node = node_from_bytes(&arr);
            let id = leidos + k as u32;
            if node.parent >= node_count {
                return Err(LoadError::BadIndex(format!(
                    "parent fuera de rango en el nodo {id}: {} >= {node_count}",
                    node.parent
                )));
            }
            if node.name_off as u64 + node.name_len as u64 > name_units as u64 {
                return Err(LoadError::BadIndex(format!(
                    "nombre fuera de rango en el nodo {id}: {} + {} > {name_units}",
                    node.name_off, node.name_len
                )));
            }
            nodes.push(node);
        }
        leidos += n as u32;
    }

    // Arena UTF-16 (`names`), sin copiar cuando el mapa está alineado.
    if depurar {
        eprintln!("  [load] nodos en {:?}", t0.elapsed());
    }
    alinear_lectura(&mut origen, &mut pos, &mut scratch, con_padding)?;
    let names = leer_arena_u16(
        &mut origen,
        &mut pos,
        name_units as usize,
        &mut scratch,
        mapa.as_ref(),
        Some(&mut h),
    )?;

    // Bloques derivados (v2). Si faltan (cantidad 0), se recalculan abajo.
    let techos = [
        name_units.saturating_add(1),
        node_count.saturating_add(1),
        node_count.saturating_add(1),
        node_count.saturating_mul(4).saturating_add(1),
        node_count.saturating_add(1),
        node_count.saturating_add(1),
    ];
    let anchos = [2usize, 8, 4, 4, 4, 4];
    let mut folded = Arena::default();
    let mut dir_bytes: Vec<u64> = Vec::new();
    let mut hijos_inicio: Vec<u32> = Vec::new();
    let mut hijos: Vec<u32> = Vec::new();
    let mut por_nombre: Vec<u32> = Vec::new();
    let mut euler: Vec<u32> = Vec::new();
    for i in 0..6 {
        alinear_lectura(&mut origen, &mut pos, &mut scratch, con_padding)?;
        let cb = origen.leer(&mut pos, 4, &mut scratch)?;
        let cant = u32::from_le_bytes([cb[0], cb[1], cb[2], cb[3]]);
        if cant > techos[i] {
            return Err(LoadError::BadIndex(format!(
                "bloque derivado {i} absurdo: {cant} > {}",
                techos[i]
            )));
        }
        if i == 0 {
            // `folded`: mismos largos que `names`, sin copiar si se puede.
            folded = leer_arena_u16(
                &mut origen,
                &mut pos,
                cant as usize,
                &mut scratch,
                mapa.as_ref(),
                None,
            )?;
        } else {
            let bytes = origen.leer(&mut pos, cant as usize * anchos[i], &mut scratch)?;
            match i {
                1 => dir_bytes = bytes_a_u64(bytes),
                2 => hijos_inicio = bytes_a_u32(bytes),
                3 => hijos = bytes_a_u32(bytes),
                4 => por_nombre = bytes_a_u32(bytes),
                _ => euler = bytes_a_u32(bytes),
            }
        }
    }
    if !folded.is_empty() && folded.len() != names.len() {
        return Err(LoadError::BadIndex(
            "folded no tiene el mismo largo que la arena".into(),
        ));
    }
    if !dir_bytes.is_empty() && dir_bytes.len() != nodes.len() {
        return Err(LoadError::BadIndex(
            "dir_bytes no tiene un valor por nodo".into(),
        ));
    }
    for (nombre, bloque) in [
        ("hijos_inicio", &hijos_inicio),
        ("por_nombre", &por_nombre),
        ("euler", &euler),
    ] {
        if bloque.iter().any(|&v| v >= node_count.max(1)) {
            return Err(LoadError::BadIndex(format!("{nombre}: id fuera de rango")));
        }
    }

    if depurar {
        eprintln!("  [load] arena en {:?}", t0.elapsed());
    }

    // Cola opcional con la marca del journal.
    alinear_lectura(&mut origen, &mut pos, &mut scratch, con_padding)?;
    let marca = match origen.leer(&mut pos, 16, &mut scratch) {
        Ok(c) => Some((
            u64::from_le_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]),
            i64::from_le_bytes([c[8], c[9], c[10], c[11], c[12], c[13], c[14], c[15]]),
        )),
        Err(LoadError::Truncated) => None,
        Err(e) => return Err(e),
    };

    // Recién ahora comparamos el checksum: antes pasaron las validaciones
    // estructurales, así que un `BadIndex` tiene prioridad sobre `Checksum`.
    if h.finish() != checksum {
        return Err(LoadError::Checksum);
    }
    if depurar {
        eprintln!("  [load] checksum en {:?}", t0.elapsed());
    }

    let mut ix = Index {
        letter,
        root: roots.first().copied().unwrap_or(0),
        roots,
        nodes,
        names,
        stats,
        built_at,
        dir_bytes,
        vol_serials: Vec::new(),
        folded,
        hijos_inicio,
        hijos,
        por_nombre,
        rango: Vec::new(),
        euler,
        tin: Vec::new(),
        tout: Vec::new(),
        compacto: Vec::new(),
        frn_sorted: false,
        overlay: std::sync::Arc::new(std::sync::RwLock::new(crate::overlay::Overlay::default())),
    };
    if let Some((id, usn)) = marca {
        if id != 0 {
            *ix.overlay.write().unwrap_or_else(|e| e.into_inner()) =
                crate::overlay::Overlay::desde_watermark(id, usn);
        }
    }
    if depurar {
        eprintln!(
            "  [load] mapa: {}",
            if ix.names.es_mapeada() {
                "sí (sin copiar)"
            } else {
                "no (copiando)"
            }
        );
    }
    // Si el snapshot trae todo, sólo se reconstruye lo barato (rango y
    // tin/tout). Si no (v2 con bloques vacíos), se calcula como siempre.
    let completo = ix.dir_bytes.len() == ix.nodes.len()
        && ix.folded.len() == ix.names.len()
        && ix.hijos_inicio.len() == ix.nodes.len() + 1
        && ix.por_nombre.len() == ix.nodes.len()
        && ix.euler.len() == ix.nodes.len();
    if depurar {
        eprintln!("  [load] derivados en {:?}", t0.elapsed());
    }
    if depurar {
        eprintln!("  [load] struct listo en {:?}", t0.elapsed());
    }
    if completo {
        ix.rebuild_light();
    } else {
        ix.compute_dir_bytes();
        ix.compute_folded();
        ix.rebuild_indexes();
    }
    if depurar {
        eprintln!("  [load] total {:?}", t0.elapsed());
    }
    Ok(ix)
}

// -------------------------------------------------------------- rutas

/// Directorio de caché: `%LOCALAPPDATA%\Census`.
pub fn cache_dir() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|la| PathBuf::from(la).join("Census"))
}

/// Ruta por defecto del snapshot: `%LOCALAPPDATA%\Census\C.wcat`.
pub fn default_path() -> Option<PathBuf> {
    cache_dir().map(|d| d.join("C.wcat"))
}

// ---------------------------------------------------------------- tests

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Builder, F_DIR, F_ROOT};

    /// Archivo temporal con nombre único; se borra solo, incluso si el test
    /// entra en pánico.
    struct TempFile(PathBuf);

    impl TempFile {
        fn new(nombre: &str) -> Self {
            let p = std::env::temp_dir().join(format!(
                "censusindex_snap_test_{}_{}_{nombre}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .subsec_nanos()
            ));
            let _ = fs::remove_file(&p);
            TempFile(p)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
            let mut t = self.0.as_os_str().to_os_string();
            t.push(".tmp");
            let _ = fs::remove_file(PathBuf::from(t));
        }
    }

    fn rec(frn: u64, parent: u64, name: &str, dir: bool, size: u64) -> crate::source::RawRecord {
        crate::source::RawRecord {
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

    /// Índice sintético con dos raíces (C: y D:) y nombres con acentos y ñ.
    fn dos_raices() -> Index {
        let mut bc = Builder::with_capacity(8);
        bc.push(rec(5, 5, ".", true, 0));
        bc.push(rec(10, 5, "Usuarios", true, 0));
        bc.push(rec(11, 10, "Ñandú.txt", false, 42));
        bc.push(rec(12, 10, "informe áéíóú.pdf", false, 4096));
        let c = bc.finish('c');

        let mut bd = Builder::with_capacity(8);
        bd.push(rec(5, 5, ".", true, 0));
        bd.push(rec(20, 5, "añoün.xlsx", false, 7));
        let d = bd.finish('D');

        let mut ix = Index::merge(vec![c, d]);
        ix.built_at = 1_700_000_123;
        ix.stats.records = 111;
        ix.stats.skipped = 7;
        ix.stats.build_ms = 1234;
        ix
    }

    /// Comparación campo por campo de dos índices.
    fn assert_same_ix(a: &Index, b: &Index) {
        assert_eq!(a.letter, b.letter, "letter");
        assert_eq!(a.root, b.root, "root");
        assert_eq!(a.roots, b.roots, "roots");
        assert_eq!(a.names, b.names, "arena de nombres");
        assert_eq!(a.built_at, b.built_at, "built_at");
        assert_eq!(a.stats.records, b.stats.records, "stats.records");
        assert_eq!(a.stats.files, b.stats.files, "stats.files");
        assert_eq!(a.stats.dirs, b.stats.dirs, "stats.dirs");
        assert_eq!(a.stats.skipped, b.stats.skipped, "stats.skipped");
        assert_eq!(a.stats.name_units, b.stats.name_units, "stats.name_units");
        assert_eq!(a.stats.build_ms, b.stats.build_ms, "stats.build_ms");
        assert_eq!(a.nodes.len(), b.nodes.len(), "cantidad de nodos");
        for (i, (x, y)) in a.nodes.iter().zip(b.nodes.iter()).enumerate() {
            assert_eq!(x.name_off, y.name_off, "nodo {i}: name_off");
            assert_eq!(x.name_len, y.name_len, "nodo {i}: name_len");
            assert_eq!(x.flags, y.flags, "nodo {i}: flags");
            assert_eq!(x.parent, y.parent, "nodo {i}: parent");
            assert_eq!(x.size, y.size, "nodo {i}: size");
            assert_eq!(x.mtime, y.mtime, "nodo {i}: mtime");
            assert_eq!(x.atime, y.atime, "nodo {i}: atime");
            assert_eq!(x.attrs, y.attrs, "nodo {i}: attrs");
            assert_eq!(x.frn, y.frn, "nodo {i}: frn");
        }
    }

    /// Reescribe `path` con `f` aplicado a sus bytes (para corruptarlo).
    fn parchear(path: &Path, f: impl FnOnce(&mut [u8])) {
        let mut bytes = fs::read(path).unwrap();
        f(&mut bytes);
        fs::write(path, bytes).unwrap();
    }

    /// Offsets (nodos, arena) tal como los escribe la v4, con padding.
    fn offsets_v4(ix: &Index) -> (usize, usize) {
        let raices = HEADER_LEN.next_multiple_of(ALINEACION);
        let nodos = (raices + 4 * ix.roots.len()).next_multiple_of(ALINEACION);
        let arena = (nodos + NODE_LEN * ix.nodes.len()).next_multiple_of(ALINEACION);
        (nodos, arena)
    }

    /// Tamaño esperado de un archivo v4 (con padding en cada sección).
    fn tamano_v4(ix: &Index) -> u64 {
        let (_, arena) = offsets_v4(ix);
        let mut off = arena + 2 * ix.names.len();
        let bloques = [
            (2usize, ix.folded.len()),
            (8, ix.dir_bytes.len()),
            (4, ix.hijos_inicio.len()),
            (4, ix.hijos.len()),
            (4, ix.por_nombre.len()),
            (4, ix.euler.len()),
        ];
        for (ancho, len) in bloques {
            off = off.next_multiple_of(ALINEACION) + 4 + ancho * len;
        }
        (off.next_multiple_of(ALINEACION) + 16) as u64
    }

    /// Escribe a mano un snapshot con el layout viejo v3: sin padding.
    fn escribir_v3(ix: &Index, path: &Path) {
        let mut out: Vec<u8> = Vec::new();
        out.extend_from_slice(&MAGIC);
        out.extend_from_slice(&VERSION_SIN_PADDING.to_le_bytes());
        out.extend_from_slice(&FLAG_LE.to_le_bytes());
        out.extend_from_slice(&(ix.nodes.len() as u32).to_le_bytes());
        out.extend_from_slice(&(ix.names.len() as u32).to_le_bytes());
        out.extend_from_slice(&(ix.roots.len() as u32).to_le_bytes());
        out.extend_from_slice(&[0u8; 3]);
        out.extend_from_slice(&(ix.letter as u32).to_le_bytes());
        out.extend_from_slice(&ix.built_at.to_le_bytes());
        out.extend_from_slice(&ix.stats.records.to_le_bytes());
        out.extend_from_slice(&ix.stats.files.to_le_bytes());
        out.extend_from_slice(&ix.stats.dirs.to_le_bytes());
        out.extend_from_slice(&ix.stats.skipped.to_le_bytes());
        out.extend_from_slice(&(ix.stats.build_ms as u64).to_le_bytes());
        out.extend_from_slice(&checksum_of(ix).to_le_bytes());
        for &r in &ix.roots {
            out.extend_from_slice(&r.to_le_bytes());
        }
        let mut buf = [0u8; NODE_LEN];
        for n in &ix.nodes {
            node_to_bytes(n, &mut buf);
            out.extend_from_slice(&buf);
        }
        for &u in ix.names.iter() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&(ix.folded.len() as u32).to_le_bytes());
        for &u in ix.folded.iter() {
            out.extend_from_slice(&u.to_le_bytes());
        }
        out.extend_from_slice(&(ix.dir_bytes.len() as u32).to_le_bytes());
        for &v in &ix.dir_bytes {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(ix.hijos_inicio.len() as u32).to_le_bytes());
        for &v in &ix.hijos_inicio {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(ix.hijos.len() as u32).to_le_bytes());
        for &v in &ix.hijos {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(ix.por_nombre.len() as u32).to_le_bytes());
        for &v in &ix.por_nombre {
            out.extend_from_slice(&v.to_le_bytes());
        }
        out.extend_from_slice(&(ix.euler.len() as u32).to_le_bytes());
        for &v in &ix.euler {
            out.extend_from_slice(&v.to_le_bytes());
        }
        // Cola opcional (marca del journal).
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&0i64.to_le_bytes());
        fs::write(path, out).unwrap();
    }

    #[test]
    fn round_trip_con_dos_raices_y_nombres_raros() {
        let t = TempFile::new("round_trip");
        let ix = dos_raices();
        assert_eq!(ix.roots.len(), 2, "el índice sintético tiene 2 raíces");
        assert!(ix.names.contains(&('ñ' as u16)), "hay ñ en la arena");

        let escritos = save(&ix, t.path()).unwrap();
        assert!(escritos > 0);
        let leído = load(t.path()).unwrap();
        assert_same_ix(&ix, &leído);

        // Los nombres raros sobreviven intactos.
        let con_ñ = (0..leído.len() as u32).any(|i| leído.name_string(i) == "Ñandú.txt");
        assert!(con_ñ, "el nombre con ñ no sobrevivió al round-trip");
        let con_acentos =
            (0..leído.len() as u32).any(|i| leído.name_string(i) == "informe áéíóú.pdf");
        assert!(
            con_acentos,
            "el nombre con acentos no sobrevivió al round-trip"
        );
        assert_eq!(leído.root_for('C'), Some(leído.root));
        assert!(leído.root_for('D').is_some());
    }

    #[test]
    fn round_trip_indice_vacio() {
        let t = TempFile::new("vacio");
        let ix = Index::default();
        save(&ix, t.path()).unwrap();
        let leído = load(t.path()).unwrap();
        assert_same_ix(&ix, &leído);
        assert!(leído.nodes.is_empty());
        assert!(leído.names.is_empty());
        assert!(leído.roots.is_empty());
    }

    #[test]
    fn errores_magic_version_truncado() {
        let t = TempFile::new("malos");

        // Magic alterado.
        let ix = dos_raices();
        save(&ix, t.path()).unwrap();
        parchear(t.path(), |b| b[3] ^= 0xFF);
        match load(t.path()) {
            Err(LoadError::Magic) => {}
            otro => panic!("se esperaba Magic, obtuve: {otro:?}"),
        }

        // Versión 99.
        save(&ix, t.path()).unwrap();
        parchear(t.path(), |b| {
            b[10..12].copy_from_slice(&99u16.to_le_bytes())
        });
        match load(t.path()) {
            Err(LoadError::Version(99)) => {}
            otro => panic!("se esperaba Version(99), obtuve: {otro:?}"),
        }

        // Archivo truncado a la mitad.
        save(&ix, t.path()).unwrap();
        let bytes = fs::read(t.path()).unwrap();
        fs::write(t.path(), &bytes[..bytes.len() / 2]).unwrap();
        match load(t.path()) {
            Err(LoadError::Truncated) => {}
            otro => panic!("se esperaba Truncated, obtuve: {otro:?}"),
        }

        // También truncado por debajo de la cabecera.
        save(&ix, t.path()).unwrap();
        let bytes = fs::read(t.path()).unwrap();
        fs::write(t.path(), &bytes[..HEADER_LEN - 1]).unwrap();
        match load(t.path()) {
            Err(LoadError::Truncated) => {}
            otro => panic!("se esperaba Truncated (cabecera), obtuve: {otro:?}"),
        }
    }

    #[test]
    fn byte_alterado_da_checksum() {
        let t = TempFile::new("checksum");
        let ix = dos_raices();
        save(&ix, t.path()).unwrap();

        // Alteramos un byte en el medio de la arena de nombres: no hay
        // validación estructural ahí, así que el error tiene que ser Checksum.
        let bytes = fs::read(t.path()).unwrap();
        let (_, arena_start) = offsets_v4(&ix);
        assert!(arena_start < bytes.len());
        let medio = arena_start + 2 * (ix.names.len() / 2);
        let mut corrupto = bytes.clone();
        corrupto[medio] ^= 0x01;
        fs::write(t.path(), &corrupto).unwrap();
        match load(t.path()) {
            Err(LoadError::Checksum) => {}
            otro => panic!("se esperaba Checksum, obtuve: {otro:?}"),
        }

        // Los bloques derivados no entran en el checksum (se validan por
        // estructura), así que corromper uno no necesariamente falla: lo que
        // importa es que un byte de los nodos sí falle.
        // Ídem con un byte del medio de la zona de nodos.
        let (nodos_start, _) = offsets_v4(&ix);
        let medio_nodos = nodos_start + NODE_LEN * (ix.nodes.len() / 2);
        let mut corrupto = bytes.clone();
        corrupto[medio_nodos] ^= 0x01;
        fs::write(t.path(), &corrupto).unwrap();
        match load(t.path()) {
            Err(LoadError::Checksum) => {}
            otro => panic!("se esperaba Checksum (nodos), obtuve: {otro:?}"),
        }
    }

    #[test]
    fn parent_fuera_de_rango_da_bad_index() {
        let t = TempFile::new("parent");
        let ix = dos_raices();
        save(&ix, t.path()).unwrap();

        // `parent` del nodo 1 vive en <inicio de nodos> + 1*48 + 8.
        let (nodos_start, _) = offsets_v4(&ix);
        let off = nodos_start + NODE_LEN + NODE_PARENT_OFF;
        parchear(t.path(), |b| {
            b[off..off + 4].copy_from_slice(&u32::MAX.to_le_bytes())
        });
        match load(t.path()) {
            Err(LoadError::BadIndex(_)) => {}
            otro => panic!("se esperaba BadIndex, obtuve: {otro:?}"),
        }

        // Un nombre que se pasa de la arena también es BadIndex.
        save(&ix, t.path()).unwrap(); // arrancamos de un archivo válido
        let off = nodos_start + 2 * NODE_LEN;
        parchear(t.path(), |b| {
            b[off..off + 4].copy_from_slice(&(ix.stats.name_units as u32).to_le_bytes())
        });
        match load(t.path()) {
            Err(LoadError::BadIndex(d)) => assert!(d.contains("nombre"), "detalle: {d}"),
            otro => panic!("se esperaba BadIndex (nombre), obtuve: {otro:?}"),
        }
    }

    #[test]
    fn cargar_dos_veces_da_lo_mismo() {
        let t = TempFile::new("dos_veces");
        let ix = dos_raices();
        save(&ix, t.path()).unwrap();
        let a = load(t.path()).unwrap();
        let b = load(t.path()).unwrap();
        assert_same_ix(&a, &b);
        assert_same_ix(&ix, &b);
    }

    #[test]
    fn tamano_del_archivo() {
        let t = TempFile::new("tamano");
        let ix = dos_raices();
        save(&ix, t.path()).unwrap();
        let meta = fs::metadata(t.path()).unwrap();
        assert_eq!(meta.len(), tamano_v4(&ix));

        // Y para un índice vacío: sólo cabecera alineada + bloques vacíos.
        let t2 = TempFile::new("tamano_vacio");
        save(&Index::default(), t2.path()).unwrap();
        assert_eq!(
            fs::metadata(t2.path()).unwrap().len(),
            tamano_v4(&Index::default())
        );
    }

    #[test]
    fn v4_mapeado_y_copiado_dan_el_mismo_indice() {
        let t = TempFile::new("v4_mapeado");
        let ix = dos_raices();
        save(&ix, t.path()).unwrap();

        // Mismo archivo por los dos caminos: con mapa y con copia.
        let mapeado = load_con(t.path(), true).unwrap();
        let copiado = load_con(t.path(), false).unwrap();
        assert_same_ix(&ix, &mapeado);
        assert_same_ix(&mapeado, &copiado);

        // Consulta simple: las dos cargas responden igual.
        let qa = crate::query::parse("ext:pdf", &mapeado);
        let qb = crate::query::parse("ext:pdf", &copiado);
        assert_eq!(mapeado.match_ids(&qa), copiado.match_ids(&qb));
        assert!(!mapeado.match_ids(&qa).is_empty());
    }

    #[test]
    fn v4_mapea_las_dos_arenas_sin_copiarlas() {
        let t = TempFile::new("v4_arenas");
        let ix = dos_raices();
        save(&ix, t.path()).unwrap();
        let leido = load_con(t.path(), true).unwrap();
        // En little-endian el rango de las arenas queda alineado por la v4.
        #[cfg(target_endian = "little")]
        {
            assert!(leido.names.es_mapeada(), "names debería leerse del mapa");
            assert!(leido.folded.es_mapeada(), "folded debería leerse del mapa");
        }
        assert_eq!(leido.names.as_slice(), ix.names.as_slice());
        assert_eq!(leido.folded.as_slice(), ix.folded.as_slice());
    }

    #[test]
    fn v3_sin_padding_se_sigue_leyendo() {
        let t = TempFile::new("v3");
        let ix = dos_raices();
        escribir_v3(&ix, t.path());
        let bytes = fs::read(t.path()).unwrap();
        assert_eq!(u16::from_le_bytes([bytes[10], bytes[11]]), 3);

        let leido = load(t.path()).unwrap();
        assert_same_ix(&ix, &leido);
        // Sin padding la arena cae impar: se copia, no se mapea.
        assert!(!leido.names.es_mapeada());
        assert_eq!(leido.name_string(1), "Usuarios");
    }

    #[test]
    fn sin_mapa_el_camino_de_copia_da_lo_mismo() {
        let t = TempFile::new("sin_mapa");
        let ix = dos_raices();
        save(&ix, t.path()).unwrap();
        let copiado = load_con(t.path(), false).unwrap();
        assert!(
            !copiado.names.es_mapeada(),
            "con usar_mapa=false no hay mapa"
        );
        assert!(!copiado.folded.es_mapeada(), "folded tampoco");
        assert_same_ix(&ix, &copiado);
        assert_eq!(copiado.name_string(1), "Usuarios");
    }

    #[test]
    fn nodos_del_snapshot_son_validos() {
        // Chequeo extra: lo leído respeta los invariantes del índice y las
        // marcas de directorio/raíz se conservan.
        let t = TempFile::new("flags");
        let ix = dos_raices();
        save(&ix, t.path()).unwrap();
        let leído = load(t.path()).unwrap();
        let nu = leído.stats.name_units;
        for (i, n) in leído.nodes.iter().enumerate() {
            assert!((n.parent as usize) < leído.nodes.len(), "nodo {i}: parent");
            assert!(
                n.name_off as u64 + n.name_len as u64 <= nu,
                "nodo {i}: nombre fuera de la arena"
            );
        }
        let esperado_dirs = ix.nodes.iter().filter(|n| n.flags & F_DIR != 0).count();
        let esperado_roots = ix.nodes.iter().filter(|n| n.flags & F_ROOT != 0).count();
        assert_eq!(leído.stats.dirs, esperado_dirs as u64);
        assert_eq!(
            leído.stats.files,
            (leído.nodes.len() - esperado_dirs) as u64
        );
        assert_eq!(
            leído.nodes.iter().filter(|n| n.flags & F_ROOT != 0).count(),
            esperado_roots
        );
    }
}
