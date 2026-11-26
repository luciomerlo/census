//! Snapshot global `.cns` (v1): nodos, nombres y raices; los indices
//! derivados se reconstruyen al cargar. Guardado atomico (tmp + rename).
//!
//! Un archivo por PC simplifica el v1: `query/top/stats` cargan una sola vez y
//! las raices conservan su `volume_id`. Little-endian, checksum FNV-1a.

use std::fs;
use std::io::{Cursor, Read, Write};
use std::path::Path;

use crate::index::{Index, Node, Root, Stats};
use crate::plataforma::fnv1a64;

const MAGIC: &[u8; 4] = b"CNS1";
const VERSION: u16 = 1;
const HEADER: usize = 4 + 2 + 8 + 8;
const MAX_NODES: u32 = 50_000_000;

pub fn save(ix: &Index, path: &Path) -> Result<u64, String> {
    if ix.nodes.len() > MAX_NODES as usize {
        return Err("indice demasiado grande para el snapshot v1".into());
    }
    let mut b = Vec::with_capacity(ix.names.len() + ix.nodes.len() * 64 + 1024);
    put_str(&mut b, &ix.pc_id);
    let host = serde_json::to_vec(&ix.host).unwrap_or_else(|_| b"{}".to_vec());
    put_bytes(&mut b, &host);
    put_i64(&mut b, ix.built_at);
    put_i64(&mut b, ix.scan_started);
    put_u64(&mut b, ix.generation);
    put_u64(&mut b, ix.cambio_n);
    put_u64(&mut b, ix.content_hash);
    put_u32(&mut b, ix.nodes.len() as u32);
    put_u32(&mut b, ix.names.len() as u32);
    put_u32(&mut b, ix.roots.len() as u32);
    b.extend_from_slice(&ix.names);
    for n in &ix.nodes {
        put_u64(&mut b, n.size);
        put_u64(&mut b, n.alloc);
        put_i64(&mut b, n.mtime_ns);
        put_i64(&mut b, n.btime_ns);
        put_u64(&mut b, n.ino);
        put_u64(&mut b, n.dev);
        put_u32(&mut b, n.name_off);
        put_u32(&mut b, n.parent);
        put_u16(&mut b, n.name_len);
        put_u16(&mut b, n.flags);
        put_u16(&mut b, n.nlink);
        put_u16(&mut b, n.mode);
    }
    for r in &ix.roots {
        put_u32(&mut b, r.id);
        for s in [
            &r.volume_id,
            &r.fs_uuid,
            &r.cloud_id,
            &r.path,
            &r.fs,
            &r.label,
            &r.source,
            &r.kind,
            &r.method,
            &r.subvol,
            &r.disk_hw_id,
            &r.serial,
            &r.model,
            &r.identity_status,
        ] {
            put_str(&mut b, s);
        }
        put_u64(&mut b, r.total);
        put_u64(&mut b, r.free);
        put_u64(&mut b, r.capacity);
        put_u64(&mut b, r.partition_offset);
        put_i64(&mut b, r.scan_started);
        put_i64(&mut b, r.scanned_at);
        put_i64(&mut b, r.max_mtime);
        put_u64(&mut b, r.nodes);
        put_u8(&mut b, r.complete as u8);
        put_u8(&mut b, r.federable as u8);
    }
    let checksum = fnv1a64(&b);
    let tmp = path.with_extension("cns.tmp");
    {
        let mut out =
            fs::File::create(&tmp).map_err(|e| format!("crear {}: {e}", tmp.display()))?;
        out.write_all(MAGIC).map_err(ioerr)?;
        out.write_all(&VERSION.to_le_bytes()).map_err(ioerr)?;
        out.write_all(&(b.len() as u64).to_le_bytes())
            .map_err(ioerr)?;
        out.write_all(&checksum.to_le_bytes()).map_err(ioerr)?;
        out.write_all(&b).map_err(ioerr)?;
        out.sync_all().map_err(ioerr)?;
    }
    fs::rename(&tmp, path)
        .or_else(|_| {
            // Windows no reemplaza en rename; en Linux el primer intento ya es atómico.
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path)
        })
        .map_err(|e| format!("rename {}: {e}", path.display()))?;
    Ok(HEADER as u64 + b.len() as u64)
}

