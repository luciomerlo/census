//! Snapshot binario `.cnx` (v1) de macOS: **un archivo por `volume_id`**.
//!
//! §10: la letra o el punto de montaje no son identidad. Por eso no hay un
//! `.cnx` global, sino uno por volumen nombrado por su `volume_id`. Dos discos
//! externos que se alternan en `/Volumes/Backup` tienen distinto `DiskHwId`,
//! distinto `volume_id` y por lo tanto dos archivos: ninguno pisa al otro.
//!
//! # Formato (little-endian, campo por campo, sin `repr(C)`)
//!
//! Cabecera fija de [`HEADER_LEN`] (96) bytes:
//!
//! | Off | Tamaño | Campo |
//! |-----|--------|-------|
//! | 0   | 9      | magic `CENSUSCNX` (ASCII) |
//! | 9   | u16    | versión del formato (1) |
//! | 11  | u16    | flags (bit 0 = little-endian) |
//! | 13  | u8[3]  | relleno, siempre 0 |
//! | 16  | u32    | `node_count` |
//! | 20  | u32    | `name_bytes` (arena de nombres) |
//! | 24  | u32    | `folded_bytes` (arena plegada) |
//! | 28  | u32    | `volume_count` (v1: 1) |
//! | 32  | u32    | `root_count` |
//! | 36  | u32    | `header_block_len` |
//! | 40  | u64    | `body_len` |
//! | 48  | u64    | `content_hash` (FNV-1a 64 de `names ‖ folded`) |
//! | 56  | u64    | `body_checksum` (FNV-1a 64 de nodos ‖ names ‖ folded) |
//! | 64  | u64    | `header_block_checksum` (FNV-1a 64 del bloque variable) |
//! | 72  | i64    | `built_at` (segundos Unix) |
//! | 80  | u64    | `generation` |
//! | 88  | u64    | `total_len` (96 + bloque variable + cuerpo) |
//!
//! Después de la cabecera fija va el **bloque variable** (`header_block_len`
//! bytes). Todo texto es `u32` largo + bytes UTF-8, en este orden:
//!
//! 1. provenance: `pc_id`, `host`, `platform`, `arch`, `env`, `app_version`.
//! 2. `Marca` (`m1:<vol_uuid>:<event_id>`) como texto.
//! 3. por cada volumen: `volume_id`, `name`, `mount`, `fs`, `method`,
//!    `uuid[16]` crudo, `disk_hw_id` (`u8` 0/1 + texto), `disk_guid`,
//!    `serial`, `cloud_id`, `identity_state` (`u8`), `partition_offset` u64,
//!    `total` u64, `free` u64, `local` u8, `network` u8, `cloud` u8,
//!    `removable` u8, `possible_duplicate` u8, `nodes` u64.
//! 4. por cada raíz: `node_id` u32, `volume_index` u16 y la ruta como texto.
//!
//! Después, el **cuerpo** (`body_len` bytes):
//!
//! 1. `nodes`: `node_count` × 64 B — `name_off` u32, `name_len` u16,
//!    `flags` u16, `parent` u32, `size` u64, `alloc` u64, `mtime_ns` i64,
//!    `btime_ns` i64, `ino` u64, `gen` u32, `vol` u16, `nlink` u32,
//!    `reserved` u16 (0).
//! 2. `names`: arena UTF-8, `name_bytes` bytes.
//! 3. `folded`: arena plegada, `folded_bytes` bytes.
//!
//! Los índices derivados (`compacto`, CSR de hijos, `por_nombre`, Euler,
//! `dir_bytes`) **no** se guardan: se recalculan al cargar con
//! [`crate::index::compute_derived`], la misma función que usa el `Builder`.
//! Así un archivo con checksum válido pero invariantes raras no puede hacer
//! que la consulta lea fuera de rango, y no se reserva memoria según números
//! que vienen del archivo.
//!
//! Recalcular no es reindexar: no se toca el disco ni se vuelve a enumerar.
//!
//! # Robustez
//!
//! La carga valida, en este orden: tamaño real contra `total_len` declarado,
//! magic, versión, banderas, cantidades (`node_count` ≤ 200 M, coherencia de
//! `body_len`/`header_block_len` con el tamaño del archivo), los dos checksums
//! y `content_hash`, y por último cada nodo (`name_off + name_len` dentro de
//! las dos arenas, `parent` y `vol` dentro de rango) y cada raíz (id válido,
//! `parent == sí mismo`, nombre no vacío). Cualquier cosa rara devuelve
//! [`SnapshotError`]: nunca hay pánico ni lectura fuera de rango.
//!
//! `save` escribe a `<ruta>.tmp` y renombra al final, para que un corte de luz
//! no deje un `.cnx` a medio escribir.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use crate::index::{compute_derived, Index, Node};
use crate::types::{
    fnv1a64, IdentityState, Marca, Provenance, Volume, VolumeId, FLAG_DIR, FLAG_ROOT,
};

/// `CENSUSCNX`, 9 bytes ASCII. Distinto de `CENSUSWCAT` (Windows) y de `CNS1`
/// (Linux): cada crate tiene su snapshot nativo y el hub los guarda opacos.
const MAGIC: [u8; 9] = *b"CENSUSCNX";

/// Versión del formato que escribe este binario.
pub const FORMAT_VERSION: u16 = 1;

/// Extension de los snapshots de macOS.
pub const EXT: &str = "cnx";

/// Bit 0 de `flags`: los campos van en little-endian.
const FLAG_LE: u16 = 0x1;

/// Tamaño de la cabecera fija (ver la tabla del encabezado del módulo).
const HEADER_LEN: usize = 96;

/// Tamaño serializado de un nodo.
const NODE_LEN: usize = 64;

/// Cantidad de nodos que se considera absurda: se rechaza antes de reservar.
const MAX_NODES: u32 = 200_000_000;

/// Techo de volúmenes en un archivo. v1 escribe uno; el límite es defensivo.
const MAX_VOLUMES: u32 = 4_096;

/// Techo de raíces por archivo.
const MAX_ROOTS: u32 = 65_536;

/// Error de formato o de E/S al guardar o cargar un `.cnx`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SnapshotError {
    /// El archivo no mide lo que declara su cabecera (truncado o con cola).
    Truncated { declared: u64, actual: usize },
    /// El buffer se termina donde el formato pide más bytes. Es el error de
    /// bajo nivel del lector; `Truncated` es el del tamaño del archivo.
    Short { needed: usize, available: usize },
    /// El magic no es `CENSUSCNX`.
    Magic,
    /// Versión de formato que este binario no lee.
    Version(u16),
    /// El checksum del cuerpo o del bloque de cabecera no cierra.
    Checksum,
    /// `content_hash` no coincide con lo que hay en las arenas.
    ContentHash,
    /// Estructura inválida; el texto dice qué campo y por qué.
    Invalid(String),
    /// Error de E/S (abrir, escribir, renombrar).
    Io(String),
}

