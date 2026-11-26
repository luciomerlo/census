//! Reescaneo incremental: construye un `Index` nuevo reutilizando el anterior.
//!
//! Dos modos, un solo algoritmo:
//!
//! * [`Mode::Sweep`] — barrido podado. Por cada carpeta se hace **un** `stat`;
//!   si su `mtime` coincide con el del indice anterior, sus entradas no se
//!   vuelven a leer (ni `getdents` ni `statx` por archivo) y se copian del
//!   indice viejo. Una carpeta cambia de `mtime` cuando se crea, borra o
//!   renombra una entrada suya; **no** cuando se reescribe un archivo existente
//!   (eso lo cubre `MediaStore` o el modo `Dirty`).
//! * [`Mode::Dirty`] — la app dice que carpetas cambiaron (por `MediaStore`,
//!   `inotify` o un `ContentObserver`). Solo esas se relistan; los subarboles
//!   sin carpetas sucias se copian enteros sin ningun `stat`.
//!
//! Un volumen que ya no esta (SD extraida) conserva su ultima copia: el
//! subarbol viejo se copia tal cual y queda disponible para buscar offline.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

use censusindex::index::{normalize_path, Builder, Index, Root, F_COMPLETE, F_DIR, F_ROOT, F_SYMLINK};
use censusindex::plataforma::Kind;
use censusindex::scan::sys;
use censusindex::volumes::Mount;

const MAX_DEPTH: u32 = 512;

#[derive(Clone, Debug)]
pub enum Mode {
    /// Indexa desde cero (sin reutilizar nada).
    Full,
    /// Barrido podado por `mtime` de carpeta.
    Sweep,
    /// Relista solo estas carpetas (rutas absolutas).
    Dirty(Vec<String>),
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub excludes: Vec<PathBuf>,
    pub pc_id: String,
    pub host: censusindex::index::HostInfo,
    /// Volumenes que se recorren completos aunque haya copia previa. Los
    /// extraibles FAT/exFAT no garantizan que el `mtime` de carpeta cambie si
    /// se modificaron en otro equipo: al reconectarlos no se confia en la poda.
    pub full_volumes: Vec<String>,
}

/// Cuanto trabajo hizo realmente el reescaneo. Es la medida de que tan
/// "incremental" fue: `dirs_listed` y `statx` bajos frente a `nodes`.
#[derive(Clone, Debug, Default)]
pub struct Report {
    pub dirs_listed: u64,
    pub dirs_statted: u64,
    pub subtrees_copied: u64,
    pub nodes_copied: u64,
    pub nodes_new: u64,
    pub volumes_kept_offline: u64,
    pub errors: u64,
    pub ms: u128,
    pub nodes: u64,
}

struct Ctx<'a> {
    old: Option<&'a Index>,
    b: Builder,
    mode: &'a Mode,
    dirty: HashSet<String>,
    /// Dirty + todos sus ancestros: hay que bajar hasta ellas.
    touch: HashSet<String>,
    excludes: &'a [PathBuf],
    root_dev: u64,
    max_mtime: i64,
    rep: Report,
}

impl Ctx<'_> {
    fn touches(&self, path: &str) -> bool {
        self.touch.contains(path)
    }

    /// Copia el nodo `i` del indice viejo bajo `parent`.
    fn copy_node(&mut self, i: u32, parent: u32) -> u32 {
        let old = self.old.expect("copy sin indice previo");
        let n = old.nodes[i as usize];
        self.rep.nodes_copied += 1;
        self.b.push(
            old.name_bytes(i),
            n.flags,
            parent,
            n.size,
            n.alloc,
            n.mtime_ns,
            n.btime_ns,
            n.dev,
            n.ino,
            n.nlink,
            n.mode,
        )
    }

    /// Copia el subarbol completo de `i` (preorden contiguo) bajo `parent`.
    fn copy_subtree(&mut self, i: u32, parent: u32) {
        let old = self.old.expect("copy sin indice previo");
        let range = old.subtree(i);
        let base = self.b.len() as u32;
        for j in range.clone() {
            let p = if j == i {
                parent
            } else {
                base + (old.nodes[j as usize].parent - i)
            };
            self.copy_node(j, p);
        }
        self.rep.subtrees_copied += 1;
    }

    fn skip(&self, path: &Path) -> bool {
        self.excludes.iter().any(|e| path.starts_with(e))
    }
}

