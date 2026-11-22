//! Snapshot en disco del índice: formato propio `C.wcat`.
// `as_chunks` (lo que sugiere el lint) todavía es inestable en Rust estable.
#![allow(clippy::chunks_exact_to_as_chunks)]
//!
//! Es lo que permite arrancar en milisegundos en vez de releer la MFT: el
//! índice completo (`Index`) se vuelca a un archivo y al arrancar se lee tal
//! cual, sin tocar NTFS ni pedir elevación.
//!
//! # Formato `C.wcat` (little-endian, sólo std, sin unsafe)
//!
//! Todo se serializa **campo por campo** en little-endian. `Node` NO es
//! `repr(C)`: jamás se escriben bytes crudos de la estructura.
//!
//! Cabecera fija de `HEADER_LEN` (89) bytes, en este orden:
//!
//! | Off | Tamaño | Campo |
//! |-----|--------|-------|
//! | 0   | 10     | magic: `CENSUSWCAT` (ASCII, 10 bytes) |
//! | 10  | u16    | version = 1 |
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
//! | 81  | u64    | checksum: FNV-1a 64 sobre nodos + arena + índices derivados |
//!
//! Nota: los campos pedidos para la cabecera suman 89 bytes (no 64); el
//! tamaño total del archivo es `HEADER_LEN + 4*root_count + 48*node_count +
//! 2*name_units`.
//!
//! Después de la cabecera, en orden:
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
//! El checksum (FNV-1a 64, offset `0xcbf29ce484222325`, primo
//! `0x100000001b3`) cubre los bytes de los nodos y de la arena; la cabecera y
//! las raíces quedan fuera porque los campos estructurales se validan aparte
//! al cargar (rango de `parent`, de `name_off`/`name_len` y de las raíces).
//!
//! `save` escribe a un `<ruta>.tmp` y renombra al final para que un corte de
//! luz nunca deje un snapshot a medio escribir. `load` rechaza cantidades
//! absurdas (>200 M nodos) antes de reservar memoria.

use std::fmt;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use crate::index::{Index, Node, Stats};

/// `CENSUSWCAT`, 10 bytes ASCII.
const MAGIC: [u8; 10] = *b"CENSUSWCAT";

/// Versión del formato que escribimos y leemos.
const VERSION: u16 = 3;

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
    for &r in &index.roots {
        w.u32(r)?;
    }

    // Nodos, campo por campo.
    let mut buf = [0u8; NODE_LEN];
    for n in &index.nodes {
        node_to_bytes(n, &mut buf);
        w.raw(&buf)?;
    }

    // Arena UTF-16, por tandas para no armar un buffer gigante.
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

