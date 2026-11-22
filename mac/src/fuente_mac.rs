//! Fuente rápida de macOS: `getattrlistbulk`.
//!
//! Cada llamada devuelve los atributos de muchos hijos de una carpeta en un
//! solo syscall. Es la vía más parecida a leer la MFT en bloque: no hay un
//! `lstat` por archivo. Los directorios se reparten entre workers y un
//! colector entrega lotes al builder.
//!
//! `ATTR_CMN_FILEID` da el inodo y `ATTR_CMN_CRTIME` la creación; con eso se
//! arma `FileId = (FileKey, btime_ns)`. El padre del árbol es el `FileId` de
//! la carpeta que se está leyendo, que ya incluye su `btime`.
#![cfg(target_os = "macos")]

use std::collections::{HashMap, VecDeque};
use std::ffi::CString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use crate::source::{Fuente, Result};
use crate::types::{Encoding, Entrada, FileId, Kind, VolumeId, VolumeUuid, FLAG_MOUNT};

const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const BUF_INICIAL: usize = 256 * 1024;
const BUF_MAX: usize = 4 * 1024 * 1024;
const LOTE: usize = 1024;

#[derive(Clone, Debug)]
pub struct VolMap {
    pub by_dev: Arc<HashMap<u64, (VolumeId, VolumeUuid)>>,
    pub fallback: (VolumeId, VolumeUuid),
}

pub struct FuenteMac {
    pub volume_id: VolumeId,
    pub volume_uuid: VolumeUuid,
    pub dev_id: u64,
    pub jobs: usize,
    pub excludes: Vec<PathBuf>,
    pub skip_mounts: bool,
    pub volumes: VolMap,
}

impl FuenteMac {
    pub fn new(volume_id: VolumeId, volume_uuid: VolumeUuid, dev_id: u64) -> Self {
        let fallback = (volume_id.clone(), volume_uuid);
        FuenteMac {
            volume_id,
            volume_uuid,
            dev_id,
            jobs: 0,
            excludes: Vec::new(),
            skip_mounts: true,
            volumes: VolMap {
                by_dev: Arc::new(HashMap::new()),
                fallback,
            },
        }
    }

    fn jobs_real(&self) -> usize {
        if self.jobs > 0 {
            self.jobs.min(32)
        } else {
            std::thread::available_parallelism()
                .map(|n| n.get())
                .unwrap_or(4)
                .clamp(1, 8)
        }
    }
}

struct Estado {
    cola: VecDeque<(PathBuf, FileId)>,
    activos: usize,
    cerrado: bool,
}

impl Fuente for FuenteMac {
    fn nombre(&self) -> &'static str {
        "getattrlistbulk"
    }

    fn escanear(
        &self,
        raiz: &Path,
        emit: &mut dyn FnMut(&[Entrada]) -> Result<()>,
    ) -> Result<()> {
        let root_meta = std::fs::symlink_metadata(raiz)?;
        let root_id = meta_id(&self.volume_id, &root_meta);
        let root = Entrada {
            id: root_id.clone(),
            parent_id: root_id.clone(),
            dev_id: root_meta.dev(),
            kind: Kind::Dir,
            size: 0,
            alloc: 0,
            mtime_ns: meta_mtime(&root_meta),
            btime_ns: meta_btime(&root_meta),
            nlink: root_meta.nlink() as u32,
            flags: 0,
            name: raiz.as_os_str().as_bytes().to_vec(),
            encoding: Encoding::Utf8,
        };
        emit(&[root])?;

        let estado = Arc::new((
            Mutex::new(Estado {
                cola: VecDeque::from([(raiz.to_path_buf(), root_id)]),
                activos: 0,
                cerrado: false,
            }),
            Condvar::new(),
        ));
        let (tx, rx) = std::sync::mpsc::channel::<Vec<Entrada>>();
        let errores = Arc::new(AtomicU64::new(0));
        let nodos = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let jobs = self.jobs_real();
        let mut handles = Vec::with_capacity(jobs);
        for _ in 0..jobs {
            let estado = Arc::clone(&estado);
            let tx = tx.clone();
            let errores = Arc::clone(&errores);
            let nodos = Arc::clone(&nodos);
            let stop = Arc::clone(&stop);
            let excl = self.excludes.clone();
            let volumes = self.volumes.clone();
            let skip_mounts = self.skip_mounts;
            handles.push(std::thread::spawn(move || {
                worker(
                    estado,
                    tx,
                    errores,
                    nodos,
                    stop,
                    excl,
                    volumes,
                    skip_mounts,
                );
            }));
        }
        drop(tx);

        for lote in rx {
            emit(&lote)?;
        }
        for h in handles {
            let _ = h.join();
        }
        if errores.load(Ordering::Relaxed) > 0 {
            eprintln!(
                "  aviso: {} directorios no se pudieron leer; el índice sigue",
                errores.load(Ordering::Relaxed)
            );
        }
        eprintln!(
            "  getattrlistbulk: {} nodos en {} hilos",
            nodos.load(Ordering::Relaxed),
            jobs
        );
        Ok(())
    }
}