impl fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SnapshotError::Truncated { declared, actual } => write!(
                f,
                "snapshot truncado: la cabecera declara {declared} bytes y el archivo tiene {actual}"
            ),
            SnapshotError::Short { needed, available } => write!(
                f,
                "snapshot incompleto: hacían falta {needed} bytes y quedaban {available}"
            ),
            SnapshotError::Magic => {
                f.write_str("no es un snapshot de macOS (magic inválido, se esperaba CENSUSCNX)")
            }
            SnapshotError::Version(v) => write!(
                f,
                "versión de snapshot desconocida: {v} (este census lee v{FORMAT_VERSION})"
            ),
            SnapshotError::Checksum => f.write_str(
                "checksum del snapshot no coincide: el archivo está alterado o incompleto",
            ),
            SnapshotError::ContentHash => {
                f.write_str("content_hash del snapshot no coincide con las arenas de nombres")
            }
            SnapshotError::Invalid(msg) => write!(f, "snapshot inválido: {msg}"),
            SnapshotError::Io(msg) => write!(f, "error de E/S en el snapshot: {msg}"),
        }
    }
}

impl std::error::Error for SnapshotError {}

impl From<std::io::Error> for SnapshotError {
    fn from(e: std::io::Error) -> Self {
        SnapshotError::Io(e.to_string())
    }
}

/// Un snapshot cargado: el índice de **un** volumen y los datos del volumen.
#[derive(Clone, Debug)]
pub struct Snapshot {
    pub index: Index,
    /// Volumen dueño del archivo (el que dio `volume_id` y `mount`).
    pub volume: Volume,
}

impl Snapshot {
    /// Envuelve un índice de un solo volumen.
    pub fn from_index(index: Index) -> Result<Self, SnapshotError> {
        if index.volumes.len() != 1 {
            return Err(SnapshotError::Invalid(format!(
                "un .cnx guarda exactamente un volumen y el índice tiene {}; usá save_per_volume",
                index.volumes.len()
            )));
        }
        let volume = index.volumes[0].clone();
        Ok(Snapshot { index, volume })
    }

    pub fn volume_id(&self) -> &str {
        &self.volume.id
    }

    pub fn mount(&self) -> &str {
        &self.volume.mount
    }

    pub fn marca(&self) -> &Marca {
        &self.index.marca
    }

    /// Serializa el snapshot completo.
    pub fn to_bytes(&self) -> Result<Vec<u8>, SnapshotError> {
        let header_block = self.header_block_bytes()?;
        let body = self.body_bytes()?;
        let total = HEADER_LEN
            .checked_add(header_block.len())
            .and_then(|v| v.checked_add(body.len()))
            .ok_or_else(|| SnapshotError::Invalid("el snapshot no entra en memoria".into()))?;

        let mut out = Vec::with_capacity(total);
        out.extend_from_slice(&MAGIC);
        put_u16(&mut out, FORMAT_VERSION);
        put_u16(&mut out, FLAG_LE);
        out.extend_from_slice(&[0u8; 3]);
        put_u32(&mut out, to_u32(self.index.nodes.len(), "node_count")?);
        put_u32(&mut out, to_u32(self.index.names.len(), "name_bytes")?);
        put_u32(&mut out, to_u32(self.index.folded.len(), "folded_bytes")?);
        put_u32(
            &mut out,
            to_u32(self.index.volumes.len(), "volume_count")?,
        );
        put_u32(&mut out, to_u32(self.index.roots.len(), "root_count")?);
        put_u32(&mut out, to_u32(header_block.len(), "header_block_len")?);
        put_u64(&mut out, body.len() as u64);
        put_u64(&mut out, self.index.content_hash);
        put_u64(&mut out, fnv1a64(&[&body]));
        put_u64(&mut out, fnv1a64(&[&header_block]));
        put_i64(&mut out, self.index.built_at);
        put_u64(&mut out, self.index.generation);
        put_u64(&mut out, total as u64);
        debug_assert_eq!(out.len(), HEADER_LEN);
        out.extend_from_slice(&header_block);
        out.extend_from_slice(&body);
        Ok(out)
    }