pub fn load(path: &Path) -> Result<Index, String> {
    let data = fs::read(path).map_err(|e| format!("leer {}: {e}", path.display()))?;
    if data.len() < HEADER {
        return Err("snapshot truncado".into());
    }
    if &data[0..4] != MAGIC {
        return Err("no es un snapshot .cns".into());
    }
    let version = u16::from_le_bytes([data[4], data[5]]);
    if version != VERSION {
        return Err(format!("snapshot v{version}; este binario lee v{VERSION}"));
    }
    let body_len = u64::from_le_bytes(data[6..14].try_into().unwrap()) as usize;
    let checksum = u64::from_le_bytes(data[14..22].try_into().unwrap());
    if data.len() != HEADER + body_len {
        return Err("snapshot truncado o con bytes extra".into());
    }
    let body = &data[HEADER..];
    if fnv1a64(body) != checksum {
        return Err("checksum del snapshot no coincide".into());
    }

    let mut c = Cursor::new(body);
    let pc_id = get_str(&mut c)?;
    let host_raw = get_bytes(&mut c)?;
    let host = serde_json::from_slice(&host_raw).unwrap_or_default();
    let built_at = get_i64(&mut c)?;
    let scan_started = get_i64(&mut c)?;
    let generation = get_u64(&mut c)?;
    let cambio_n = get_u64(&mut c)?;
    let content_hash = get_u64(&mut c)?;
    let node_count = get_u32(&mut c)?;
    let name_len = get_u32(&mut c)? as usize;
    let root_count = get_u32(&mut c)?;
    if node_count > MAX_NODES {
        return Err("snapshot con demasiados nodos".into());
    }
    let mut names = vec![0u8; name_len];
    c.read_exact(&mut names).map_err(ioerr)?;
    let mut nodes = Vec::with_capacity(node_count as usize);
    for _ in 0..node_count {
        nodes.push(Node {
            size: get_u64(&mut c)?,
            alloc: get_u64(&mut c)?,
            mtime_ns: get_i64(&mut c)?,
            btime_ns: get_i64(&mut c)?,
            ino: get_u64(&mut c)?,
            dev: get_u64(&mut c)?,
            name_off: get_u32(&mut c)?,
            parent: get_u32(&mut c)?,
            name_len: get_u16(&mut c)?,
            flags: get_u16(&mut c)?,
            nlink: get_u16(&mut c)?,
            mode: get_u16(&mut c)?,
        });
    }
    let mut roots = Vec::with_capacity(root_count as usize);
    for _ in 0..root_count {
        let id = get_u32(&mut c)?;
        let volume_id = get_str(&mut c)?;
        let fs_uuid = get_str(&mut c)?;
        let cloud_id = get_str(&mut c)?;
        let path = get_str(&mut c)?;
        let fs_name = get_str(&mut c)?;
        let label = get_str(&mut c)?;
        let source = get_str(&mut c)?;
        let kind = get_str(&mut c)?;
        let method = get_str(&mut c)?;
        let subvol = get_str(&mut c)?;
        let disk_hw_id = get_str(&mut c)?;
        let serial = get_str(&mut c)?;
        let model = get_str(&mut c)?;
        let identity_status = get_str(&mut c)?;
        let total = get_u64(&mut c)?;
        let free = get_u64(&mut c)?;
        let capacity = get_u64(&mut c)?;
        let partition_offset = get_u64(&mut c)?;
        let scan_started = get_i64(&mut c)?;
        let scanned_at = get_i64(&mut c)?;
        let max_mtime = get_i64(&mut c)?;
        let nodes = get_u64(&mut c)?;
        let complete = get_u8(&mut c)? != 0;
        let federable = get_u8(&mut c)? != 0;
        roots.push(Root {
            id,
            volume_id,
            fs_uuid,
            cloud_id,
            path,
            fs: fs_name,
            label,
            source,
            kind,
            total,
            free,
            scan_started,
            scanned_at,
            complete,
            nodes,
            max_mtime,
            method,
            subvol,
            disk_hw_id,
            serial,
            model,
            capacity,
            partition_offset,
            identity_status,
            federable,
        });
    }
    if c.position() as usize != body.len() {
        return Err("snapshot con datos sobrantes".into());
    }
    let mut ix = Index {
        host,
        pc_id,
        generation,
        cambio_n,
        content_hash,
        nodes,
        names,
        folded: Vec::new(),
        compacto: Vec::new(),
        dir_bytes: Vec::new(),
        dir_alloc: Vec::new(),
        tout: Vec::new(),
        rango: Vec::new(),
        hijos_inicio: Vec::new(),
        hijos: Vec::new(),
        roots,
        built_at,
        scan_started,
        stats: Stats::default(),
    };
    ix.rebuild();
    ix.content_hash = content_hash;
    Ok(ix)
}