/// Reconstruye el indice de `volumes`. `old` aporta lo reutilizable.
pub fn rescan(
    old: Option<&Index>,
    volumes: &[Mount],
    mode: &Mode,
    opts: &Options,
) -> Result<(Index, Report), String> {
    let started = Instant::now();
    let full = matches!(mode, Mode::Full);
    let old = if full { None } else { old };

    let (dirty, touch) = match mode {
        Mode::Dirty(paths) => {
            let mut d = HashSet::new();
            let mut t = HashSet::new();
            for p in paths {
                let p = normalize_path(p);
                let mut cur = p.as_str();
                t.insert(cur.to_string());
                while let Some(pos) = cur.rfind('/') {
                    cur = &cur[..pos];
                    if cur.is_empty() {
                        break;
                    }
                    t.insert(cur.to_string());
                }
                d.insert(p);
            }
            (d, t)
        }
        _ => (HashSet::new(), HashSet::new()),
    };

    let mut b = Builder::with_capacity(old.map_or(1 << 16, |o| o.len() + 1024));
    b.host = opts.host.clone();
    b.pc_id = opts.pc_id.clone();
    b.generation = old.map_or(1, |o| o.generation + 1);
    let mut ctx = Ctx {
        old,
        b,
        mode,
        dirty,
        touch,
        excludes: &opts.excludes,
        root_dev: 0,
        max_mtime: 0,
        rep: Report::default(),
    };

    let mut seen: HashSet<&str> = HashSet::new();
    for m in volumes {
        seen.insert(m.volume_id.as_str());
        let old_root = old.and_then(|o| o.roots.iter().find(|r| r.volume_id == m.volume_id));
        let path = PathBuf::from(&m.path);
        match sys::lstat(&path) {
            Ok(meta) => {
                let forced = opts.full_volumes.contains(&m.volume_id);
                let old_id = if forced { None } else { old_root.map(|r| r.id) };
                add_root(&mut ctx, m, &path, &meta, old_id, old_root);
            }
            Err(_) => match old_root {
                // Montaje ausente pero con copia previa: queda offline.
                Some(r) => keep_offline(&mut ctx, r, Some(m)),
                None => ctx.rep.errors += 1,
            },
        }
    }
    // Volumenes que ya no se enumeran: se conserva su ultima copia.
    if let Some(o) = old {
        for r in &o.roots {
            if !seen.contains(r.volume_id.as_str()) {
                keep_offline(&mut ctx, r, None);
            }
        }
    }

    let mut rep = ctx.rep;
    let mut ix = ctx.b.finish();
    ix.generation = ix.generation.max(1);
    rep.nodes = ix.len() as u64;
    rep.ms = started.elapsed().as_millis();
    Ok((ix, rep))
}