    /// Escribe el snapshot de forma atómica (`tmp` + `rename`). Devuelve los
    /// bytes escritos.
    pub fn save(&self, path: &Path) -> Result<u64, SnapshotError> {
        let bytes = self.to_bytes()?;
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut tmp = path.to_path_buf();
        let nombre = tmp
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "snapshot.cnx".into());
        tmp.set_file_name(format!("{nombre}.tmp"));
        fs::write(&tmp, &bytes)?;
        fs::rename(&tmp, path)?;
        Ok(bytes.len() as u64)
    }

    /// Carga desde memoria. Nunca hace pánico ni lee fuera del buffer.
    pub fn load(bytes: &[u8]) -> Result<Self, SnapshotError> {
        if let Some(declared) = read_total_len(bytes) {
            if declared as usize != bytes.len() {
                return Err(SnapshotError::Truncated {
                    declared,
                    actual: bytes.len(),
                });
            }
        }
        let mut r = Reader::new(bytes);
        let magic = r.bytes(MAGIC.len())?;
        if magic != MAGIC {
            return Err(SnapshotError::Magic);
        }
        let version = r.u16()?;
        if version != FORMAT_VERSION {
            return Err(SnapshotError::Version(version));
        }
        let flags = r.u16()?;
        if flags & FLAG_LE == 0 {
            return Err(SnapshotError::Invalid(
                "el snapshot no declara little-endian".into(),
            ));
        }
        r.skip(3)?; // relleno
        let node_count = r.u32()?;
        let name_bytes = r.u32()?;
        let folded_bytes = r.u32()?;
        let volume_count = r.u32()?;
        let root_count = r.u32()?;
        let header_block_len = r.u32()?;
        let body_len = r.u64()?;
        let content_hash = r.u64()?;
        let body_checksum = r.u64()?;
        let header_block_checksum = r.u64()?;
        let built_at = r.i64()?;
        let generation = r.u64()?;
        let total_len = r.u64()?;
        debug_assert_eq!(r.pos, HEADER_LEN);

        // Cantidades antes de reservar memoria: el orden importa.
        if node_count > MAX_NODES {
            return Err(SnapshotError::Invalid(format!(
                "node_count absurdo: {node_count}"
            )));
        }
        if volume_count == 0 || volume_count > MAX_VOLUMES {
            return Err(SnapshotError::Invalid(format!(
                "volume_count fuera de rango: {volume_count}"
            )));
        }
        if root_count == 0 || root_count > MAX_ROOTS {
            return Err(SnapshotError::Invalid(format!(
                "root_count fuera de rango: {root_count}"
            )));
        }
        let node_bytes = (node_count as usize)
            .checked_mul(NODE_LEN)
            .ok_or_else(|| SnapshotError::Invalid("node_count * 64 desborda".into()))?;
        let esperado_body = node_bytes
            .checked_add(name_bytes as usize)
            .and_then(|v| v.checked_add(folded_bytes as usize))
            .ok_or_else(|| SnapshotError::Invalid("body_len desborda".into()))?;
        if body_len != esperado_body as u64 {
            return Err(SnapshotError::Invalid(format!(
                "body_len declara {body_len} y los campos suman {esperado_body}"
            )));
        }
        let esperado_total = (HEADER_LEN as u64)
            .checked_add(header_block_len as u64)
            .and_then(|v| v.checked_add(body_len))
            .ok_or_else(|| SnapshotError::Invalid("total_len desborda".into()))?;
        if total_len != esperado_total || total_len as usize != bytes.len() {
            return Err(SnapshotError::Truncated {
                declared: esperado_total,
                actual: bytes.len(),
            });
        }

        let header_block = r.bytes(header_block_len as usize)?;
        let body = r.bytes(body_len as usize)?;
        if fnv1a64(&[header_block]) != header_block_checksum {
            return Err(SnapshotError::Checksum);
        }
        if fnv1a64(&[body]) != body_checksum {
            return Err(SnapshotError::Checksum);
        }

        // Bloque variable.
        let mut h = Reader::new(header_block);
        let provenance = Provenance {
            pc_id: h.string()?,
            host: h.string()?,
            platform: h.string()?,
            arch: h.string()?,
            env: h.string()?,
            app_version: h.string()?,
        };
        let marca = Marca(h.string()?);
        let mut volumes = Vec::with_capacity(volume_count as usize);
        for _ in 0..volume_count {
            volumes.push(read_volume(&mut h)?);
        }
        let mut roots: Vec<u32> = Vec::with_capacity(root_count as usize);
        let mut root_paths: Vec<(usize, String)> = Vec::with_capacity(root_count as usize);
        for _ in 0..root_count {
            let node_id = h.u32()?;
            let vol = h.u16()? as usize;
            let path = h.string()?;
            if vol >= volumes.len() {
                return Err(SnapshotError::Invalid(format!(
                    "raíz con volume_index {vol} de {} volúmenes",
                    volumes.len()
                )));
            }
            roots.push(node_id);
            root_paths.push((vol, path));
        }
        if h.remaining() != 0 {
            return Err(SnapshotError::Invalid(format!(
                "sobran {} bytes en el bloque de cabecera",
                h.remaining()
            )));
        }

        // Cuerpo: primero los nodos, después las dos arenas (ese es el orden
        // en que `body_bytes` las escribe).
        let mut c = Reader::new(body);
        let mut nodes: Vec<Node> = Vec::with_capacity(node_count as usize);
        for _ in 0..node_count {
            nodes.push(read_node(&mut c)?);
        }
        let names = c.bytes(name_bytes as usize)?.to_vec();
        let folded = c.bytes(folded_bytes as usize)?.to_vec();
        if c.remaining() != 0 {
            return Err(SnapshotError::Invalid(format!(
                "sobran {} bytes en el cuerpo",
                c.remaining()
            )));
        }

        if fnv1a64(&[&names, &folded]) != content_hash {
            return Err(SnapshotError::ContentHash);
        }
        for (i, n) in nodes.iter().enumerate() {
            let off = n.name_off as usize;
            let len = n.name_len as usize;
            if off
                .checked_add(len)
                .is_none_or(|end| end > names.len() || end > folded.len())
            {
                return Err(SnapshotError::Invalid(format!(
                    "el nombre del nodo {i} apunta fuera de la arena"
                )));
            }
            if n.parent as usize >= nodes.len() {
                return Err(SnapshotError::Invalid(format!(
                    "el nodo {i} tiene parent {} de {}",
                    n.parent,
                    nodes.len()
                )));
            }
            if n.vol as usize >= volumes.len() {
                return Err(SnapshotError::Invalid(format!(
                    "el nodo {i} apunta al volumen {} de {}",
                    n.vol,
                    volumes.len()
                )));
            }
        }
        for (i, &id) in roots.iter().enumerate() {
            let Some(n) = nodes.get(id as usize) else {
                return Err(SnapshotError::Invalid(format!(
                    "la raíz {i} apunta al nodo {id} de {}",
                    nodes.len()
                )));
            };
            if n.parent != id {
                return Err(SnapshotError::Invalid(format!(
                    "la raíz {i} (nodo {id}) no es su propio padre"
                )));
            }
            if n.name_len == 0 {
                return Err(SnapshotError::Invalid(format!("la raíz {i} no tiene ruta")));
            }
        }

        // Raíces por volumen, con respaldo si el bloque variable no las trajo.
        let mut roots_de: Vec<Vec<String>> = vec![Vec::new(); volumes.len()];
        for (vol_idx, path) in &root_paths {
            roots_de[*vol_idx].push(path.clone());
        }
        for (i, v) in volumes.iter_mut().enumerate() {
            if roots_de[i].is_empty() {
                for &id in &roots {
                    let n = &nodes[id as usize];
                    if n.vol as usize != i {
                        continue;
                    }
                    let a = n.name_off as usize;
                    let b = a + n.name_len as usize;
                    roots_de[i].push(String::from_utf8_lossy(&names[a..b]).into_owned());
                }
            }
            v.roots = std::mem::take(&mut roots_de[i]);
        }
        // Los índices derivados se recalculan acá, con la misma función del
        // `Builder`: un archivo con checksum válido pero invariantes raras no
        // puede dejar la consulta leyendo fuera de rango.
        let d = compute_derived(&nodes, &folded, &roots);
        let mut stats = crate::types::Stats::default();
        for n in &nodes {
            if n.flags & FLAG_DIR != 0 {
                stats.dirs += 1;
            } else {
                stats.files += 1;
            }
            stats.records += 1;
        }
        stats.name_bytes = names.len() as u64;

        let volume = volumes[0].clone();
        let index = Index {
            roots,
            nodes,
            names,
            folded,
            compacto: d.compacto,
            dir_bytes: d.dir_bytes,
            hijos_inicio: d.hijos_inicio,
            hijos: d.hijos,
            por_nombre: d.por_nombre,
            euler: d.euler,
            tin: d.tin,
            tout: d.tout,
            volumes,
            built_at,
            generation,
            marca,
            provenance,
            stats,
            content_hash,
        };
        Ok(Snapshot { index, volume })
    }

    /// Carga desde un archivo.
    pub fn load_file(path: &Path) -> Result<Self, SnapshotError> {
        let bytes = fs::read(path)?;
        Snapshot::load(&bytes)
    }

    /// Remapea el prefijo de montaje: mismo `volume_id`, otro punto de montaje.
    ///
    /// §10.2: se detecta el mismo volumen en otra ruta y **se remapea el
    /// prefijo**; no se reindexa. Sólo se reescribe el nombre de los nodos
    /// raíz; los nodos, la `Marca` y los archivos no cambian. `content_hash`
    /// sí se recalcula, porque cubre las arenas.
    ///
    /// Devuelve las raíces nuevas; vacío si el montaje no cambió.
    pub fn remap_mount(&mut self, new_mount: &str) -> Vec<String> {
        let old = crate::index::normalize_dir(&self.volume.mount);
        let new = crate::index::normalize_dir(new_mount);
        if old == new {
            return Vec::new();
        }
        let roots = self.index.roots.clone();
        let mut rutas: Vec<String> = Vec::with_capacity(roots.len());
        let mut hubo_cambio = false;
        for id in roots {
            let before = self.index.path_of(id);
            let after = remap_prefix(&before, &old, &new);
            if after != before {
                self.index.rename_node(id, &after);
                hubo_cambio = true;
            }
            // Se conservan todas las raíces, también las que no colgaban del
            // montaje viejo (un volumen de nube puede tener varias).
            rutas.push(after);
        }
        if !hubo_cambio {
            return Vec::new();
        }
        self.volume.mount = new.clone();
        self.volume.roots = rutas.clone();
        if let Some(v) = self.index.volumes.first_mut() {
            v.mount = new;
            v.roots = rutas.clone();
        }
        self.index.recompute_content_hash();
        rutas
    }

    /// Bloque de cabecera variable (provenance, `Marca`, volúmenes, raíces).
    fn header_block_bytes(&self) -> Result<Vec<u8>, SnapshotError> {
        let mut b = Vec::with_capacity(512);
        let p = &self.index.provenance;
        for s in [
            &p.pc_id,
            &p.host,
            &p.platform,
            &p.arch,
            &p.env,
            &p.app_version,
        ] {
            put_str(&mut b, s);
        }
        put_str(&mut b, self.index.marca.as_str());
        for v in &self.index.volumes {
            put_str(&mut b, &v.id);
            put_str(&mut b, &v.name);
            put_str(&mut b, &v.mount);
            put_str(&mut b, &v.fs);
            put_str(&mut b, &v.method);
            b.extend_from_slice(&v.uuid);
            put_opt_str(&mut b, v.disk_hw_id.as_deref());
            put_opt_str(&mut b, v.disk_guid.as_deref());
            put_opt_str(&mut b, v.serial.as_deref());
            put_opt_str(&mut b, v.cloud_id.as_deref());
            b.push(identity_code(v.identity_state));
            put_u64(&mut b, v.partition_offset);
            put_u64(&mut b, v.total);
            put_u64(&mut b, v.free);
            b.push(v.local as u8);
            b.push(v.network as u8);
            b.push(v.cloud as u8);
            b.push(v.removable as u8);
            b.push(v.possible_duplicate as u8);
            put_u64(&mut b, v.nodes);
        }
        for &id in &self.index.roots {
            let vol = self.index.nodes.get(id as usize).map(|n| n.vol).unwrap_or(0);
            put_u32(&mut b, id);
            put_u16(&mut b, vol);
            put_str(&mut b, &self.index.name_string_lossy(id));
        }
        Ok(b)
    }

    /// Cuerpo: nodos, arena de nombres y arena plegada.
    fn body_bytes(&self) -> Result<Vec<u8>, SnapshotError> {
        let mut b = Vec::with_capacity(
            self.index.nodes.len() * NODE_LEN + self.index.names.len() + self.index.folded.len(),
        );
        for n in &self.index.nodes {
            put_u32(&mut b, n.name_off);
            put_u16(&mut b, n.name_len);
            put_u16(&mut b, n.flags);
            put_u32(&mut b, n.parent);
            put_u64(&mut b, n.size);
            put_u64(&mut b, n.alloc);
            put_i64(&mut b, n.mtime_ns);
            put_i64(&mut b, n.btime_ns);
            put_u64(&mut b, n.ino);
            put_u32(&mut b, n.gen);
            put_u16(&mut b, n.vol);
            put_u32(&mut b, n.nlink);
            put_u16(&mut b, 0);
        }
        b.extend_from_slice(&self.index.names);
        b.extend_from_slice(&self.index.folded);
        Ok(b)
    }
}

