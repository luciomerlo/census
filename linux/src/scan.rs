//! Fuente Linux: `getdents64` + `statx` con ensamblado en preorden.
//!
//! El recorrido es secuencial por directorio para que el subarbol de cada
//! carpeta quede contiguo en el indice; el costo real esta en `statx`, no en
//! `readdir`. La version portable (`std::fs`) se usa en Windows para tests.
//!
//! No sigue symlinks, no cruza sistemas de archivos si `one_fs` y no borra
//! nada. `Fuente` emite lotes; `index_roots` empuja directo al `Builder` para
//! no duplicar memoria.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use crate::index::{Builder, Index, F_COMPLETE, F_DIR, F_ROOT, F_SYMLINK};
use crate::plataforma::{Encoding, Entrada, FileId, Fuente, Kind};
use crate::volumes::Mount;

const MAX_DEPTH: u32 = 512;

#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub full: bool,
    pub jobs: usize,
    pub excludes: Vec<PathBuf>,
    pub keep_other_roots: bool,
    pub host: crate::index::HostInfo,
    pub pc_id: String,
    pub one_fs: bool,
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            full: false,
            jobs: 0,
            excludes: Vec::new(),
            keep_other_roots: false,
            host: crate::index::HostInfo::default(),
            pc_id: String::new(),
            one_fs: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct RootReport {
    pub path: String,
    pub volume_id: String,
    pub nodes: u64,
    pub files: u64,
    pub dirs: u64,
    pub complete: bool,
}

#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub roots: Vec<RootReport>,
    pub nodes: u64,
    pub files: u64,
    pub dirs: u64,
    pub skipped: u64,
    pub errors: u64,
    pub walk_ms: u128,
    pub assemble_ms: u128,
    pub derived_ms: u128,
}

/// Fuente del contrato: recorre una raiz y entrega lotes de `Entrada`.
pub struct WalkSource {
    pub volume_id: String,
    pub batch: usize,
    pub one_fs: bool,
}

impl WalkSource {
    pub fn new(volume_id: impl Into<String>) -> Self {
        Self {
            volume_id: volume_id.into(),
            batch: 4096,
            one_fs: true,
        }
    }
}

impl Fuente for WalkSource {
    fn nombre(&self) -> &'static str {
        "walk"
    }

    fn escanear(
        &self,
        raiz: &Path,
        emit: &mut dyn FnMut(&[Entrada]) -> Result<(), String>,
    ) -> Result<(), String> {
        let root_meta = sys::lstat(raiz).map_err(|e| format!("{}: {e}", raiz.display()))?;
        let root_id = FileId::linux(&self.volume_id, root_meta.ino, root_meta.btime_ns);
        let mut out: Vec<Entrada> = Vec::with_capacity(self.batch.max(1));
        walk_emit(
            raiz,
            &root_id,
            root_meta.dev,
            self.one_fs,
            0,
            self.batch.max(1),
            &mut out,
            emit,
        )?;
        if !out.is_empty() {
            emit(&out)?;
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
fn walk_emit(
    dir: &Path,
    parent: &FileId,
    root_dev: u64,
    one_fs: bool,
    depth: u32,
    batch: usize,
    out: &mut Vec<Entrada>,
    emit: &mut dyn FnMut(&[Entrada]) -> Result<(), String>,
) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Ok(());
    }
    let entries = match sys::read_dir(dir) {
        Ok(v) => v,
        Err(_) => return Ok(()),
    };
    for e in entries {
        if e.name == b"." || e.name == b".." {
            continue;
        }
        let full = join(dir, &e.name);
        let meta = match sys::lstat(&full) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if one_fs && meta.kind == Kind::Dir && meta.dev != root_dev {
            continue;
        }
        let id = FileId::linux(&parent.key.volume_id, meta.ino, meta.btime_ns);
        out.push(Entrada {
            id: id.clone(),
            parent_id: parent.clone(),
            dev_id: meta.dev,
            kind: meta.kind,
            size: meta.size,
            alloc: meta.alloc,
            mtime_ns: meta.mtime_ns,
            btime_ns: meta.btime_ns,
            nlink: meta.nlink,
            flags: flags_for(meta.kind) as u32,
            encoding: Encoding::detect(&e.name),
            name: e.name,
        });
        if out.len() >= batch {
            emit(out)?;
            out.clear();
        }
        if meta.kind == Kind::Dir {
            walk_emit(&full, &id, root_dev, one_fs, depth + 1, batch, out, emit)?;
        }
    }
    Ok(())
}