fn add_root(
    ctx: &mut Ctx,
    m: &Mount,
    path: &Path,
    meta: &sys::Meta,
    old_id: Option<u32>,
    old_root: Option<&Root>,
) {
    let scan_started = censusindex::time::now_ns();
    let id = ctx.b.push(
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
    ctx.b.nodes[id as usize].parent = id;
    ctx.root_dev = meta.dev;
    ctx.max_mtime = meta.mtime_ns;
    let first = ctx.b.len();

    let path_s = m.path.as_str();
    let relist = match (&ctx.mode, old_id) {
        (_, None) => true,
        (Mode::Sweep, Some(o)) => ctx
            .old
            .is_some_and(|x| x.nodes[o as usize].mtime_ns != meta.mtime_ns),
        (Mode::Dirty(_), Some(_)) => ctx.dirty.contains(path_s),
        (Mode::Full, _) => true,
    };
    if relist {
        list_dir(ctx, path, id, old_id, 0);
    } else if let Some(o) = old_id {
        if matches!(ctx.mode, Mode::Dirty(_)) && !ctx.touches(path_s) {
            // Nada sucio bajo esta raiz: se copia entera sin ningun stat.
            for &c in ctx.old.expect("old").children(o).to_vec().iter() {
                ctx.copy_subtree(c, id);
            }
        } else {
            reuse_dir(ctx, path, id, o, 0);
        }
    }

    let nodes = (ctx.b.len() - first) as u64 + 1;
    let complete = ctx.b.nodes[id as usize].flags & F_COMPLETE != 0;
    let mut root = old_root.cloned().unwrap_or_default();
    root.id = id;
    root.volume_id = m.volume_id.clone();
    root.fs_uuid = m.fs_uuid.clone();
    root.path = m.path.clone();
    root.fs = m.fs.clone();
    root.label = m.label.clone();
    root.source = m.source.clone();
    root.kind = m.kind.clone();
    root.total = m.total;
    root.free = m.free;
    root.scan_started = scan_started;
    root.scanned_at = censusindex::time::now_ns();
    root.complete = complete;
    root.nodes = nodes;
    root.max_mtime = ctx.max_mtime.max(old_root.map_or(0, |r| r.max_mtime));
    root.method = if old_id.is_some() { "delta" } else { "walk" }.into();
    root.identity_status = m.identity_status.clone();
    root.federable = m.federable;
    ctx.b.roots.push(root);
}

fn keep_offline(ctx: &mut Ctx, r: &Root, mount: Option<&Mount>) {
    let old = ctx.old.expect("old");
    let id = ctx.copy_node(r.id, u32::MAX);
    ctx.b.nodes[id as usize].parent = id;
    for &c in old.children(r.id) {
        ctx.copy_subtree(c, id);
    }
    let mut root = r.clone();
    root.id = id;
    if let Some(m) = mount {
        root.free = m.free;
    }
    ctx.rep.volumes_kept_offline += 1;
    ctx.b.roots.push(root);
}

/// Relista `dir` (getdents + statx) y decide, por hijo, si se reutiliza.
fn list_dir(ctx: &mut Ctx, dir: &Path, id: u32, old_id: Option<u32>, depth: u32) {
    if depth > MAX_DEPTH {
        ctx.b.mark_incomplete(id);
        return;
    }
    let entries = match sys::read_dir(dir) {
        Ok(v) => v,
        Err(_) => {
            ctx.b.mark_incomplete(id);
            ctx.rep.errors += 1;
            return;
        }
    };
    ctx.rep.dirs_listed += 1;
    // Mapa nombre -> id viejo, una sola vez por carpeta (evita busquedas O(n)).
    let old_kids: HashMap<&[u8], u32> = match (ctx.old, old_id) {
        (Some(o), Some(oid)) => o
            .children(oid)
            .iter()
            .map(|&c| (o.name_bytes(c), c))
            .collect(),
        _ => HashMap::new(),
    };
    // `old_kids` toma prestado `ctx.old`; se materializa para liberar `ctx`.
    let old_kids: HashMap<Vec<u8>, u32> =
        old_kids.into_iter().map(|(k, v)| (k.to_vec(), v)).collect();

    for e in entries {
        if e.name == b"." || e.name == b".." {
            continue;
        }
        let full = join(dir, &e.name);
        if ctx.skip(&full) {
            continue;
        }
        let meta = match sys::lstat(&full) {
            Ok(m) => m,
            Err(_) => {
                ctx.b.mark_incomplete(id);
                ctx.rep.errors += 1;
                continue;
            }
        };
        if meta.kind == Kind::Dir && meta.dev != ctx.root_dev {
            continue; // otro filesystem (p. ej. un bind mount)
        }
        let old_child = old_kids.get(&e.name).copied();
        if meta.kind == Kind::Dir {
            add_dir(ctx, &full, &e.name, &meta, old_child, id, depth + 1);
        } else {
            push_meta(ctx, &e.name, &meta, id);
            ctx.rep.nodes_new += 1;
        }
    }
}

/// Reutiliza las entradas de `dir` del indice viejo sin leerla del disco.
fn reuse_dir(ctx: &mut Ctx, dir: &Path, id: u32, old_id: u32, depth: u32) {
    if depth > MAX_DEPTH {
        ctx.b.mark_incomplete(id);
        return;
    }
    let old = ctx.old.expect("old");
    let kids: Vec<u32> = old.children(old_id).to_vec();
    for c in kids {
        if !old.is_dir(c) {
            ctx.copy_node(c, id);
            continue;
        }
        let name = old.name_bytes(c).to_vec();
        let full = join(dir, &name);
        let path_s = censusindex::index::path_string(&full);
        if matches!(ctx.mode, Mode::Dirty(_)) && !ctx.touches(&path_s) {
            ctx.copy_subtree(c, id);
            continue;
        }
        ctx.rep.dirs_statted += 1;
        match sys::lstat(&full) {
            Ok(meta) if meta.kind == Kind::Dir => {
                add_dir(ctx, &full, &name, &meta, Some(c), id, depth + 1)
            }
            // Desaparecio (o ahora es otra cosa): la carpeta padre tambien
            // cambio de mtime, asi que el proximo barrido la relista.
            _ => {}
        }
    }
}

fn add_dir(
    ctx: &mut Ctx,
    full: &Path,
    name: &[u8],
    meta: &sys::Meta,
    old_child: Option<u32>,
    parent: u32,
    depth: u32,
) {
    let path_s = censusindex::index::path_string(full);
    let relist = match (&ctx.mode, old_child) {
        (_, None) | (Mode::Full, _) => true,
        (Mode::Sweep, Some(o)) => ctx
            .old
            .is_some_and(|x| x.nodes[o as usize].mtime_ns != meta.mtime_ns),
        (Mode::Dirty(_), Some(_)) => ctx.dirty.contains(&path_s),
    };
    // Dirty sin nada sucio debajo: subarbol completo, cero stats.
    if let (Mode::Dirty(_), Some(o)) = (&ctx.mode, old_child) {
        if !relist && !ctx.touches(&path_s) {
            ctx.copy_subtree(o, parent);
            return;
        }
    }
    let id = push_meta(ctx, name, meta, parent);
    ctx.b.nodes[id as usize].flags |= F_COMPLETE;
    if relist {
        list_dir(ctx, full, id, old_child, depth);
    } else if let Some(o) = old_child {
        reuse_dir(ctx, full, id, o, depth);
    }
    if ctx.b.nodes[id as usize].flags & F_COMPLETE == 0 {
        ctx.b.mark_incomplete(parent);
    }
}

fn push_meta(ctx: &mut Ctx, name: &[u8], meta: &sys::Meta, parent: u32) -> u32 {
    let flags = match meta.kind {
        Kind::Dir => F_DIR,
        Kind::Symlink => F_SYMLINK,
        _ => 0,
    };
    if meta.mtime_ns > ctx.max_mtime {
        ctx.max_mtime = meta.mtime_ns;
    }
    ctx.b.push(
        name,
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
    )
}

fn join(dir: &Path, name: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    dir.join(std::ffi::OsStr::from_bytes(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::Duration;

    fn tmp(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("censusindex-android-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    fn mount_for(p: &Path, id: &str) -> Mount {
        Mount {
            path: normalize_path(&p.to_string_lossy()),
            volume_id: id.into(),
            ..Default::default()
        }
    }

    /// `mtime` de carpeta con granularidad fina: esperar un tick evita que la
    /// creacion de un archivo caiga en el mismo instante que el barrido previo.
    fn tick() {
        std::thread::sleep(Duration::from_millis(20));
    }

    fn names(ix: &Index) -> Vec<String> {
        let mut v: Vec<String> = (0..ix.len() as u32)
            .filter(|&i| !ix.is_root(i))
            .map(|i| ix.full_path(i))
            .collect();
        v.sort();
        v
    }

    fn tree(base: &Path) {
        fs::create_dir_all(base.join("DCIM/Camera")).unwrap();
        fs::create_dir_all(base.join("Download")).unwrap();
        fs::create_dir_all(base.join("Music/a/b")).unwrap();
        for f in [
            "DCIM/Camera/1.jpg",
            "DCIM/Camera/2.jpg",
            "Download/x.pdf",
            "Music/a/b/s.mp3",
            "top.txt",
        ] {
            fs::write(base.join(f), b"data").unwrap();
        }
    }

    #[test]
    fn sweep_without_changes_lists_nothing() {
        let base = tmp("nochange");
        tree(&base);
        let m = [mount_for(&base, "v1")];
        let (full, r0) = rescan(None, &m, &Mode::Full, &Options::default()).unwrap();
        assert!(r0.dirs_listed >= 6);
        let (ix, r) = rescan(Some(&full), &m, &Mode::Sweep, &Options::default()).unwrap();
        assert_eq!(r.dirs_listed, 0, "{r:?}");
        assert!(r.dirs_statted >= 5);
        assert_eq!(names(&ix), names(&full));
        assert_eq!(ix.len(), full.len());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sweep_finds_new_and_deleted_files_listing_only_the_changed_dirs() {
        let base = tmp("sweep");
        tree(&base);
        let m = [mount_for(&base, "v1")];
        let (full, _) = rescan(None, &m, &Mode::Full, &Options::default()).unwrap();
        tick();
        fs::write(base.join("Download/new.apk"), b"zz").unwrap();
        fs::remove_file(base.join("DCIM/Camera/2.jpg")).unwrap();
        let (ix, r) = rescan(Some(&full), &m, &Mode::Sweep, &Options::default()).unwrap();
        let n = names(&ix);
        assert!(n.iter().any(|p| p.ends_with("Download/new.apk")));
        assert!(!n.iter().any(|p| p.ends_with("Camera/2.jpg")));
        assert!(n.iter().any(|p| p.ends_with("Camera/1.jpg")));
        assert_eq!(r.dirs_listed, 2, "solo Download y Camera: {r:?}");
        assert!(r.nodes_copied > 0);
        assert_eq!(ix.generation, full.generation + 1);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn sweep_finds_new_nested_directory() {
        let base = tmp("nested");
        tree(&base);
        let m = [mount_for(&base, "v1")];
        let (full, _) = rescan(None, &m, &Mode::Full, &Options::default()).unwrap();
        tick();
        fs::create_dir_all(base.join("Music/a/b/c")).unwrap();
        fs::write(base.join("Music/a/b/c/deep.flac"), b"1").unwrap();
        let (ix, _) = rescan(Some(&full), &m, &Mode::Sweep, &Options::default()).unwrap();
        assert!(names(&ix)
            .iter()
            .any(|p| p.ends_with("Music/a/b/c/deep.flac")));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn dirty_mode_relists_only_declared_dirs_and_copies_the_rest() {
        let base = tmp("dirty");
        tree(&base);
        let m = [mount_for(&base, "v1")];
        let (full, _) = rescan(None, &m, &Mode::Full, &Options::default()).unwrap();
        tick();
        fs::write(base.join("Download/n.txt"), b"a").unwrap();
        fs::write(base.join("Music/a/b/ignored.mp3"), b"a").unwrap(); // no declarada
        let dirty = Mode::Dirty(vec![normalize_path(
            &base.join("Download").to_string_lossy(),
        )]);
        let (ix, r) = rescan(Some(&full), &m, &dirty, &Options::default()).unwrap();
        let n = names(&ix);
        assert!(n.iter().any(|p| p.ends_with("Download/n.txt")));
        assert!(
            !n.iter().any(|p| p.ends_with("ignored.mp3")),
            "Dirty no mira lo que no se declaro"
        );
        assert_eq!(r.dirs_listed, 1, "{r:?}");
        assert_eq!(r.dirs_statted, 1, "solo el camino hasta Download: {r:?}");
        assert!(r.subtrees_copied >= 2);
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn dirty_mode_handles_deleted_directory() {
        let base = tmp("dirtydel");
        tree(&base);
        let m = [mount_for(&base, "v1")];
        let (full, _) = rescan(None, &m, &Mode::Full, &Options::default()).unwrap();
        fs::remove_dir_all(base.join("Music/a")).unwrap();
        let dirty = Mode::Dirty(vec![
            normalize_path(&base.join("Music").to_string_lossy()),
            normalize_path(&base.join("Music/a").to_string_lossy()),
        ]);
        let (ix, _) = rescan(Some(&full), &m, &dirty, &Options::default()).unwrap();
        assert!(!names(&ix).iter().any(|p| p.contains("Music/a")));
        assert!(names(&ix).iter().any(|p| p.ends_with("Camera/1.jpg")));
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn incremental_result_equals_a_fresh_full_scan() {
        let base = tmp("equiv");
        tree(&base);
        let m = [mount_for(&base, "v1")];
        let (full, _) = rescan(None, &m, &Mode::Full, &Options::default()).unwrap();
        tick();
        fs::write(base.join("Download/y.zip"), b"1").unwrap();
        fs::create_dir_all(base.join("Pictures/Screenshots")).unwrap();
        fs::write(base.join("Pictures/Screenshots/s.png"), b"1").unwrap();
        fs::remove_file(base.join("top.txt")).unwrap();
        let (inc, _) = rescan(Some(&full), &m, &Mode::Sweep, &Options::default()).unwrap();
        let (fresh, _) = rescan(None, &m, &Mode::Full, &Options::default()).unwrap();
        assert_eq!(names(&inc), names(&fresh));
        // El arbol tiene que seguir en preorden valido: tout cubre el subarbol.
        for i in 0..inc.len() as u32 {
            assert!(inc.subtree_end(i) as usize <= inc.len());
            if let Some(p) = inc.parent(i) {
                if p != i {
                    assert!(inc.is_under(p, i), "padre {p} no contiene a {i}");
                }
            }
        }
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn missing_volume_keeps_its_last_copy_offline() {
        let a = tmp("offA");
        let b = tmp("offB");
        tree(&a);
        fs::write(b.join("sd-file.txt"), b"1").unwrap();
        let ms = [mount_for(&a, "int"), mount_for(&b, "sd")];
        let (full, _) = rescan(None, &ms, &Mode::Full, &Options::default()).unwrap();
        let (ix, r) = rescan(Some(&full), &ms[..1], &Mode::Sweep, &Options::default()).unwrap();
        assert_eq!(r.volumes_kept_offline, 1);
        assert_eq!(ix.roots.len(), 2);
        assert!(names(&ix).iter().any(|p| p.ends_with("sd-file.txt")));
        // La SD reaparece con otro punto de montaje: misma identidad, sin duplicar.
        let moved = tmp("offB2");
        fs::write(moved.join("sd-file.txt"), b"1").unwrap();
        let ms2 = [mount_for(&a, "int"), mount_for(&moved, "sd")];
        let (ix2, _) = rescan(Some(&ix), &ms2, &Mode::Sweep, &Options::default()).unwrap();
        assert_eq!(ix2.roots.len(), 2);
        let n = names(&ix2);
        assert_eq!(n.iter().filter(|p| p.ends_with("sd-file.txt")).count(), 1);
        for d in [a, b, moved] {
            let _ = fs::remove_dir_all(&d);
        }
    }

    #[test]
    fn excludes_are_respected() {
        let base = tmp("excl");
        tree(&base);
        fs::create_dir_all(base.join("Android/data/x")).unwrap();
        fs::write(base.join("Android/data/x/secret.db"), b"1").unwrap();
        let m = [mount_for(&base, "v1")];
        let opts = Options {
            excludes: vec![base.join("Android/data")],
            ..Default::default()
        };
        let (ix, _) = rescan(None, &m, &Mode::Full, &opts).unwrap();
        assert!(!names(&ix).iter().any(|p| p.contains("secret.db")));
        let _ = fs::remove_dir_all(&base);
    }
}