/// Archivo `.cnx` escrito por [`save_per_volume`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SavedFile {
    pub path: PathBuf,
    pub volume_id: String,
    pub bytes: u64,
}

/// Nombre estable del `.cnx` de un volumen dentro de `dir`.
///
/// Lleva el `volume_id` saneado (para que se vea de qué volumen es) y un hash
/// de 32 bits del id completo, porque `sanitize` puede juntar dos ids
/// distintos. La identidad real se lee siempre de la cabecera, nunca del
/// nombre del archivo.
pub fn file_for(dir: &Path, volume_id: &str) -> PathBuf {
    let h = fnv1a64(&[volume_id.as_bytes()]);
    dir.join(format!(
        "{}_{:08x}.{EXT}",
        crate::config::sanitize(volume_id),
        (h & 0xffff_ffff) as u32
    ))
}

/// Guarda un `.cnx` por cada volumen del índice. Un volumen que falle no
/// impide guardar los demás: los errores vuelven para avisar.
///
/// La `Marca` se ajusta al volumen: si la del índice ya es `m1:<vol_uuid>:…`
/// del volumen que se guarda se respeta; si no, se escribe
/// `m1:<vol_uuid>:0`, que significa "reindexado completo, sin `eventId`
/// todavía" (FSEvents es el pendiente 4 de `PENDIENTE.md`).
pub fn save_per_volume(ix: &Index, dir: &Path) -> (Vec<SavedFile>, Vec<String>) {
    let mut ok = Vec::new();
    let mut fallos = Vec::new();
    for (i, vol) in ix.volumes.iter().enumerate() {
        if !ix.nodes.iter().any(|n| n.vol as usize == i) {
            continue;
        }
        let mut sub = subset_for_volume(ix, i);
        sub.marca = per_volume_marca(&ix.marca, vol);
        let snap = match Snapshot::from_index(sub) {
            Ok(s) => s,
            Err(e) => {
                fallos.push(format!("{}: {e}", vol.id));
                continue;
            }
        };
        let path = file_for(dir, &vol.id);
        match snap.save(&path) {
            Ok(bytes) => ok.push(SavedFile {
                path,
                volume_id: vol.id.to_string(),
                bytes,
            }),
            Err(e) => fallos.push(format!("{}: {e}", vol.id)),
        }
    }
    (ok, fallos)
}

/// `Marca` que le corresponde a un volumen dentro de un índice multi-volumen.
fn per_volume_marca(marca: &Marca, vol: &Volume) -> Marca {
    let uuid = crate::types::uuid_hex(&vol.uuid);
    let mut it = marca.as_str().split(':');
    if it.next() == Some("m1") && it.next() == Some(uuid.as_str()) {
        return marca.clone();
    }
    Marca::mac(vol.uuid, 0)
}

/// Sub-índice con los nodos de un solo volumen, recalculado desde cero.
///
/// Se copia campo por campo (no se reusa el `Builder`) para conservar las
/// banderas originales del nodo, incluida `FLAG_ROOT`, y para no arrastrar
/// offsets de una arena compartida con otros volúmenes.
fn subset_for_volume(ix: &Index, vol_idx: usize) -> Index {
    let mut map: HashMap<u32, u32> = HashMap::new();
    let mut nodes: Vec<Node> = Vec::new();
    let mut names: Vec<u8> = Vec::new();
    let mut folded: Vec<u8> = Vec::new();
    let mut roots: Vec<u32> = Vec::new();
    let mut files = 0u64;
    let mut dirs = 0u64;
    for (id, n) in ix.nodes.iter().enumerate() {
        if n.vol as usize != vol_idx {
            continue;
        }
        let es_raiz = n.parent as usize == id;
        if !es_raiz && !map.contains_key(&n.parent) {
            // El padre es de otro volumen (no debería pasar): se descarta el
            // huérfano en vez de colgarlo de una raíz ajena.
            continue;
        }
        let new_id = nodes.len() as u32;
        map.insert(id as u32, new_id);
        let name = ix.name(id as u32);
        let fold = ix.folded_name(id as u32);
        let mut node = *n;
        node.name_off = names.len() as u32;
        node.name_len = name.len().min(u16::MAX as usize) as u16;
        node.vol = 0;
        node.parent = if es_raiz {
            new_id
        } else {
            *map.get(&n.parent).unwrap_or(&0)
        };
        names.extend_from_slice(name);
        folded.extend_from_slice(fold);
        if es_raiz {
            node.flags |= FLAG_ROOT;
            roots.push(new_id);
        }
        if node.flags & FLAG_DIR != 0 {
            dirs += 1;
        } else {
            files += 1;
        }
        nodes.push(node);
    }
    let d = compute_derived(&nodes, &folded, &roots);
    let stats = crate::types::Stats {
        records: nodes.len() as u64,
        files,
        dirs,
        skipped: ix.stats.skipped,
        name_bytes: names.len() as u64,
        build_ms: 0,
    };
    let content_hash = fnv1a64(&[&names, &folded]);
    Index {
        roots,
        nodes,
        names,
        folded,
        compacto: d.compacto,
        dir_bytes: d.dir_bytes,
        hijos_inicio: d.hijos_inicio,
        hijos: d.hijos,
        por_nombre: d.por_nombre,
        euler: d.euler,
        tin: d.tin,
        tout: d.tout,
        volumes: vec![ix.volumes[vol_idx].clone()],
        built_at: ix.built_at,
        generation: ix.generation,
        marca: ix.marca.clone(),
        provenance: ix.provenance.clone(),
        stats,
        content_hash,
    }
}

