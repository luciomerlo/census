//! Fuente portable por lotes y recorrido de directorios de respaldo.
//!
//! En macOS la vía rápida es `getattrlistbulk` (`fuente_mac.rs`). Este
//! recorrido queda para volúmenes de red/nube, para sistemas que no exponen la
//! llamada y para los tests que corren en Windows.

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::types::{dir_volume_id, Encoding, Entrada, FileId, Kind, VolumeId};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Contrato de fuente de nodos: emite lotes, nunca un `Vec` de todo.
pub trait Fuente {
    fn nombre(&self) -> &'static str;
    fn escanear(&self, raiz: &Path, emit: &mut dyn FnMut(&[Entrada]) -> Result<()>) -> Result<()>;
}

/// Recorrido paralelo simple, portable. No sigue symlinks y no cruza puntos
/// de montaje. En macOS usa `MetadataExt` para inodo, `nlink` y bloques.
pub struct WalkFuente {
    pub volume_id: VolumeId,
    pub host: String,
    pub skip_mounts: bool,
    pub excludes: Vec<PathBuf>,
}

impl WalkFuente {
    pub fn new(volume_id: VolumeId, host: String) -> Self {
        WalkFuente {
            volume_id,
            host,
            skip_mounts: true,
            excludes: Vec::new(),
        }
    }
}

impl Fuente for WalkFuente {
    fn nombre(&self) -> &'static str {
        "walk"
    }

    fn escanear(&self, raiz: &Path, emit: &mut dyn FnMut(&[Entrada]) -> Result<()>) -> Result<()> {
        let raiz = raiz.to_path_buf();
        let meta = fs::symlink_metadata(&raiz)?;
        let root_id = file_id(&self.volume_id, &meta, &raiz, 0);
        let root_name = raiz.to_string_lossy().into_owned().into_bytes();
        let root = entrada(&raiz, &meta, root_id.clone(), root_id.clone(), root_name);
        emit(&[root])?;
        let mut cola: VecDeque<(PathBuf, FileId)> = VecDeque::new();
        cola.push_back((raiz, root_id));
        let mut lote: Vec<Entrada> = Vec::with_capacity(512);
        while let Some((dir, dir_id)) = cola.pop_front() {
            if self.excludes.contains(&dir) {
                continue;
            }
            let rd = match fs::read_dir(&dir) {
                Ok(rd) => rd,
                Err(_) => continue,
            };
            for ent in rd.flatten() {
                let path = ent.path();
                let meta = match fs::symlink_metadata(&path) {
                    Ok(m) => m,
                    Err(_) => continue,
                };
                let id = file_id(&self.volume_id, &meta, &path, 0);
                let name = ent.file_name().as_encoded_bytes().to_vec();
                let e = entrada(&path, &meta, id.clone(), dir_id.clone(), name);
                let is_dir = e.kind == Kind::Dir;
                lote.push(e);
                if is_dir {
                    if self.skip_mounts && is_mount(&path) {
                        continue;
                    }
                    cola.push_back((path, id));
                }
                if lote.len() >= 512 {
                    emit(&lote)?;
                    lote.clear();
                }
            }
        }
        if !lote.is_empty() {
            emit(&lote)?;
        }
        Ok(())
    }
}

fn is_mount(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if let (Ok(p), Some(Ok(parent))) = (fs::metadata(path), path.parent().map(fs::metadata)) {
            return p.dev() != parent.dev();
        }
    }
    let _ = path;
    false
}

fn file_id(volume: &VolumeId, meta: &fs::Metadata, path: &Path, gen: u32) -> FileId {
    let _ = path;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        FileId::new(volume.clone(), meta.ino(), gen, btime_ns(meta))
    }
    #[cfg(not(unix))]
    {
        let _ = gen;
        let mut h = crate::types::fnv1a64(&[path.to_string_lossy().as_bytes()]);
        h ^= meta.len();
        FileId::new(volume.clone(), h, 0, 0)
    }
}

#[cfg(unix)]
fn btime_ns(meta: &fs::Metadata) -> i64 {
    #[cfg(target_os = "macos")]
    {
        use std::os::macos::fs::MetadataExt;
        meta.st_birthtime()
            .saturating_mul(1_000_000_000)
            .saturating_add(meta.st_birthtime_nsec())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = meta;
        0
    }
}

fn entrada(path: &Path, meta: &fs::Metadata, id: FileId, parent: FileId, name: Vec<u8>) -> Entrada {
    let kind = if meta.is_dir() {
        Kind::Dir
    } else if meta.is_symlink() {
        Kind::Symlink
    } else if meta.is_file() {
        Kind::File
    } else {
        Kind::Other
    };
    let (ino, gen, nlink, alloc, mtime_ns, btime_ns) = {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            (
                meta.ino(),
                0,
                meta.nlink() as u32,
                meta.blocks().saturating_mul(512),
                meta.mtime().saturating_mul(1_000_000_000) + meta.mtime_nsec(),
                btime_ns(meta),
            )
        }
        #[cfg(not(unix))]
        {
            let mt = crate::time::to_ns(meta.modified().unwrap_or(std::time::UNIX_EPOCH));
            (id.key.ino, 0, 1, meta.len(), mt, 0)
        }
    };
    let _ = (ino, gen);
    #[allow(unused_mut)]
    #[cfg(unix)]
    let dev_id = {
        use std::os::unix::fs::MetadataExt;
        meta.dev()
    };
    #[cfg(not(unix))]
    let dev_id = id.key.ino;
    let encoding = match std::str::from_utf8(&name) {
        Ok(_) => Encoding::Utf8,
        Err(_) => Encoding::Other,
    };
    let _ = path;
    Entrada {
        id,
        parent_id: parent,
        dev_id,
        kind,
        size: if kind == Kind::Dir { 0 } else { meta.len() },
        alloc: if kind == Kind::Dir { 0 } else { alloc },
        mtime_ns,
        btime_ns,
        nlink,
        flags: 0,
        name,
        encoding,
    }
}

/// Volumen sintético para una raíz suelta (`census index --root`).
pub fn synthetic_volume(path: &Path, host: &str) -> crate::types::Volume {
    let abs = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mount = abs.to_string_lossy().into_owned();
    let id = dir_volume_id(host, &mount);
    let (total, free) = fs::metadata(&abs)
        .ok()
        .map(|_| (0u64, 0u64))
        .unwrap_or((0, 0));
    crate::types::Volume {
        id,
        disk_hw_id: None,
        disk_guid: None,
        serial: None,
        model: None,
        partition_offset: 0,
        identity_state: crate::types::IdentityState::SinIdentidad,
        cloud_id: None,
        possible_duplicate: false,
        uuid: [0; 16],
        name: abs
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| mount.clone()),
        mount: mount.clone(),
        fs: "dir".into(),
        total,
        free,
        local: true,
        network: false,
        cloud: false,
        removable: false,
        dev: 0,
        method: "walk".into(),
        roots: vec![mount],
        nodes: 0,
    }
}

pub fn io_err(msg: impl Into<String>) -> io::Error {
    io::Error::other(msg.into())
}