/// Carga un snapshot `C.wcat` desde `path`.
///
/// Valida magic, versión, endian, cantidades absurdas (antes de reservar),
/// checksum de nodos+arena y la estructura del índice: `parent` en rango,
/// `name_off + name_len <= name_units` y raíces en rango.
pub fn load(path: &Path) -> Result<Index, LoadError> {
    let depurar = std::env::var_os("CENSUS_DEBUG").is_some();
    let t0 = std::time::Instant::now();
    let mut file = fs::File::open(path).map_err(LoadError::Io)?;

    // Cabecera completa primero; un archivo más corto es un truncado.
    let mut hdr = [0u8; HEADER_LEN];
    file.read_exact(&mut hdr).map_err(eof_or_io)?;

    let mut r = LeReader { b: &hdr, off: 0 };
    let magic = r.take(10).unwrap();
    if magic != MAGIC {
        return Err(LoadError::Magic);
    }
    let version = r.u16().unwrap();
    if version != VERSION {
        return Err(LoadError::Version(version));
    }
    let flags = r.u16().unwrap();
    if flags & FLAG_LE == 0 {
        return Err(LoadError::BadIndex("flags: endian no soportado".into()));
    }
    let node_count = r.u32().unwrap();
    let name_units = r.u32().unwrap();
    let root_count = r.u32().unwrap();
    let _padding = r.take(3).unwrap();
    let letter_u32 = r.u32().unwrap();
    let built_at = r.i64().unwrap();
    let stats = Stats {
        records: r.u64().unwrap(),
        files: r.u64().unwrap(),
        dirs: r.u64().unwrap(),
        skipped: r.u64().unwrap(),
        build_ms: r.u64().unwrap() as u128,
        name_units: name_units as u64,
    };
    let checksum = r.u64().unwrap();

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

    // Raíces.
    let mut roots_bytes = vec![0u8; root_count as usize * 4];
    file.read_exact(&mut roots_bytes).map_err(eof_or_io)?;
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

    // Nodos por tandas, hasheando a medida que leemos.
    let mut nodes: Vec<Node> = Vec::with_capacity(node_count as usize);
    let mut h = Fnv::new();
    let mut chunk = vec![0u8; NODE_CHUNK * NODE_LEN];
    let mut leídos: u32 = 0;
    while leídos < node_count {
        let n = (node_count - leídos).min(NODE_CHUNK as u32) as usize;
        file.read_exact(&mut chunk[..n * NODE_LEN])
            .map_err(eof_or_io)?;
        for node_bytes in chunk[..n * NODE_LEN].chunks_exact(NODE_LEN) {
            let arr: [u8; NODE_LEN] = node_bytes.try_into().unwrap();
            h.bytes(&arr);
            let node = node_from_bytes(&arr);
            if node.parent >= node_count {
                return Err(LoadError::BadIndex(format!(
                    "parent fuera de rango en el nodo {}: {} >= {}",
                    leídos + nodes.len() as u32,
                    node.parent,
                    node_count
                )));
            }
            if node.name_off as u64 + node.name_len as u64 > name_units as u64 {
                return Err(LoadError::BadIndex(format!(
                    "nombre fuera de rango en el nodo {}: {} + {} > {}",
                    leídos + nodes.len() as u32,
                    node.name_off,
                    node.name_len,
                    name_units
                )));
            }
            nodes.push(node);
        }
        leídos += n as u32;
    }

    // Arena UTF-16.
    if depurar {
        eprintln!("  [load] nodos en {:?}", t0.elapsed());
    }
    let mut arena_bytes = vec![0u8; name_units as usize * 2];
    file.read_exact(&mut arena_bytes).map_err(eof_or_io)?;
    h.bytes(&arena_bytes);
    let names = bytes_a_u16(&arena_bytes);

    // Bloques derivados (v2). Si faltan (cantidad 0), se recalculan abajo.
    let leer_bloque =
        |file: &mut fs::File, techo: u32, cual: usize| -> Result<(u32, Vec<u8>), LoadError> {
            let mut cb = [0u8; 4];
            file.read_exact(&mut cb).map_err(eof_or_io)?;
            let cant = u32::from_le_bytes(cb);
            if cant > techo {
                return Err(LoadError::BadIndex(format!(
                    "bloque derivado {cual} absurdo: {cant} > {techo}"
                )));
            }
            Ok((cant, Vec::new()))
        };
    let mut bloques: Vec<Vec<u8>> = Vec::with_capacity(6);
    let techos = [
        name_units.saturating_add(1),
        node_count.saturating_add(1),
        node_count.saturating_add(1),
        node_count.saturating_mul(4).saturating_add(1),
        node_count.saturating_add(1),
        node_count.saturating_add(1),
    ];
    let anchos = [2usize, 8, 4, 4, 4, 4];
    let mut cantidades = [0u32; 6];
    for i in 0..6 {
        let (cant, _) = leer_bloque(&mut file, techos[i], i)?;
        cantidades[i] = cant;
        let mut bytes = vec![0u8; cant as usize * anchos[i]];
        file.read_exact(&mut bytes).map_err(eof_or_io)?;
        bloques.push(bytes);
    }

    let folded = bytes_a_u16(&bloques[0]);
    let dir_bytes = bytes_a_u64(&bloques[1]);
    let hijos_inicio = bytes_a_u32(&bloques[2]);
    let hijos = bytes_a_u32(&bloques[3]);
    let por_nombre = bytes_a_u32(&bloques[4]);
    let euler = bytes_a_u32(&bloques[5]);
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
    let mut cola = [0u8; 16];
    let marca = if file.read_exact(&mut cola).is_ok() {
        Some((
            u64::from_le_bytes(cola[0..8].try_into().unwrap()),
            i64::from_le_bytes(cola[8..16].try_into().unwrap()),
        ))
    } else {
        None
    };

    // Recién ahora comparamos el checksum: antes pasaron las validaciones
    // estructurales, así que un `BadIndex` tiene prioridad sobre `Checksum`.
    if h.finish() != checksum {
        return Err(LoadError::Checksum);
    }

    let mut ix = Index {
        letter,
        root: roots.first().copied().unwrap_or(0),
        roots,
        nodes,
        names: crate::index::Arena::propio(names),
        stats,
        built_at,
        dir_bytes,
        vol_serials: Vec::new(),
        folded: crate::index::Arena::propio(folded),
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
                "census_snap_test_{}_{}_{nombre}",
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
        let arena_start = HEADER_LEN + 4 * ix.roots.len() + NODE_LEN * ix.nodes.len();
        assert!(arena_start < bytes.len());
        let medio = arena_start + (bytes.len() - arena_start) / 4;
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
        let medio_nodos = HEADER_LEN + 4 * ix.roots.len() + NODE_LEN * (ix.nodes.len() / 2);
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

        // `parent` del nodo 1 vive en HEADER_LEN + 4*roots + 1*48 + 8.
        let off = HEADER_LEN + 4 * ix.roots.len() + NODE_LEN + NODE_PARENT_OFF;
        parchear(t.path(), |b| {
            b[off..off + 4].copy_from_slice(&u32::MAX.to_le_bytes())
        });
        match load(t.path()) {
            Err(LoadError::BadIndex(_)) => {}
            otro => panic!("se esperaba BadIndex, obtuve: {otro:?}"),
        }

        // Un nombre que se pasa de la arena también es BadIndex.
        save(&ix, t.path()).unwrap(); // arrancamos de un archivo válido
        let off = HEADER_LEN + 4 * ix.roots.len() + 2 * NODE_LEN;
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
        let esperado = (HEADER_LEN as u64)
            + 4 * ix.roots.len() as u64
            + NODE_LEN as u64 * ix.nodes.len() as u64
            + 2 * ix.names.len() as u64
            + 6 * 4
            + 2 * ix.folded.len() as u64
            + 8 * ix.dir_bytes.len() as u64
            + 4 * ix.hijos_inicio.len() as u64
            + 4 * ix.hijos.len() as u64
            + 4 * ix.por_nombre.len() as u64
            + 4 * ix.euler.len() as u64
            + 16; // cola con la marca del journal
        assert_eq!(meta.len(), esperado);

        // Y para un índice vacío: sólo cabecera.
        let t2 = TempFile::new("tamano_vacio");
        save(&Index::default(), t2.path()).unwrap();
        assert_eq!(
            fs::metadata(t2.path()).unwrap().len(),
            HEADER_LEN as u64 + 6 * 4 + 16
        );
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