/// Calcula la ruta nueva de `path` al cambiar `old` por `new`.
fn remap_prefix(path: &str, old: &str, new: &str) -> String {
    if path == old {
        return new.to_string();
    }
    if old == "/" {
        return format!("{}{}", new.trim_end_matches('/'), path);
    }
    match path.strip_prefix(old) {
        Some(rest) if rest.starts_with('/') => format!("{}{}", new.trim_end_matches('/'), rest),
        _ => path.to_string(),
    }
}

/// Código estable de `IdentityState` (no depende del orden del `enum`).
fn identity_code(s: IdentityState) -> u8 {
    match s {
        IdentityState::Ok => 0,
        IdentityState::SinIdentidad => 1,
        IdentityState::Cloud => 2,
        IdentityState::PosibleDuplicado => 3,
    }
}

fn identity_from_code(c: u8) -> Result<IdentityState, SnapshotError> {
    match c {
        0 => Ok(IdentityState::Ok),
        1 => Ok(IdentityState::SinIdentidad),
        2 => Ok(IdentityState::Cloud),
        3 => Ok(IdentityState::PosibleDuplicado),
        other => Err(SnapshotError::Invalid(format!(
            "identity_state desconocido: {other}"
        ))),
    }
}

/// Bytes totales declarados, leídos sin validar nada más. Sirve para
/// distinguir "truncado" de "basura" con un mensaje útil.
fn read_total_len(bytes: &[u8]) -> Option<u64> {
    if bytes.len() < HEADER_LEN || bytes.get(0..MAGIC.len())? != MAGIC {
        return None;
    }
    if bytes.get(9..11) != Some(&FORMAT_VERSION.to_le_bytes()[..]) {
        return None;
    }
    let raw = bytes.get(88..96)?;
    Some(u64::from_le_bytes(raw.try_into().ok()?))
}

fn read_volume(r: &mut Reader<'_>) -> Result<Volume, SnapshotError> {
    let id: VolumeId = VolumeId::from(r.string()?.as_str());
    let name = r.string()?;
    let mount = r.string()?;
    let fs = r.string()?;
    let method = r.string()?;
    let mut uuid = [0u8; 16];
    uuid.copy_from_slice(r.bytes(16)?);
    let disk_hw_id = r.opt_string()?;
    let disk_guid = r.opt_string()?;
    let serial = r.opt_string()?;
    let cloud_id = r.opt_string()?;
    let identity_state = identity_from_code(r.u8()?)?;
    let partition_offset = r.u64()?;
    let total = r.u64()?;
    let free = r.u64()?;
    let local = r.u8()? != 0;
    let network = r.u8()? != 0;
    let cloud = r.u8()? != 0;
    let removable = r.u8()? != 0;
    let possible_duplicate = r.u8()? != 0;
    let nodes = r.u64()?;
    Ok(Volume {
        id,
        disk_hw_id,
        disk_guid,
        serial,
        model: None,
        partition_offset,
        identity_state,
        cloud_id,
        possible_duplicate,
        uuid,
        name,
        mount,
        fs,
        total,
        free,
        local,
        network,
        cloud,
        removable,
        dev: 0,
        method,
        roots: Vec::new(),
        nodes,
    })
}

fn read_node(r: &mut Reader<'_>) -> Result<Node, SnapshotError> {
    let name_off = r.u32()?;
    let name_len = r.u16()?;
    let flags = r.u16()?;
    let parent = r.u32()?;
    let size = r.u64()?;
    let alloc = r.u64()?;
    let mtime_ns = r.i64()?;
    let btime_ns = r.i64()?;
    let ino = r.u64()?;
    let gen = r.u32()?;
    let vol = r.u16()?;
    let nlink = r.u32()?;
    r.skip(2)?; // reservado
    Ok(Node {
        name_off,
        name_len,
        flags,
        parent,
        size,
        alloc,
        mtime_ns,
        btime_ns,
        ino,
        gen,
        vol,
        nlink,
    })
}

