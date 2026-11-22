//! Deteccion de volumenes: `/proc/self/mountinfo`, `statvfs`, UUID, etiquetas
//! e identidad de nube.
//!
//! En Linux no hay letras de unidad: cada punto de montaje es una raiz. Un
//! disco extraible puede aparecer en `/media/usb` en una PC y `/mnt/usb` en
//! otra; el `volume_id` (UUID + fs + tamano) es lo que lo une en el catalogo.
//!
//! La nube se identifica por `provider + cuenta + unidad` (`cloud_id`), no por
//! el punto de montaje: el mismo Google Drive puede estar en gvfs, rclone y
//! google-drive-ocamlfuse a la vez.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::cloud::{self, CloudContext, CloudId};
use crate::plataforma::volume_id;

#[derive(Clone, Debug, Default)]
pub struct Mount {
    pub path: String,
    pub source: String,
    pub fs: String,
    pub label: String,
    pub fs_uuid: String,
    pub volume_id: String,
    pub total: u64,
    pub free: u64,
    pub kind: String,
    pub removable: bool,
    /// Campo `root` de mountinfo (subvolumen en btrfs).
    pub subvol: String,
    pub dev: u64,
    // Identidad de nube.
    pub cloud_id: String,
    pub cloud_provider: String,
    pub cloud_account: String,
    pub cloud_unit: String,
    pub cloud_attributed: bool,
    pub possible_duplicate: bool,
    pub cloud_note: Option<String>,
}

impl Mount {
    pub fn display(&self) -> String {
        if !self.label.is_empty() {
            format!("{} ({})", self.label, self.path)
        } else {
            self.path.clone()
        }
    }

    pub fn is_network(&self) -> bool {
        self.kind == "network"
    }

    pub fn is_cloud(&self) -> bool {
        self.kind == "cloud"
    }

    /// Clave del catalogo: nube por `cloud_id`, el resto por `volume_id`.
    pub fn key(&self) -> &str {
        if !self.cloud_id.is_empty() {
            &self.cloud_id
        } else {
            &self.volume_id
        }
    }

    fn apply_cloud(&mut self, id: CloudId, cloud_id: &str) {
        self.kind = "cloud".into();
        self.cloud_provider = id.provider;
        self.cloud_account = id.account;
        self.cloud_unit = id.unit;
        self.cloud_attributed = id.attributed;
        self.possible_duplicate = id.possible_duplicate();
        self.cloud_note = id.note;
        self.cloud_id = cloud_id.to_string();
        self.fs_uuid = format!(
            "cloud:{}:{}:{}",
            self.cloud_provider, self.cloud_account, self.cloud_unit
        );
        self.volume_id = if id.attributed {
            cloud_id.to_string()
        } else {
            // Un desconocido no se fusiona con otro: id por camino.
            volume_id(&format!("cloud:{}", self.path), &self.fs, self.total)
        };
    }
}

const NETWORK_FS: [&str; 12] = [
    "cifs", "smb3", "smbfs", "nfs", "nfs4", "9p", "ceph", "glusterfs", "sshfs", "fuse.sshfs",
    "davfs", "afs",
];

const CLOUD_FS: [&str; 10] = [
    "fuse.rclone",
    "fuse.google-drive-ocamlfuse",
    "fuse.onedrive",
    "fuse.dropbox",
    "fuse.s3fs",
    "fuse.juicefs",
    "fuse.gvfsd-fuse",
    "fuse.mergerfs",
    "fuse.davfs2",
    "fuse.curlftpfs",
];

const PSEUDO_FS: [&str; 24] = [
    "proc",
    "sysfs",
    "devtmpfs",
    "devpts",
    "tmpfs",
    "cgroup",
    "cgroup2",
    "pstore",
    "securityfs",
    "debugfs",
    "tracefs",
    "configfs",
    "fusectl",
    "bpf",
    "mqueue",
    "hugetlbfs",
    "autofs",
    "binfmt_misc",
    "efivarfs",
    "rpc_pipefs",
    "nfsd",
    "nsfs",
    "selinuxfs",
    "ramfs",
];