fn flags_for(kind: Kind) -> u16 {
    match kind {
        Kind::Dir => F_DIR,
        Kind::Symlink => F_SYMLINK,
        _ => 0,
    }
}

fn join(dir: &Path, name: &[u8]) -> PathBuf {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        dir.join(std::ffi::OsStr::from_bytes(name))
    }
    #[cfg(not(unix))]
    {
        dir.join(String::from_utf8_lossy(name).into_owned())
    }
}

fn excluded(path: &Path, excludes: &[PathBuf]) -> bool {
    excludes.iter().any(|e| path.starts_with(e))
}

struct WalkStat {
    nodes: u64,
    files: u64,
    dirs: u64,
    skipped: u64,
    errors: u64,
    max_mtime: i64,
}

#[allow(clippy::too_many_arguments)]
fn add_dir(
    dir: &Path,
    parent: u32,
    b: &mut Builder,
    root_dev: u64,
    opts: &ScanOptions,
    depth: u32,
    st: &mut WalkStat,
) {
    if depth > MAX_DEPTH {
        b.mark_incomplete(parent);
        st.skipped += 1;
        return;
    }
    let entries = match sys::read_dir(dir) {
        Ok(v) => v,
        Err(_) => {
            b.mark_incomplete(parent);
            st.errors += 1;
            return;
        }
    };
    for e in entries {
        if e.name == b"." || e.name == b".." {
            continue;
        }
        let full = join(dir, &e.name);
        if excluded(&full, &opts.excludes) {
            st.skipped += 1;
            continue;
        }
        let meta = match sys::lstat(&full) {
            Ok(m) => m,
            Err(_) => {
                b.mark_incomplete(parent);
                st.errors += 1;
                continue;
            }
        };
        if opts.one_fs && meta.kind == Kind::Dir && meta.dev != root_dev {
            st.skipped += 1;
            continue;
        }
        let flags = flags_for(meta.kind)
            | if meta.kind == Kind::Dir {
                F_COMPLETE
            } else {
                0
            };
        let id = b.push(
            &e.name,
            flags,
            parent,
            meta.size,
            meta.alloc,
            meta.mtime_ns,
            meta.btime_ns,
            meta.dev,
            meta.ino,
            meta.nlink.min(u16::MAX as u32) as u16,
            meta.mode,
        );
        st.nodes += 1;
        if meta.kind == Kind::Dir {
            st.dirs += 1;
        } else {
            st.files += 1;
        }
        if meta.mtime_ns > st.max_mtime {
            st.max_mtime = meta.mtime_ns;
        }
        if meta.kind == Kind::Dir {
            add_dir(&full, id, b, root_dev, opts, depth + 1, st);
            if b.nodes[id as usize].flags & F_COMPLETE == 0 {
                b.mark_incomplete(parent);
            }
        }
    }
}