/// Lector con límites: toda lectura se valida contra el buffer.
struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Reader { buf, pos: 0 }
    }

    fn remaining(&self) -> usize {
        self.buf.len().saturating_sub(self.pos)
    }

    fn bytes(&mut self, n: usize) -> Result<&'a [u8], SnapshotError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| SnapshotError::Invalid("tamaño imposible en el snapshot".into()))?;
        let out = self.buf.get(self.pos..end).ok_or(SnapshotError::Short {
            needed: n,
            available: self.remaining(),
        })?;
        self.pos = end;
        Ok(out)
    }

    fn skip(&mut self, n: usize) -> Result<(), SnapshotError> {
        self.bytes(n).map(|_| ())
    }

    fn u8(&mut self) -> Result<u8, SnapshotError> {
        Ok(self.bytes(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, SnapshotError> {
        let b = self.bytes(2)?;
        Ok(u16::from_le_bytes([b[0], b[1]]))
    }

    fn u32(&mut self) -> Result<u32, SnapshotError> {
        let b = self.bytes(4)?;
        Ok(u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn u64(&mut self) -> Result<u64, SnapshotError> {
        let mut a = [0u8; 8];
        a.copy_from_slice(self.bytes(8)?);
        Ok(u64::from_le_bytes(a))
    }

    fn i64(&mut self) -> Result<i64, SnapshotError> {
        Ok(self.u64()? as i64)
    }

    fn string(&mut self) -> Result<String, SnapshotError> {
        let len = self.u32()? as usize;
        let raw = self.bytes(len)?;
        String::from_utf8(raw.to_vec())
            .map_err(|_| SnapshotError::Invalid("texto no UTF-8 en la cabecera".into()))
    }

    fn opt_string(&mut self) -> Result<Option<String>, SnapshotError> {
        match self.u8()? {
            0 => Ok(None),
            1 => Ok(Some(self.string()?)),
            other => Err(SnapshotError::Invalid(format!(
                "bandera de texto opcional inválida: {other}"
            ))),
        }
    }
}

fn to_u32(n: usize, campo: &str) -> Result<u32, SnapshotError> {
    u32::try_from(n).map_err(|_| SnapshotError::Invalid(format!("{campo} no entra en u32: {n}")))
}

fn put_u16(b: &mut Vec<u8>, v: u16) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_u32(b: &mut Vec<u8>, v: u32) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_u64(b: &mut Vec<u8>, v: u64) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_i64(b: &mut Vec<u8>, v: i64) {
    b.extend_from_slice(&v.to_le_bytes());
}

fn put_str(b: &mut Vec<u8>, s: &str) {
    put_u32(b, s.len() as u32);
    b.extend_from_slice(s.as_bytes());
}

fn put_opt_str(b: &mut Vec<u8>, s: Option<&str>) {
    match s {
        Some(s) => {
            b.push(1);
            put_str(b, s);
        }
        None => b.push(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Builder;
    use crate::types::{disk_hw_id, volume_identity, Encoding, Entrada, FileId, Kind};

    fn volume(mount: &str, serial: &str) -> Volume {
        let hw = disk_hw_id(Some(serial), "Seagate Backup+", 4_000_000, None).unwrap();
        Volume {
            id: volume_identity(&hw, 0, &format!("fs-{serial}")),
            disk_hw_id: Some(hw),
            serial: Some(serial.into()),
            model: Some("Seagate Backup+".into()),
            uuid: [7u8; 16],
            name: "Backup".into(),
            mount: mount.into(),
            fs: "apfs".into(),
            total: 4_000_000,
            free: 1_000_000,
            local: true,
            method: "walk".into(),
            roots: vec![mount.into()],
            ..Default::default()
        }
    }

    fn entrada(vol: &Volume, ino: u64, parent: u64, name: &str, dir: bool, size: u64) -> Entrada {
        let vid: VolumeId = vol.id.clone();
        let kind = if dir { Kind::Dir } else { Kind::File };
        Entrada {
            id: FileId::new(vid.clone(), ino, 0, 1_000 + ino as i64),
            parent_id: FileId::new(vid, parent, 0, 1_000 + parent as i64),
            dev_id: 1,
            kind,
            size,
            alloc: size,
            mtime_ns: 1_700_000_000_000_000_000,
            btime_ns: 1_000 + ino as i64,
            nlink: 1,
            flags: 0,
            name: name.as_bytes().to_vec(),
            encoding: Encoding::Utf8,
        }
    }

    /// Índice de un volumen con la forma real: el nodo raíz lleva la ruta
    /// completa del montaje y los hijos sólo el nombre.
    fn indice(mount: &str, serial: &str) -> Index {
        let v = volume(mount, serial);
        let mut b = Builder::new(vec![v.clone()]);
        b.push_batch(&[entrada(&v, 1, 1, mount, true, 0)]);
        b.push_batch(&[entrada(&v, 2, 1, "Documentos", true, 0)]);
        b.push_batch(&[
            entrada(&v, 10, 2, "informe.pdf", false, 4096),
            entrada(&v, 11, 2, "Ñandú.txt", false, 10),
            entrada(&v, 12, 1, "foto.jpg", false, 2048),
        ]);
        b.finish(
            1_700_000_000,
            3,
            Marca("m1:07070707070707070707070707070707:42".into()),
            Provenance {
                pc_id: "pc-mac-1".into(),
                host: "mac-de-prueba".into(),
                platform: "macos".into(),
                arch: "aarch64".into(),
                env: "prueba".into(),
                app_version: "0.1.0".into(),
            },
        )
    }

    fn snap(mount: &str, serial: &str) -> Snapshot {
        Snapshot::from_index(indice(mount, serial)).expect("un volumen")
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("census-cnx-{}-{tag}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("crear dir temporal");
        dir
    }

    #[test]
    fn header_has_provenance_marca_and_checksums() {
        let s = snap("/Volumes/A", "SN-A");
        let bytes = s.to_bytes().unwrap();
        assert_eq!(&bytes[0..9], b"CENSUSCNX");
        assert_eq!(u16::from_le_bytes([bytes[9], bytes[10]]), FORMAT_VERSION);
        // El tamaño declarado es el real.
        let total = u64::from_le_bytes(bytes[88..96].try_into().unwrap());
        assert_eq!(total as usize, bytes.len());
        let c = Snapshot::load(&bytes).unwrap();
        assert_eq!(c.index.provenance.pc_id, "pc-mac-1");
        assert_eq!(c.index.provenance.host, "mac-de-prueba");
        assert_eq!(c.index.provenance.arch, "aarch64");
        assert_eq!(c.index.provenance.app_version, "0.1.0");
        assert_eq!(
            c.index.marca.as_str(),
            "m1:07070707070707070707070707070707:42"
        );
        assert_eq!(c.index.generation, 3);
        assert_eq!(c.index.content_hash, s.index.content_hash);
        assert_eq!(c.volume_id(), s.volume.id.to_string());
        assert_eq!(c.mount(), "/Volumes/A");
        assert_eq!(c.volume.roots, vec!["/Volumes/A".to_string()]);
        // El `volume_id` de la cabecera es 16 hex sin prefijo (§10).
        assert_eq!(c.volume_id().len(), 16);
    }

    #[test]
    fn roundtrip_keeps_nodes_paths_and_flags() {
        let s = snap("/Volumes/A", "SN-A");
        let bytes = s.to_bytes().unwrap();
        let c = Snapshot::load(&bytes).unwrap();
        assert_eq!(c.index.nodes.len(), s.index.nodes.len());
        assert_eq!(c.index.names, s.index.names);
        assert_eq!(c.index.folded, s.index.folded);
        assert_eq!(c.index.roots.len(), 1);
        assert_eq!(c.index.stats.files, 3);
        assert_eq!(c.index.stats.dirs, 2);
        let doc = c.index.find_dir("/Volumes/A/Documentos").unwrap();
        let mut nombres: Vec<String> = c
            .index
            .children(doc)
            .iter()
            .map(|&id| c.index.name_string_lossy(id))
            .collect();
        nombres.sort();
        assert_eq!(
            nombres,
            vec!["informe.pdf".to_string(), "Ñandú.txt".to_string()]
        );
        // El plegado del acento viaja igual que en el índice original.
        assert!(c
            .index
            .children(doc)
            .iter()
            .any(|&id| c.index.folded_name(id) == "ñandú.txt".as_bytes()));
        // `FLAG_ROOT` se conserva y no se inventan otras banderas.
        let root = c.index.roots[0];
        assert_eq!(c.index.nodes[root as usize].flags & FLAG_ROOT, FLAG_ROOT);
        assert_eq!(
            c.index.nodes[root as usize].flags,
            s.index.nodes[s.index.roots[0] as usize].flags
        );
    }

    #[test]
    fn all_truncations_are_errors() {
        let bytes = snap("/Volumes/A", "SN-A").to_bytes().unwrap();
        for n in 0..bytes.len() {
            match Snapshot::load(&bytes[..n]) {
                Ok(_) => panic!("una truncación a {n} bytes se aceptó como válida"),
                Err(e) => assert!(!e.to_string().is_empty()),
            }
        }
        assert!(Snapshot::load(&bytes).is_ok());
    }

    #[test]
    fn deterministic_garbage_never_panics() {
        let original = snap("/Volumes/A", "SN-A").to_bytes().unwrap();
        let mut estado: u64 = 0x9e37_79b9_7f4a_7c15;
        let mut azar = move || {
            estado ^= estado << 13;
            estado ^= estado >> 7;
            estado ^= estado << 17;
            estado
        };
        for caso in 0..128u32 {
            let buf: Vec<u8> = match caso % 4 {
                // Basura pura, de largo variable.
                0 => {
                    let n = (azar() % 512) as usize;
                    (0..n).map(|_| azar() as u8).collect()
                }
                // Basura con magic y versión válidos: cae en la validación.
                1 => {
                    let mut b = Vec::new();
                    b.extend_from_slice(&MAGIC);
                    put_u16(&mut b, FORMAT_VERSION);
                    let n = (azar() % 256) as usize;
                    b.extend((0..n).map(|_| azar() as u8));
                    b
                }
                // Un archivo válido con un byte cambiado en una zona que sí
                // cubre alguno de los checksums.
                2 => {
                    let mut b = original.clone();
                    let desde = HEADER_LEN;
                    let i = desde + (azar() as usize) % (b.len() - desde);
                    b[i] ^= (azar() as u8) | 1;
                    b
                }
                // Un archivo válido al que se le agregó cola.
                _ => {
                    let mut b = original.clone();
                    let n = 1 + (azar() % 32) as usize;
                    b.extend((0..n).map(|_| azar() as u8));
                    b
                }
            };
            match Snapshot::load(&buf) {
                Ok(_) => panic!("el caso {caso} de basura se aceptó como snapshot válido"),
                Err(e) => assert!(!e.to_string().is_empty()),
            }
        }
    }

    #[test]
    fn only_unchecksummed_header_bytes_can_change_silently() {
        // `built_at` y `generation` no entran en ningún checksum a propósito
        // (son metadatos de presentación); el resto de la cabecera fija sí se
        // valida por estructura y el archivo entero por `total_len`.
        let base = snap("/Volumes/A", "SN-A").to_bytes().unwrap();
        let mut b = base.clone();
        b[72..80].copy_from_slice(&1_600_000_000i64.to_le_bytes());
        let c = Snapshot::load(&b).expect("built_at no está en el checksum");
        assert_eq!(c.index.built_at, 1_600_000_000);
        assert_eq!(c.index.content_hash, snap("/Volumes/A", "SN-A").index.content_hash);
        // Un byte del relleno tampoco: no lo lee nadie.
        let mut b = base.clone();
        b[13] = 0xff;
        assert!(Snapshot::load(&b).is_ok());
        // Pero mover el `total_len` declarado ya no cierra.
        let mut b = base;
        b[88..96].copy_from_slice(&1u64.to_le_bytes());
        assert!(Snapshot::load(&b).is_err());
    }

    #[test]
    fn unknown_version_is_a_clear_error() {
        let mut bytes = snap("/Volumes/A", "SN-A").to_bytes().unwrap();
        bytes[9..11].copy_from_slice(&99u16.to_le_bytes());
        let e = Snapshot::load(&bytes).unwrap_err();
        assert!(matches!(e, SnapshotError::Version(99)), "{e}");
        assert!(e.to_string().contains("versión"));
    }

    #[test]
    fn garbage_header_is_magic_error() {
        let mut bytes = snap("/Volumes/A", "SN-A").to_bytes().unwrap();
        bytes[0] = b'X';
        assert_eq!(Snapshot::load(&bytes).unwrap_err(), SnapshotError::Magic);
        assert!(Snapshot::load(&[0u8; 200]).is_err());
        assert!(Snapshot::load(&[]).is_err());
    }

    /// Recalcula los dos checksums después de tocar el archivo a propósito,
    /// para probar que las validaciones estructurales no dependen de ellos.
    fn fix_checksums(bytes: &mut [u8]) {
        let hbl = u32::from_le_bytes(bytes[36..40].try_into().unwrap()) as usize;
        let bl = u64::from_le_bytes(bytes[40..48].try_into().unwrap()) as usize;
        let hc = fnv1a64(&[&bytes[HEADER_LEN..HEADER_LEN + hbl]]);
        let bc = fnv1a64(&[&bytes[HEADER_LEN + hbl..HEADER_LEN + hbl + bl]]);
        bytes[56..64].copy_from_slice(&bc.to_le_bytes());
        bytes[64..72].copy_from_slice(&hc.to_le_bytes());
    }

    fn body_start(bytes: &[u8]) -> usize {
        HEADER_LEN + u32::from_le_bytes(bytes[36..40].try_into().unwrap()) as usize
    }

    #[test]
    fn broken_checksums_are_rejected() {
        let bytes = snap("/Volumes/A", "SN-A").to_bytes().unwrap();
        let total = bytes.len();
        // Un byte del cuerpo: lo cubre `body_checksum`.
        let mut roto = bytes.clone();
        roto[total - 1] ^= 0x40;
        assert_eq!(Snapshot::load(&roto).unwrap_err(), SnapshotError::Checksum);
        // Un byte del bloque de cabecera: lo cubre `header_block_checksum`.
        let mut roto = bytes.clone();
        roto[HEADER_LEN + 2] ^= 0x01;
        assert_eq!(Snapshot::load(&roto).unwrap_err(), SnapshotError::Checksum);
    }

    #[test]
    fn remap_keeps_files_and_changes_only_the_prefix() {
        let mut s = snap("/Volumes/A", "SN-A");
        let antes = s.index.stats.files;
        let marca_antes = s.index.marca.clone();
        let cambiadas = s.remap_mount("/Volumes/B");
        assert_eq!(cambiadas, vec!["/Volumes/B".to_string()]);
        let raiz = s.index.roots[0];
        assert_eq!(s.index.path_of(raiz), "/Volumes/B");
        assert_eq!(s.mount(), "/Volumes/B");
        assert_eq!(s.volume.roots, vec!["/Volumes/B".to_string()]);
        // No se reindexó: mismos nodos, misma marca, mismos archivos.
        assert_eq!(s.index.stats.files, antes);
        assert_eq!(s.index.marca, marca_antes);
        assert!(s.index.find_dir("/Volumes/B/Documentos").is_some());
        assert!(s.index.find_dir("/Volumes/A").is_none());
        // El archivo se encuentra por su ruta nueva, con el prefijo remapeado.
        let doc = s.index.find_dir("/Volumes/B/Documentos").unwrap();
        let hijo = s
            .index
            .children(doc)
            .iter()
            .copied()
            .find(|&id| s.index.name_string_lossy(id) == "informe.pdf")
            .unwrap();
        assert_eq!(s.index.path_of(hijo), "/Volumes/B/Documentos/informe.pdf");
        // Volver al montaje viejo funciona igual (y no duplica la arena).
        let largo = s.index.names.len();
        assert_eq!(
            s.remap_mount("/Volumes/A"),
            vec!["/Volumes/A".to_string()]
        );
        assert_eq!(s.index.names.len(), largo);
        assert_eq!(s.index.path_of(s.index.roots[0]), "/Volumes/A");
        // Y sobrevive al roundtrip con la ruta remapeada en memoria.
        let c = Snapshot::load(&s.to_bytes().unwrap()).unwrap();
        assert_eq!(c.mount(), "/Volumes/A");
        assert_eq!(c.index.path_of(c.index.roots[0]), "/Volumes/A");
        assert_eq!(c.index.stats.files, antes);
    }

    #[test]
    fn two_disks_alternating_one_mount_do_not_overwrite() {
        let dir = tmp_dir("dos-discos");
        let a = indice("/Volumes/Backup", "SN-SEAGATE");
        let b = indice("/Volumes/Backup", "SN-WD");
        assert_ne!(a.volumes[0].id, b.volumes[0].id);
        let (ok_a, fallos_a) = save_per_volume(&a, &dir);
        let (ok_b, fallos_b) = save_per_volume(&b, &dir);
        assert!(fallos_a.is_empty(), "{fallos_a:?}");
        assert!(fallos_b.is_empty(), "{fallos_b:?}");
        assert_eq!(ok_a.len(), 1);
        assert_eq!(ok_b.len(), 1);
        assert_ne!(ok_a[0].path, ok_b[0].path, "se pisaron los .cnx");
        let ca = Snapshot::load_file(&ok_a[0].path).unwrap();
        let cb = Snapshot::load_file(&ok_b[0].path).unwrap();
        assert_ne!(ca.volume_id(), cb.volume_id());
        // Mismo punto de montaje, dos volúmenes distintos: no se mezclan.
        assert_eq!(ca.mount(), cb.mount());
        assert_eq!(ca.volume.serial.as_deref(), Some("SN-SEAGATE"));
        assert_eq!(cb.volume.serial.as_deref(), Some("SN-WD"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn remap_keeps_roots_outside_the_old_mount() {
        // Un volumen de nube puede tener varias raíces (`My Drive` y un drive
        // compartido). Al remapear el montaje sólo cambia la que cuelga de él.
        let v = volume("/Volumes/Cloud", "SN-CLOUD");
        let mut b = Builder::new(vec![v.clone()]);
        b.push_batch(&[
            entrada(&v, 1, 1, "/Volumes/Cloud/My Drive", true, 0),
            entrada(&v, 2, 2, "/Users/lucio/Library/CloudStorage/GoogleDrive-x", true, 0),
        ]);
        b.push_batch(&[entrada(&v, 10, 1, "informe.pdf", false, 7)]);
        let ix = b.finish(1, 0, Marca::mac(v.uuid, 0), Provenance::default());
        let mut s = Snapshot::from_index(ix).unwrap();
        assert_eq!(s.index.roots.len(), 2);
        let rutas = s.remap_mount("/Volumes/Cloud 1");
        assert_eq!(rutas.len(), 2, "no se pierde ninguna raíz");
        assert_eq!(rutas[0], "/Volumes/Cloud 1/My Drive");
        assert_eq!(
            rutas[1],
            "/Users/lucio/Library/CloudStorage/GoogleDrive-x"
        );
        assert_eq!(s.volume.roots, rutas);
        assert_eq!(s.index.path_of(s.index.roots[0]), "/Volumes/Cloud 1/My Drive");
        assert_eq!(s.index.stats.files, 1);
    }

    #[test]
    fn each_volume_gets_its_own_file() {
        let dir = tmp_dir("multi");
        let v1 = volume("/Volumes/Uno", "SN-1");
        let v2 = volume("/Volumes/Dos", "SN-2");
        let mut b = Builder::new(vec![v1.clone(), v2.clone()]);
        b.push_batch(&[
            entrada(&v1, 1, 1, "/Volumes/Uno", true, 0),
            entrada(&v2, 1, 1, "/Volumes/Dos", true, 0),
        ]);
        b.push_batch(&[
            entrada(&v1, 10, 1, "uno.txt", false, 1),
            entrada(&v2, 10, 1, "dos.txt", false, 2),
        ]);
        let ix = b.finish(1, 0, Marca("m1:00:1".into()), Provenance::default());
        assert!(
            Snapshot::from_index(ix.clone()).is_err(),
            "un .cnx es de un volumen"
        );
        let (ok, fallos) = save_per_volume(&ix, &dir);
        assert!(fallos.is_empty(), "{fallos:?}");
        assert_eq!(ok.len(), 2);
        let ids: Vec<String> = ok.iter().map(|f| f.volume_id.clone()).collect();
        assert!(ids.contains(&v1.id.to_string()));
        assert!(ids.contains(&v2.id.to_string()));
        for f in &ok {
            let s = Snapshot::load_file(&f.path).unwrap();
            assert_eq!(s.index.stats.files, 1);
            assert_eq!(s.index.volumes.len(), 1);
            assert_eq!(s.volume_id(), f.volume_id);
            // Cada archivo sólo tiene las rutas de su volumen.
            assert_eq!(s.index.roots.len(), 1);
            assert_eq!(s.index.nodes.len(), 2);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn structural_limits_hold_even_with_valid_checksums() {
        let base = snap("/Volumes/A", "SN-A").to_bytes().unwrap();
        let start = body_start(&base);

        // `parent` fuera de rango: no se puede leer el árbol.
        let mut b = base.clone();
        b[start + 8..start + 12].copy_from_slice(&9_999u32.to_le_bytes());
        fix_checksums(&mut b);
        let e = Snapshot::load(&b).unwrap_err();
        assert!(
            matches!(&e, SnapshotError::Invalid(m) if m.contains("parent")),
            "{e}"
        );

        // `name_off` fuera de la arena: no se puede leer un nombre.
        let mut b = base.clone();
        b[start..start + 4].copy_from_slice(&0xffff_0000u32.to_le_bytes());
        fix_checksums(&mut b);
        let e = Snapshot::load(&b).unwrap_err();
        assert!(
            matches!(&e, SnapshotError::Invalid(m) if m.contains("arena")),
            "{e}"
        );

        // `vol` fuera de rango: el nodo apunta a un volumen que no está.
        let mut b = base.clone();
        b[start + 56..start + 58].copy_from_slice(&7u16.to_le_bytes());
        fix_checksums(&mut b);
        let e = Snapshot::load(&b).unwrap_err();
        assert!(
            matches!(&e, SnapshotError::Invalid(m) if m.contains("volumen")),
            "{e}"
        );

        // `node_count` absurdo: se rechaza antes de reservar memoria.
        let mut b = base.clone();
        b[16..20].copy_from_slice(&(MAX_NODES + 1).to_le_bytes());
        fix_checksums(&mut b);
        let e = Snapshot::load(&b).unwrap_err();
        assert!(
            matches!(&e, SnapshotError::Invalid(m) if m.contains("node_count")),
            "{e}"
        );

        // Una raíz que no es su propio padre deja el índice inconsistente.
        let mut b = base.clone();
        b[start + 8..start + 12].copy_from_slice(&1u32.to_le_bytes());
        fix_checksums(&mut b);
        let e = Snapshot::load(&b).unwrap_err();
        assert!(
            matches!(&e, SnapshotError::Invalid(m) if m.contains("padre")),
            "{e}"
        );
    }

    #[test]
    fn remap_prefix_helper() {
        assert_eq!(
            remap_prefix("/Volumes/A", "/Volumes/A", "/Volumes/B"),
            "/Volumes/B"
        );
        assert_eq!(
            remap_prefix("/Volumes/A/x/y", "/Volumes/A", "/Volumes/B"),
            "/Volumes/B/x/y"
        );
        // Un prefijo que sólo coincide en texto no se toca.
        assert_eq!(
            remap_prefix("/Volumes/AB/x", "/Volumes/A", "/Volumes/B"),
            "/Volumes/AB/x"
        );
        assert_eq!(
            remap_prefix("/Users/x", "/Volumes/A", "/Volumes/B"),
            "/Users/x"
        );
        assert_eq!(remap_prefix("/Users/x", "/", "/base"), "/base/Users/x");
    }
}