fn unescape(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1..i + 4].iter().all(u8::is_ascii_digit) {
            let oct = std::str::from_utf8(&b[i + 1..i + 4]).unwrap_or("0");
            if let Ok(v) = u8::from_str_radix(oct, 8) {
                out.push(v);
                i += 4;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Lee `/dev/disk/by-uuid` y `/dev/disk/by-label` una vez y devuelve
/// `source canonico -> (uuid, label)`.
fn device_index() -> HashMap<PathBuf, (String, String)> {
    let mut map: HashMap<PathBuf, (String, String)> = HashMap::new();
    for (dir, is_uuid) in [("/dev/disk/by-uuid", true), ("/dev/disk/by-label", false)] {
        let Ok(rd) = fs::read_dir(dir) else { continue };
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(target) = fs::read_link(entry.path()) else {
                continue;
            };
            let target = if target.is_absolute() {
                target
            } else {
                Path::new(dir).join(target)
            };
            let key = fs::canonicalize(&target).unwrap_or(target);
            let slot = map.entry(key).or_default();
            if is_uuid {
                slot.0 = name;
            } else {
                slot.1 = name;
            }
        }
    }
    map
}

#[cfg(unix)]
fn statvfs(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::ffi::OsStrExt;
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut st: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(c.as_ptr(), &mut st) };
    if rc != 0 {
        return None;
    }
    let frsize = if st.f_frsize == 0 {
        st.f_bsize as u64
    } else {
        st.f_frsize as u64
    };
    Some((
        (st.f_blocks as u64).saturating_mul(frsize),
        (st.f_bavail as u64).saturating_mul(frsize),
    ))
}

#[cfg(not(unix))]
fn statvfs(_path: &Path) -> Option<(u64, u64)> {
    None
}

#[cfg(unix)]
fn is_removable(major: u64, minor: u64) -> bool {
    fs::read_to_string(format!("/sys/dev/block/{major}:{minor}/removable"))
        .map(|s| s.trim() == "1")
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_removable(_major: u64, _minor: u64) -> bool {
    false
}

fn classify(fs: &str, removable: bool) -> &'static str {
    if NETWORK_FS.contains(&fs) {
        "network"
    } else if CLOUD_FS.contains(&fs) || fs.starts_with("fuse.") {
        "cloud"
    } else if fs == "iso9660" || fs == "udf" {
        "cd"
    } else if removable {
        "removable"
    } else {
        "fixed"
    }
}

fn parse_mountinfo(text: &str, ctx: &CloudContext) -> Vec<Mount> {
    let devs = device_index();
    let mut out: Vec<Mount> = Vec::new();
    let mut seen: HashMap<String, usize> = HashMap::new();
    for line in text.lines() {
        let Some((pre, post)) = line.split_once(" - ") else {
            continue;
        };
        let p: Vec<&str> = pre.split_whitespace().collect();
        let q: Vec<&str> = post.split_whitespace().collect();
        if p.len() < 5 || q.len() < 2 {
            continue;
        }
        let mount_point = unescape(p[4]);
        let fs = q[0].to_string();
        if PSEUDO_FS.contains(&fs.as_str()) || fs.starts_with("cgroup") {
            continue;
        }
        let source = unescape(q[1]);
        if source == "none" || source == "proc" || source == "tmpfs" {
            continue;
        }
        let super_opts = q.get(2).copied().unwrap_or("");
        let (major, minor) = p[2]
            .split_once(':')
            .and_then(|(a, b)| Some((a.parse::<u64>().ok()?, b.parse::<u64>().ok()?)))
            .unwrap_or((0, 0));
        let dev = (major << 32) | minor;
        let removable = is_removable(major, minor);
        let mut kind = classify(&fs, removable).to_string();
        let canon = fs::canonicalize(&source).unwrap_or_else(|_| PathBuf::from(&source));
        let (fs_uuid, label) = devs
            .get(&canon)
            .cloned()
            .unwrap_or_else(|| (String::new(), String::new()));
        let (total, free) = statvfs(Path::new(&mount_point)).unwrap_or((0, 0));
        let fs_uuid = if fs_uuid.is_empty() {
            if kind == "network" || kind == "cloud" {
                format!("{kind}:{source}")
            } else {
                format!("fsid:{major}:{minor}")
            }
        } else {
            fs_uuid
        };
        let vol = volume_id(&fs_uuid, &fs, total);
        let mut m = Mount {
            path: mount_point,
            source,
            fs: fs.clone(),
            label,
            fs_uuid,
            volume_id: vol,
            total,
            free,
            kind: kind.clone(),
            removable,
            subvol: unescape(p[3]),
            dev,
            ..Default::default()
        };
        if let Some(id) = cloud::identify(ctx, &fs, &m.source, &m.path, super_opts) {
            let cid = id.id.clone();
            m.apply_cloud(id, &cid);
            kind = "cloud".into();
        } else if kind == "cloud" {
            // FUSE que no supimos atribuir: se muestra como nube desconocida.
            m.possible_duplicate = true;
            m.cloud_note = Some("sin cuenta/unidad atribuible".into());
        }
        if let Some(&i) = seen.get(&m.path) {
            out[i] = m;
        } else {
            seen.insert(m.path.clone(), out.len());
            out.push(m);
        }
    }
    out
}