fn ioerr(e: std::io::Error) -> String {
    format!("snapshot: {e}")
}
fn put_u8(b: &mut Vec<u8>, v: u8) {
    b.push(v);
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
fn put_bytes(b: &mut Vec<u8>, v: &[u8]) {
    put_u32(b, v.len() as u32);
    b.extend_from_slice(v);
}
fn put_str(b: &mut Vec<u8>, v: &str) {
    put_bytes(b, v.as_bytes());
}
fn get_u8(c: &mut Cursor<&[u8]>) -> Result<u8, String> {
    let mut x = [0u8; 1];
    c.read_exact(&mut x).map_err(ioerr)?;
    Ok(x[0])
}
fn get_u16(c: &mut Cursor<&[u8]>) -> Result<u16, String> {
    let mut x = [0u8; 2];
    c.read_exact(&mut x).map_err(ioerr)?;
    Ok(u16::from_le_bytes(x))
}
fn get_u32(c: &mut Cursor<&[u8]>) -> Result<u32, String> {
    let mut x = [0u8; 4];
    c.read_exact(&mut x).map_err(ioerr)?;
    Ok(u32::from_le_bytes(x))
}
fn get_u64(c: &mut Cursor<&[u8]>) -> Result<u64, String> {
    let mut x = [0u8; 8];
    c.read_exact(&mut x).map_err(ioerr)?;
    Ok(u64::from_le_bytes(x))
}
fn get_i64(c: &mut Cursor<&[u8]>) -> Result<i64, String> {
    let mut x = [0u8; 8];
    c.read_exact(&mut x).map_err(ioerr)?;
    Ok(i64::from_le_bytes(x))
}
fn get_bytes(c: &mut Cursor<&[u8]>) -> Result<Vec<u8>, String> {
    let n = get_u32(c)? as usize;
    if n > 16 * 1024 * 1024 {
        return Err("campo de snapshot demasiado grande".into());
    }
    let mut v = vec![0u8; n];
    c.read_exact(&mut v).map_err(ioerr)?;
    Ok(v)
}
fn get_str(c: &mut Cursor<&[u8]>) -> Result<String, String> {
    let v = get_bytes(c)?;
    String::from_utf8(v).map_err(|_| "texto invalido en snapshot".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{Builder, F_DIR, F_ROOT};

    #[test]
    fn roundtrip_preserves_nodes_and_roots() {
        let mut b = Builder::with_capacity(4);
        let root = b.push(b"/", F_DIR | F_ROOT, 0, 0, 0, 1, 2, 3, 4, 1, 0o755);
        b.nodes[root as usize].parent = root;
        b.push(b"hola.txt", 0, root, 4, 512, 5, 6, 3, 7, 1, 0o644);
        b.roots.push(Root {
            id: root,
            volume_id: "vol".into(),
            path: "/".into(),
            fs: "ext4".into(),
            disk_hw_id: "hw".into(),
            identity_status: "ok".into(),
            federable: true,
            nodes: 2,
            ..Default::default()
        });
        let ix = b.finish();
        let path = std::env::temp_dir().join(format!("censusindex-snap-{}.cns", std::process::id()));
        save(&ix, &path).unwrap();
        let back = load(&path).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.name_bytes(1), b"hola.txt");
        assert_eq!(back.roots[0].volume_id, "vol");
        assert_eq!(back.roots[0].disk_hw_id, "hw");
        assert_eq!(back.content_hash, ix.content_hash);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn rejects_corrupt_checksum() {
        let mut b = Builder::with_capacity(1);
        let root = b.push(b"/", F_DIR | F_ROOT, 0, 0, 0, 0, 0, 0, 1, 1, 0o755);
        b.nodes[root as usize].parent = root;
        let ix = b.finish();
        let path = std::env::temp_dir().join(format!("censusindex-snap-bad-{}.cns", std::process::id()));
        save(&ix, &path).unwrap();
        let mut bytes = fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        fs::write(&path, &bytes).unwrap();
        assert!(load(&path).is_err());
        let _ = fs::remove_file(path);
    }
}