fn worker(
    estado: Arc<(Mutex<Estado>, Condvar)>,
    tx: std::sync::mpsc::Sender<Vec<Entrada>>,
    errores: Arc<AtomicU64>,
    nodos: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    excludes: Vec<PathBuf>,
    volumes: VolMap,
    skip_mounts: bool,
) {
    loop {
        let (dir, dir_id) = {
            let (lock, cv) = &*estado;
            let mut st = lock.lock().unwrap();
            loop {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                if let Some(job) = st.cola.pop_front() {
                    st.activos += 1;
                    break job;
                }
                if st.cerrado || st.activos == 0 {
                    return;
                }
                st = cv.wait(st).unwrap();
            }
        };
        let mut hijos = Vec::new();
        let mut total = 0u64;
        let res = leer_dir(
            &dir,
            &dir_id,
            &excludes,
            &volumes,
            skip_mounts,
            &tx,
            &mut hijos,
            &mut total,
        );
        if res.is_err() {
            errores.fetch_add(1, Ordering::Relaxed);
        }
        nodos.fetch_add(total, Ordering::Relaxed);
        let (lock, cv) = &*estado;
        let mut st = lock.lock().unwrap();
        for h in hijos {
            st.cola.push_back(h);
        }
        st.activos -= 1;
        if st.activos == 0 && st.cola.is_empty() {
            st.cerrado = true;
            cv.notify_all();
        } else {
            cv.notify_all();
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn leer_dir(
    dir: &Path,
    dir_id: &FileId,
    excludes: &[PathBuf],
    volumes: &VolMap,
    skip_mounts: bool,
    tx: &std::sync::mpsc::Sender<Vec<Entrada>>,
    hijos: &mut Vec<(PathBuf, FileId)>,
    total: &mut u64,
) -> std::io::Result<()> {
    let c = CString::new(dir.as_os_str().as_bytes())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "ruta con NUL"))?;
    let fd = unsafe {
        libc::open(
            c.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    let mut alist: libc::attrlist = unsafe { std::mem::zeroed() };
    alist.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
    alist.commonattr = libc::ATTR_CMN_RETURNED_ATTRS
        | libc::ATTR_CMN_NAME
        | libc::ATTR_CMN_DEVID
        | libc::ATTR_CMN_OBJTYPE
        | libc::ATTR_CMN_CRTIME
        | libc::ATTR_CMN_MODTIME
        | libc::ATTR_CMN_ACCTIME
        | libc::ATTR_CMN_ACCESSMASK
        | libc::ATTR_CMN_FLAGS
        | libc::ATTR_CMN_FILEID
        | ATTR_CMN_ERROR;
    alist.dirattr = libc::ATTR_DIR_MOUNTSTATUS;
    alist.fileattr =
        libc::ATTR_FILE_LINKCOUNT | libc::ATTR_FILE_TOTALSIZE | libc::ATTR_FILE_ALLOCSIZE;

    let mut buf = vec![0u8; BUF_INICIAL];
    let mut lote: Vec<Entrada> = Vec::with_capacity(LOTE);
    loop {
        let count = unsafe {
            libc::getattrlistbulk(
                fd,
                &mut alist as *mut _ as *mut libc::c_void,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            )
        };
        if count < 0 {
            let e = std::io::Error::last_os_error();
            if e.raw_os_error() == Some(libc::ERANGE) && buf.len() < BUF_MAX {
                buf.resize((buf.len() * 2).min(BUF_MAX), 0);
                continue;
            }
            unsafe { libc::close(fd) };
            return Err(e);
        }
        if count == 0 {
            break;
        }
        let mut off = 0usize;
        for _ in 0..count {
            if off + 4 > buf.len() {
                break;
            }
            let len = read_u32(&buf[off..]) as usize;
            if len < 24 || off + len > buf.len() {
                break;
            }
            let entry = &buf[off..off + len];
            off += len;
            let Some((mut e, es_montaje)) = parsear_entrada(entry, volumes) else {
                continue;
            };
            e.parent_id = dir_id.clone();
            let child_path = dir.join(std::ffi::OsStr::from_bytes(&e.name));
            if excludes.iter().any(|x| child_path == *x) {
                continue;
            }
            let es_dir = e.kind == Kind::Dir;
            if es_dir && es_montaje {
                e.flags |= FLAG_MOUNT;
            }
            let child_id = e.id.clone();
            lote.push(e);
            *total += 1;
            if es_dir && !(skip_mounts && es_montaje) {
                hijos.push((child_path, child_id));
            }
            if lote.len() >= LOTE {
                let _ = tx.send(std::mem::replace(&mut lote, Vec::with_capacity(LOTE)));
            }
        }
    }
    if !lote.is_empty() {
        let _ = tx.send(lote);
    }
    unsafe { libc::close(fd) };
    Ok(())
}

/// Parsea una entrada del buffer de `getattrlistbulk`.
fn parsear_entrada(entry: &[u8], volumes: &VolMap) -> Option<(Entrada, bool)> {
    if entry.len() < 24 {
        return None;
    }
    let mut p = 4usize;
    let returned = AttributeSet {
        commonattr: read_u32(&entry[p..]),
        volattr: read_u32(&entry[p + 4..]),
        dirattr: read_u32(&entry[p + 8..]),
        fileattr: read_u32(&entry[p + 12..]),
        forkattr: read_u32(&entry[p + 16..]),
    };
    p += 20;
    if returned.commonattr & ATTR_CMN_ERROR != 0 {
        let error = read_u32(&entry[p..]);
        p += 4;
        if error != 0 {
            return None;
        }
    }
    if returned.commonattr & libc::ATTR_CMN_NAME == 0 {
        return None;
    }
    let (name, name_len) = attrref(entry, p)?;
    p += 8;
    if name_len == 0 {
        return None;
    }
    let name = if name.last() == Some(&0) {
        &name[..name_len - 1]
    } else {
        name
    };
    let mut devid = 0u32;
    if returned.commonattr & libc::ATTR_CMN_DEVID != 0 {
        devid = read_u32(&entry[p..]);
        p += 4;
    }
    let mut objtype = 0u32;
    if returned.commonattr & libc::ATTR_CMN_OBJTYPE != 0 {
        objtype = read_u32(&entry[p..]);
        p += 4;
    }
    let mut btime_ns = 0i64;
    if returned.commonattr & libc::ATTR_CMN_CRTIME != 0 {
        btime_ns = timespec_ns(entry, p);
        p += 16;
    }
    let mut mtime_ns = 0i64;
    if returned.commonattr & libc::ATTR_CMN_MODTIME != 0 {
        mtime_ns = timespec_ns(entry, p);
        p += 16;
    }
    if returned.commonattr & libc::ATTR_CMN_ACCTIME != 0 {
        p += 16;
    }
    if returned.commonattr & libc::ATTR_CMN_ACCESSMASK != 0 {
        p += 4;
    }
    if returned.commonattr & libc::ATTR_CMN_FLAGS != 0 {
        p += 4;
    }
    let mut ino = 0u64;
    if returned.commonattr & libc::ATTR_CMN_FILEID != 0 {
        ino = read_u64(&entry[p..]);
        p += 8;
    }
    let mut es_montaje = false;
    if returned.dirattr & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        let st = read_u32(&entry[p..]);
        es_montaje = st & libc::DIR_MNTSTATUS_MNTPOINT != 0;
        p += 4;
    }
    let mut nlink = 1u32;
    if returned.fileattr & libc::ATTR_FILE_LINKCOUNT != 0 {
        nlink = read_u32(&entry[p..]);
        p += 4;
    }
    let mut size = 0u64;
    if returned.fileattr & libc::ATTR_FILE_TOTALSIZE != 0 {
        size = read_i64(&entry[p..]).max(0) as u64;
        p += 8;
    }
    let mut alloc = size;
    if returned.fileattr & libc::ATTR_FILE_ALLOCSIZE != 0 {
        alloc = read_i64(&entry[p..]).max(0) as u64;
        let _ = p;
    }
    let kind = match objtype {
        2 => Kind::Dir,
        5 => Kind::Symlink,
        1 => Kind::File,
        _ => Kind::Other,
    };
    let (vol_id, _uuid) = volumes
        .by_dev
        .get(&(devid as u64))
        .cloned()
        .unwrap_or_else(|| volumes.fallback.clone());
    let id = FileId::new(vol_id, ino, 0, btime_ns);
    let encoding = if std::str::from_utf8(name).is_ok() {
        Encoding::Utf8
    } else {
        Encoding::Other
    };
    Some((
        Entrada {
            id,
            parent_id: FileId::default(),
            dev_id: devid as u64,
            kind,
            size: if kind == Kind::Dir { 0 } else { size },
            alloc: if kind == Kind::Dir { 0 } else { alloc },
            mtime_ns,
            btime_ns,
            nlink,
            flags: 0,
            name: name.to_vec(),
            encoding,
        },
        es_montaje,
    ))
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AttributeSet {
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

fn attrref(buf: &[u8], p: usize) -> Option<(&[u8], usize)> {
    if p + 8 > buf.len() {
        return None;
    }
    let off = i32::from_ne_bytes(buf[p..p + 4].try_into().ok()?) as isize;
    let len = u32::from_ne_bytes(buf[p + 4..p + 8].try_into().ok()?) as usize;
    let start = (p as isize).checked_add(off)? as usize;
    if start > buf.len() || start.checked_add(len)? > buf.len() {
        return None;
    }
    Some((&buf[start..start + len], len))
}

fn timespec_ns(buf: &[u8], p: usize) -> i64 {
    if p + 16 > buf.len() {
        return 0;
    }
    let sec = i64::from_ne_bytes(buf[p..p + 8].try_into().unwrap());
    let nsec = i64::from_ne_bytes(buf[p + 8..p + 16].try_into().unwrap());
    sec.saturating_mul(1_000_000_000).saturating_add(nsec)
}

fn read_u32(b: &[u8]) -> u32 {
    u32::from_ne_bytes(b[..4].try_into().unwrap())
}

fn read_u64(b: &[u8]) -> u64 {
    u64::from_ne_bytes(b[..8].try_into().unwrap())
}

fn read_i64(b: &[u8]) -> i64 {
    i64::from_ne_bytes(b[..8].try_into().unwrap())
}

fn meta_id(volume: &VolumeId, meta: &std::fs::Metadata) -> FileId {
    use std::os::unix::fs::MetadataExt;
    let btime = meta
        .st_birthtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.st_birthtime_nsec());
    FileId::new(volume.clone(), meta.ino(), 0, btime)
}

fn meta_mtime(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.mtime_nsec())
}

fn meta_btime(meta: &std::fs::Metadata) -> i64 {
    use std::os::unix::fs::MetadataExt;
    meta.st_birthtime()
        .saturating_mul(1_000_000_000)
        .saturating_add(meta.st_birthtime_nsec())
}

/// `getattrlistbulk` no sigue symlinks: devuelve el enlace, no el destino.
/// Esta función deja el contrato explícito para los tests.
pub fn no_sigue_symlinks(kind: Kind, target_is_dir: bool) -> bool {
    kind == Kind::Symlink && target_is_dir
}