/// Todos los montajes reales del sistema (sin pseudo-filesystems).
pub fn mounts() -> Vec<Mount> {
    mounts_with(&CloudContext::load())
}

pub fn mounts_with(ctx: &CloudContext) -> Vec<Mount> {
    #[cfg(unix)]
    {
        if let Ok(text) = fs::read_to_string("/proc/self/mountinfo") {
            let mut out = parse_mountinfo(&text, ctx);
            out.sort_by(|a, b| a.path.len().cmp(&b.path.len()));
            let mut unique: Vec<Mount> = Vec::new();
            for m in out {
                let dup = unique.iter().any(|u| {
                    u.dev == m.dev
                        && u.volume_id == m.volume_id
                        && (m.path.starts_with(&u.path) || u.path.starts_with(&m.path))
                });
                if !dup {
                    unique.push(m);
                }
            }
            unique.sort_by(|a, b| a.path.cmp(&b.path));
            return unique;
        }
        Vec::new()
    }
    #[cfg(not(unix))]
    {
        let _ = ctx;
        Vec::new()
    }
}

/// Subdirectorios de gvfs que son nubes reales. `mountinfo` solo muestra el
/// FUSE raiz, no cada drive; hay que enumerar `/run/user/<uid>/gvfs`.
pub fn cloud_mounts(ctx: &CloudContext) -> Vec<Mount> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(d) = std::env::var("XDG_RUNTIME_DIR") {
        roots.push(PathBuf::from(d).join("gvfs"));
    }
    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        roots.push(PathBuf::from(format!("/run/user/{uid}/gvfs")));
    }
    let mut out = Vec::new();
    for root in roots {
        let Ok(rd) = fs::read_dir(&root) else { continue };
        for entry in rd.flatten() {
            let path = crate::index::path_string(&entry.path());
            let name = entry.file_name().to_string_lossy().into_owned();
            let Some(id) = cloud::identify(ctx, "fuse.gvfsd-fuse", &name, &path, "") else {
                continue;
            };
            let (total, free) = statvfs(&entry.path()).unwrap_or((0, 0));
            let mut m = Mount {
                path,
                source: name,
                fs: "fuse.gvfsd-fuse".into(),
                kind: "cloud".into(),
                total,
                free,
                ..Default::default()
            };
            let cid = id.id.clone();
            m.apply_cloud(id, &cid);
            out.push(m);
        }
    }
    out
}

/// Filtra lo que se indexa por defecto: nada de red, nube ni virtual.
pub fn eligible(m: &Mount) -> bool {
    !m.is_network() && !m.is_cloud() && m.kind != "cd" && m.total > 0
}