pub fn index_roots(
    roots: Vec<Mount>,
    old: Option<Arc<Index>>,
    opts: &ScanOptions,
) -> Result<(Index, ScanReport), String> {
    let started = Instant::now();
    let mut b = Builder::with_capacity(1 << 20);
    b.host = opts.host.clone();
    b.pc_id = opts.pc_id.clone();
    b.generation = old.as_ref().map(|o| o.generation + 1).unwrap_or(1);
    b.cambio_n = 0;
    b.scan_started = crate::time::now_ns();

    let mut report = ScanReport::default();
    for m in roots {
        let path = PathBuf::from(&m.path);
        let meta = match sys::lstat(&path) {
            Ok(meta) => meta,
            Err(e) => {
                report.errors += 1;
                report.roots.push(RootReport {
                    path: m.path.clone(),
                    volume_id: m.volume_id.clone(),
                    complete: false,
                    ..Default::default()
                });
                let _ = e;
                continue;
            }
        };
        let root_id = b.push(
            path.as_os_str().as_encoded_bytes(),
            F_DIR | F_ROOT | F_COMPLETE,
            u32::MAX,
            0,
            0,
            meta.mtime_ns,
            meta.btime_ns,
            meta.dev,
            meta.ino,
            1,
            meta.mode,
        );
        b.nodes[root_id as usize].parent = root_id;
        let start_nodes = b.len() as u64;
        let mut st = WalkStat {
            nodes: 0,
            files: 0,
            dirs: 0,
            skipped: 0,
            errors: 0,
            max_mtime: meta.mtime_ns,
        };
        add_dir(&path, root_id, &mut b, meta.dev, opts, 0, &mut st);
        let complete = b.nodes[root_id as usize].flags & F_COMPLETE != 0;
        let nodes = b.len() as u64 - start_nodes + 1;
        b.roots.push(crate::index::Root {
            id: root_id,
            volume_id: m.volume_id.clone(),
            fs_uuid: m.fs_uuid.clone(),
            cloud_id: m.cloud_id.clone(),
            path: m.path.clone(),
            fs: m.fs.clone(),
            label: m.label.clone(),
            source: m.source.clone(),
            kind: m.kind.clone(),
            total: m.total,
            free: m.free,
            scan_started: b.scan_started,
            scanned_at: crate::time::now_ns(),
            complete,
            nodes,
            max_mtime: st.max_mtime,
            method: "walk".into(),
            subvol: m.subvol.clone(),
            disk_hw_id: m.disk_hw_id.clone(),
            serial: m.serial.clone(),
            model: m.model.clone(),
            capacity: m.capacity,
            partition_offset: m.partition_offset,
            identity_status: m.identity_status.clone(),
            federable: m.federable,
        });
        report.roots.push(RootReport {
            path: m.path,
            volume_id: m.volume_id,
            nodes,
            files: st.files,
            dirs: st.dirs + 1,
            complete,
        });
        report.nodes += nodes;
        report.files += st.files;
        report.dirs += st.dirs + 1;
        report.skipped += st.skipped;
        report.errors += st.errors;
    }

    report.walk_ms = started.elapsed().as_millis();
    let t = Instant::now();
    let mut ix = b.finish();
    report.assemble_ms = t.elapsed().as_millis();
    report.derived_ms = report.assemble_ms;
    ix.generation = ix.generation.max(1);
    Ok((ix, report))
}

#[cfg(target_os = "linux")]
mod sys {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    #[derive(Clone, Copy, Debug)]
    pub struct Meta {
        pub kind: Kind,
        pub size: u64,
        pub alloc: u64,
        pub mtime_ns: i64,
        pub btime_ns: i64,
        pub dev: u64,
        pub ino: u64,
        pub nlink: u32,
        pub mode: u16,
    }

    #[derive(Clone, Debug)]
    pub struct Raw {
        pub name: Vec<u8>,
    }

    pub fn read_dir(path: &Path) -> std::io::Result<Vec<Raw>> {
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let fd = unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mut buf = vec![0u8; 64 * 1024];
        let mut out = Vec::new();
        loop {
            let n = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    fd,
                    buf.as_mut_ptr() as *mut libc::c_void,
                    buf.len(),
                )
            };
            if n < 0 {
                let err = std::io::Error::last_os_error();
                unsafe { libc::close(fd) };
                return Err(err);
            }
            if n == 0 {
                break;
            }
            let mut off = 0usize;
            while off + 19 <= n as usize {
                let d = &buf[off..n as usize];
                let reclen = u16::from_ne_bytes([d[16], d[17]]) as usize;
                if reclen < 19 || off + reclen > n as usize {
                    break;
                }
                let start = 19;
                let end = d[start..reclen]
                    .iter()
                    .position(|&b| b == 0)
                    .map(|p| start + p)
                    .unwrap_or(reclen);
                if end > start {
                    out.push(Raw {
                        name: d[start..end].to_vec(),
                    });
                }
                off += reclen;
            }
        }
        unsafe { libc::close(fd) };
        Ok(out)
    }

    pub fn lstat(path: &Path) -> std::io::Result<Meta> {
        let c = CString::new(path.as_os_str().as_bytes())
            .map_err(|_| std::io::Error::from(std::io::ErrorKind::InvalidInput))?;
        let mut sx: libc::statx = unsafe { std::mem::zeroed() };
        let rc = unsafe {
            libc::syscall(
                libc::SYS_statx,
                libc::AT_FDCWD,
                c.as_ptr(),
                libc::AT_SYMLINK_NOFOLLOW | libc::AT_STATX_DONT_SYNC,
                libc::STATX_BASIC_STATS | libc::STATX_BTIME,
                &mut sx,
            )
        };
        if rc != 0 {
            return Err(std::io::Error::last_os_error());
        }
        let mode = sx.stx_mode;
        let kind = match (mode as u32) & libc::S_IFMT {
            libc::S_IFDIR => Kind::Dir,
            libc::S_IFLNK => Kind::Symlink,
            libc::S_IFREG => Kind::File,
            _ => Kind::Other,
        };
        let mtime_ns = ts_ns(sx.stx_mtime.tv_sec, sx.stx_mtime.tv_nsec);
        let btime_ns = if sx.stx_mask & libc::STATX_BTIME != 0 {
            ts_ns(sx.stx_btime.tv_sec, sx.stx_btime.tv_nsec)
        } else {
            0
        };
        Ok(Meta {
            kind,
            size: sx.stx_size,
            alloc: sx.stx_blocks.saturating_mul(512),
            mtime_ns,
            btime_ns,
            dev: ((sx.stx_dev_major as u64) << 32) | sx.stx_dev_minor as u64,
            ino: sx.stx_ino,
            nlink: sx.stx_nlink,
            mode: mode as u16,
        })
    }

    fn ts_ns(sec: i64, nsec: u32) -> i64 {
        sec.saturating_mul(1_000_000_000)
            .saturating_add(nsec as i64)
    }
}

#[cfg(not(target_os = "linux"))]
mod sys {
    use super::*;
    use std::fs;

    #[derive(Clone, Copy, Debug)]
    pub struct Meta {
        pub kind: Kind,
        pub size: u64,
        pub alloc: u64,
        pub mtime_ns: i64,
        pub btime_ns: i64,
        pub dev: u64,
        pub ino: u64,
        pub nlink: u32,
        pub mode: u16,
    }

    #[derive(Clone, Debug)]
    pub struct Raw {
        pub name: Vec<u8>,
    }

    pub fn read_dir(path: &Path) -> std::io::Result<Vec<Raw>> {
        let mut out = Vec::new();
        for e in fs::read_dir(path)? {
            let e = e?;
            out.push(Raw {
                name: e.file_name().to_string_lossy().as_bytes().to_vec(),
            });
        }
        Ok(out)
    }

    pub fn lstat(path: &Path) -> std::io::Result<Meta> {
        let m = fs::symlink_metadata(path)?;
        let ft = m.file_type();
        let kind = if ft.is_dir() {
            Kind::Dir
        } else if ft.is_symlink() {
            Kind::Symlink
        } else if ft.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        let mtime_ns = m
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        #[cfg(unix)]
        let (dev, ino, nlink, mode, alloc) = {
            use std::os::unix::fs::MetadataExt;
            (
                m.dev(),
                m.ino(),
                m.nlink(),
                m.mode() as u16,
                m.blocks() * 512,
            )
        };
        #[cfg(not(unix))]
        let (dev, ino, nlink, mode, alloc) = (0, 0, 1, 0, m.len());
        Ok(Meta {
            kind,
            size: m.len(),
            alloc,
            mtime_ns,
            btime_ns: 0,
            dev,
            ino,
            nlink: nlink.min(u32::MAX as u64) as u32,
            mode,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn walks_a_temporary_tree_without_following_symlinks() {
        let base = std::env::temp_dir().join(format!("census-scan-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("a/b")).unwrap();
        fs::write(base.join("a/f.txt"), b"hola").unwrap();
        fs::write(base.join("a/b/g.bin"), b"12345678").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("f.txt", base.join("a/link")).unwrap();

        let src = WalkSource::new("test-volume");
        let mut got = 0usize;
        src.escanear(&base, &mut |batch| {
            got += batch.len();
            Ok(())
        })
        .unwrap();
        #[cfg(unix)]
        assert_eq!(got, 5); // a, f.txt, b, g.bin, link
        #[cfg(not(unix))]
        assert_eq!(got, 4);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn index_roots_assigns_preorder_and_subtrees() {
        let base = std::env::temp_dir().join(format!("census-ix-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(base.join("d")).unwrap();
        fs::write(base.join("x"), b"1").unwrap();
        fs::write(base.join("d/y"), b"22").unwrap();
        let mut m = Mount {
            path: base.to_string_lossy().into_owned(),
            volume_id: "vol-test".into(),
            ..Default::default()
        };
        m.path = crate::index::normalize_path(&m.path);
        let (ix, report) = index_roots(vec![m], None, &ScanOptions::default()).unwrap();
        assert_eq!(report.nodes, 4);
        let root = ix.roots[0].id;
        assert_eq!(ix.name_bytes(root), ix.roots[0].path.as_bytes());
        assert!(ix.find_dir(&ix.roots[0].path).is_some());
        assert_eq!(ix.tout[root as usize] as usize, ix.len());
        let d = ix.child(root, b"d").unwrap();
        assert_eq!(ix.tout[d as usize] - d, 2);
        let _ = fs::remove_dir_all(&base);
    }
}