/// Montajes que se indexan automaticamente, evitando duplicados anidados.
pub fn auto_roots() -> Vec<Mount> {
    auto_roots_with(&CloudContext::load())
}

pub fn auto_roots_with(ctx: &CloudContext) -> Vec<Mount> {
    let mut out: Vec<Mount> = Vec::new();
    for m in mounts_with(ctx).into_iter().filter(eligible) {
        let nested = out.iter().any(|u| {
            u.volume_id == m.volume_id
                && (m.path.starts_with(&u.path) || u.path.starts_with(&m.path))
        });
        if !nested {
            out.push(m);
        }
    }
    out
}

pub fn mount_for_path(path: &str) -> Option<Mount> {
    mounts()
        .into_iter()
        .filter(|m| crate::index::path_prefix_matches(path, &m.path))
        .max_by_key(|m| m.path.len())
}

pub fn gib(bytes: u64) -> String {
    if bytes == 0 {
        "-".into()
    } else {
        crate::fmt::human(bytes)
    }
}

/// Metadata de una raiz arbitraria (directorio que no es un montaje).
pub fn root_meta_for(path: &Path) -> Mount {
    root_meta_for_with(path, &CloudContext::load())
}

pub fn root_meta_for_with(path: &Path, ctx: &CloudContext) -> Mount {
    let p = crate::index::path_string(path);
    let meta = fs::symlink_metadata(path).ok();
    let (total, free) = statvfs(path).unwrap_or((0, 0));
    let dev = meta.as_ref().map(dev_of).unwrap_or(0);
    if let Some(id) = cloud::identify_path(ctx, path) {
        let mut m = Mount {
            path: p,
            fs: "fuse".into(),
            kind: "cloud".into(),
            total,
            free,
            dev,
            ..Default::default()
        };
        let cid = id.id.clone();
        m.apply_cloud(id, &cid);
        return m;
    }
    let fs_uuid = format!("dir:{}:{}", crate::config::hostname(), p);
    let vol = volume_id(&fs_uuid, "dir", total);
    Mount {
        path: p,
        fs: "dir".into(),
        label: String::new(),
        fs_uuid,
        volume_id: vol,
        total,
        free,
        kind: "dir".into(),
        removable: false,
        subvol: String::new(),
        dev,
        ..Default::default()
    }
}

#[cfg(unix)]
fn dev_of(m: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    let dev = m.dev();
    let major = (dev >> 8) & 0xfff;
    let minor = (dev & 0xff) | ((dev >> 12) & 0xfff00);
    (major << 32) | minor
}

#[cfg(not(unix))]
fn dev_of(_m: &fs::Metadata) -> u64 {
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mountinfo_and_skips_pseudo() {
        let text = "36 35 98:0 /mnt1 /mnt2 rw,noatime master:1 - ext4 /dev/root rw\n\
                    37 35 0:1 / /proc rw - proc proc rw\n\
                    38 35 179:1 / /media/usb rw - vfat /dev/sdb1 rw";
        let m = parse_mountinfo(text, &CloudContext::default());
        assert_eq!(m.len(), 2);
        assert_eq!(m[0].path, "/mnt2");
        assert_eq!(m[0].fs, "ext4");
        assert_eq!(m[1].path, "/media/usb");
        assert_eq!(m[1].fs, "vfat");
    }

    #[test]
    fn unescapes_spaces() {
        assert_eq!(unescape("/media/My\\040Disk"), "/media/My Disk");
    }

    #[test]
    fn classifies_network_and_cloud() {
        assert_eq!(classify("cifs", false), "network");
        assert_eq!(classify("fuse.rclone", false), "cloud");
        assert_eq!(classify("ext4", true), "removable");
        assert_eq!(classify("ext4", false), "fixed");
    }

    #[test]
    fn cloud_mount_gets_cloud_id() {
        let text = "40 35 0:50 / /mnt/rclone rw - fuse.rclone gdrive: rw";
        let m = parse_mountinfo(text, &CloudContext::default());
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].kind, "cloud");
        assert!(m[0].possible_duplicate);
        assert!(m[0].cloud_id.is_empty());
    }
}
