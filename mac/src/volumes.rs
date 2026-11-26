//! Volúmenes de macOS, identidad de disco/partición y nube.
//!
//! §10: la letra o el punto de montaje **no** son identidad.
//!
//! ```text
//! DiskHwId  = hash(serial || vendor_model || capacidad || DiskUUID)
//! volume_id = hash(DiskHwId || offset_particion || fs_uuid)
//! mount     = /Volumes/X  (sólo presentación; se remapea al cargar)
//! cloud_id  = FNV-1a64(provider \0 account \0 unit)  (sin prefijo)
//! ```
//!
//! Si no se puede determinar la identidad, el volumen queda `sin_identidad` y
//! no se publica en federación. Antes un volumen faltante avisado que dos
//! discos fusionados.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::config::Config;
#[allow(unused_imports)]
use crate::types::{
    dir_volume_id, disk_hw_id, fnv1a64, volume_identity, IdentityState, Volume, VolumeId,
    VolumeUuid,
};

#[derive(Clone, Debug)]
pub struct CloudIdentity {
    pub provider: String,
    pub account: String,
    /// `my-drive` o el nombre normalizado del drive compartido.
    pub unit: String,
}

impl CloudIdentity {
    pub fn id(&self) -> String {
        cloud_id(&self.provider, &self.account, &self.unit)
    }
}

/// Identidad de nube normativa. Sin prefijo y con separador NUL, igual en las
/// tres plataformas. Vectores:
/// `("gdrive","alice@gmail.com","my-drive")` -> `28590f656b6fe252`.
pub fn cloud_id(provider: &str, account: &str, unit: &str) -> String {
    let p = normalize_provider(provider);
    let a = normalize_account(account);
    let u = normalize_unit(unit);
    let h = fnv1a64(&[p.as_bytes(), &[0], a.as_bytes(), &[0], u.as_bytes()]);
    format!("{h:016x}")
}

pub fn normalize_provider(p: &str) -> String {
    let p = p.trim().to_ascii_lowercase();
    match p.as_str() {
        "gdrive"
        | "google"
        | "google-drive"
        | "googledrive"
        | "gdfuse"
        | "google-drive-ocamlfuse"
        | "rclone-gdrive"
        | "google drive" => "gdrive".into(),
        "onedrive" | "one-drive" | "microsoft-onedrive" | "one drive" => "onedrive".into(),
        "dropbox" => "dropbox".into(),
        "icloud" | "icloud-drive" | "mobile-documents" | "icloud drive" => "icloud".into(),
        "" => "unknown".into(),
        other => other.into(),
    }
}

pub fn normalize_account(account: &str) -> String {
    let mut a = account.trim().to_ascii_lowercase();
    for prefix in [
        "googledrive-",
        "google drive - ",
        "onedrive-",
        "one drive - ",
        "dropbox-",
        "dropbox - ",
    ] {
        if let Some(rest) = a.strip_prefix(prefix) {
            a = rest.trim().to_string();
            break;
        }
    }
    for suffix in [
        " - google drive",
        " - onedrive",
        " - one drive",
        " - dropbox",
    ] {
        if let Some(rest) = a.strip_suffix(suffix) {
            a = rest.trim().to_string();
            break;
        }
    }
    a
}

pub fn normalize_unit(unit: &str) -> String {
    let u = unit.trim().to_ascii_lowercase();
    let u = u.replace('\\', "/");
    let u = u.trim_matches('/').to_string();
    let u = u
        .strip_prefix("shared drives/")
        .or_else(|| u.strip_prefix("shared drive/"))
        .or_else(|| u.strip_prefix("drives compartidos/"))
        .unwrap_or(&u)
        .to_string();
    let u = u.split_whitespace().collect::<Vec<_>>().join(" ");
    match u.as_str() {
        "" => String::new(),
        "my drive"
        | "mi unidad"
        | "onedrive"
        | "onedrive - personal"
        | "dropbox"
        | "icloud drive" => "my-drive".into(),
        other => other.to_string(),
    }
}

/// Detecta la identidad de un montaje de nube a partir de la ruta, el nombre
/// del volumen y el tipo de filesystem. `None` si no puede atribuir
/// cuenta/unidad: mejor un aviso que una fusión silenciosa.
pub fn identity_from_mount(path: &Path, volume_name: &str, fs: &str) -> Option<CloudIdentity> {
    let p = path.to_string_lossy();
    let pl = p.to_ascii_lowercase();
    let vl = volume_name.to_ascii_lowercase();
    let fl = fs.to_ascii_lowercase();

    // 1) File Provider moderno: ~/Library/CloudStorage/<Provider>-<account>/...
    if let Some(rest) = pl.split("library/cloudstorage/").nth(1) {
        let mut it = rest.splitn(3, '/');
        let dom = it.next()?;
        let provider = if dom.starts_with("googledrive-") {
            "gdrive"
        } else if dom.starts_with("onedrive-") {
            "onedrive"
        } else if dom.starts_with("dropbox-") {
            "dropbox"
        } else if dom.starts_with("icloud") {
            "icloud"
        } else {
            return None;
        };
        let account = match provider {
            "gdrive" => dom.strip_prefix("googledrive-").unwrap_or(""),
            "onedrive" => dom.strip_prefix("onedrive-").unwrap_or(""),
            "dropbox" => dom.strip_prefix("dropbox-").unwrap_or(""),
            _ => "",
        };
        let rest = it.collect::<Vec<_>>().join("/");
        let unit = if rest.is_empty() || rest.starts_with("my drive") {
            "my-drive".to_string()
        } else {
            rest
        };
        if account.trim().is_empty() {
            return None;
        }
        return Some(CloudIdentity {
            provider: provider.into(),
            account: account.into(),
            unit,
        });
    }

    // 2) iCloud Drive local: la cuenta no se expone en la ruta.
    if pl.contains("mobile documents") || vl.contains("icloud drive") {
        return None;
    }

    // 3) Montajes en /Volumes: Google Drive, OneDrive, Dropbox, rclone.
    let provider = if vl.contains("google drive") || vl.contains("googledrive") {
        "gdrive"
    } else if vl.contains("onedrive") || vl.contains("one drive") {
        "onedrive"
    } else if vl.contains("dropbox") {
        "dropbox"
    } else if pl.contains("gdrive") || pl.contains("gdfuse") || fl.contains("gdfuse") {
        "gdrive"
    } else {
        return None;
    };
    let account = extract_account(volume_name)
        .or_else(|| account_from_path(&p))
        .unwrap_or_default();
    if account.trim().is_empty() {
        return None;
    }
    let unit = unit_from_path(&p);
    Some(CloudIdentity {
        provider: provider.into(),
        account,
        unit,
    })
}

fn extract_account(name: &str) -> Option<String> {
    let lower = name.to_ascii_lowercase();
    for suffix in [
        " - google drive",
        " - onedrive",
        " - one drive",
        " - dropbox",
    ] {
        if let Some(i) = lower.find(suffix) {
            let a = name[..i].trim();
            if a.contains('@') {
                return Some(a.to_string());
            }
        }
    }
    if let Some((_, a)) = name.split_once('-') {
        if a.contains('@') {
            return Some(a.to_string());
        }
    }
    None
}

fn account_from_path(p: &str) -> Option<String> {
    for part in p.split('/') {
        if part.contains('@') && part.len() <= 254 {
            return Some(part.to_string());
        }
    }
    None
}

fn unit_from_path(p: &str) -> String {
    let lower = p.to_ascii_lowercase();
    if let Some(i) = lower.find("shared drives/") {
        return p[i + "shared drives/".len()..].to_string();
    }
    if let Some(i) = lower.find("shared drive/") {
        return p[i + "shared drive/".len()..].to_string();
    }
    if lower.contains("my drive") || lower.contains("mi unidad") {
        return "my-drive".into();
    }
    "my-drive".into()
}

/// Override manual de un montaje que la heurística no puede atribuir.
pub fn alias_identity(cfg: &Config, path: &Path) -> Option<CloudIdentity> {
    let p = path.to_string_lossy();
    cfg.cloud_aliases
        .iter()
        .filter(|a| p.starts_with(&a.path_prefix))
        .max_by_key(|a| a.path_prefix.len())
        .map(|a| CloudIdentity {
            provider: a.provider.clone(),
            account: a.account.clone(),
            unit: a.unit.clone(),
        })
}

/// Agrupa volúmenes por `cloud_id`. Devuelve `(grupos, posibles duplicados)`.
pub fn group_cloud(vols: &[Volume]) -> (HashMap<String, Vec<String>>, Vec<String>) {
    let mut groups: HashMap<String, Vec<String>> = HashMap::new();
    let mut dudosos = Vec::new();
    for v in vols {
        match &v.cloud_id {
            Some(id) => groups.entry(id.clone()).or_default().push(v.mount.clone()),
            None if v.cloud => dudosos.push(v.mount.clone()),
            None => {}
        }
    }
    (groups, dudosos)
}

/// Raíces que se indexan según la configuración y los overrides.
pub fn indexable_roots(cfg: &Config, vols: &[Volume]) -> Vec<(VolumeId, String)> {
    let mut out = Vec::new();
    for v in vols {
        if v.cloud && !cfg.include_cloud {
            continue;
        }
        if v.network && !cfg.include_network {
            continue;
        }
        if !cfg.volume_enabled(&v.id, v.cloud_id.as_deref()) {
            continue;
        }
        for r in &v.roots {
            out.push((v.id.clone(), r.clone()));
        }
    }
    out
}

/// La federación sólo acepta identidades inequívocas.
pub fn federation_ok(v: &Volume) -> bool {
    match v.identity_state {
        IdentityState::Ok => true,
        IdentityState::Cloud => v.cloud_id.is_some(),
        IdentityState::PosibleDuplicado | IdentityState::SinIdentidad => false,
    }
}

/// Mini-parser de `plist` XML: `key` seguido inmediatamente por
/// string/integer/true/false. No se usa un crate de plist para no sumar
/// dependencias a un comando que sólo corre en macOS.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn plist_values(xml: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    let mut pos = 0usize;
    while let Some(i) = xml[pos..].find("<key>") {
        let start = pos + i + 5;
        let Some(j) = xml[start..].find("</key>") else {
            break;
        };
        let key = xml[start..start + j].trim().to_string();
        let rest = start + j + 6;
        let after = &xml[rest..];
        let ws = after.len() - after.trim_start().len();
        let base = rest + ws;
        let tail = &xml[base..];
        let value = if let Some(t) = tail.strip_prefix("<string>") {
            t.find("</string>").map(|e| unescape(&t[..e]))
        } else if let Some(t) = tail.strip_prefix("<integer>") {
            t.find("</integer>").map(|e| t[..e].trim().to_string())
        } else if tail.starts_with("<true/>") {
            Some("true".into())
        } else if tail.starts_with("<false/>") {
            Some("false".into())
        } else {
            None
        };
        if let Some(v) = value {
            out.insert(key, v);
        }
        pos = rest;
    }
    out
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn unescape(s: &str) -> String {
    s.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
}

#[cfg(target_os = "macos")]
mod backend {
    use super::*;
    use std::ffi::{CStr, CString};

    #[derive(Clone, Debug, Default)]
    pub(crate) struct DiskInfo {
        pub(crate) device: String,
        pub(crate) parent: String,
        pub(crate) volume_uuid: String,
        pub(crate) disk_uuid: String,
        pub(crate) media_name: String,
        pub(crate) serial: String,
        pub(crate) total: u64,
        pub(crate) offset: u64,
        pub(crate) mount: String,
    }

    pub fn detect(cfg: &Config, host: &str) -> Vec<Volume> {
        let mut out = Vec::new();
        let mut bufp: *mut libc::statfs = std::ptr::null_mut();
        let n = unsafe { libc::getmntinfo(&mut bufp, libc::MNT_NOWAIT) };
        if n > 0 && !bufp.is_null() {
            for i in 0..n as isize {
                let st = unsafe { &*bufp.offset(i) };
                let mount = cstr(&st.f_mntonname);
                let fs = cstr(&st.f_fstypename);
                let from = cstr(&st.f_mntfromname);
                let total = st.f_blocks.saturating_mul(st.f_bsize as u64);
                let free = st.f_bavail.saturating_mul(st.f_bsize as u64);
                let local = st.f_flags as i32 & libc::MNT_LOCAL != 0;
                let network = !local
                    || matches!(
                        fs.to_ascii_lowercase().as_str(),
                        "nfs" | "smbfs" | "smb" | "afpfs" | "webdav" | "ftp"
                    );
                let name = volume_name(&mount).unwrap_or_else(|| {
                    Path::new(&mount)
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| from.clone())
                });
                let uuid = volume_uuid(&mount).unwrap_or([0; 16]);
                let info = disk_info(&mount).unwrap_or_default();
                let disk_guid = if info.disk_uuid.is_empty() {
                    None
                } else {
                    Some(info.disk_uuid.as_str())
                };
                let serial = if info.serial.is_empty() {
                    None
                } else {
                    Some(info.serial.as_str())
                };
                let vendor = if info.media_name.is_empty() {
                    name.clone()
                } else {
                    info.media_name.clone()
                };
                let hw = disk_hw_id(serial, &vendor, total.max(info.total), disk_guid);
                let fs_uuid = if info.volume_uuid.is_empty() {
                    crate::types::uuid_hex(&uuid)
                } else {
                    info.volume_uuid.clone()
                };
                let offset = info.offset;
                let identity = alias_identity(cfg, Path::new(&mount))
                    .or_else(|| identity_from_mount(Path::new(&mount), &name, &fs));
                let cloud = identity.is_some() || looks_cloud(&mount, &name, &fs);
                let cloud_id = identity.as_ref().map(|i| i.id());
                let id = if let Some(c) = &cloud_id {
                    VolumeId::from(c.as_str())
                } else if let Some(hw) = &hw {
                    let usable = !fs_uuid.is_empty() || offset > 0;
                    if usable {
                        volume_identity(hw, offset, &fs_uuid)
                    } else {
                        dir_volume_id(host, &mount)
                    }
                } else {
                    dir_volume_id(host, &mount)
                };
                let identity_state = if let Some(_c) = &cloud_id {
                    IdentityState::Cloud
                } else if cloud {
                    IdentityState::PosibleDuplicado
                } else if hw.is_some() && (!fs_uuid.is_empty() || offset > 0) {
                    IdentityState::Ok
                } else {
                    IdentityState::SinIdentidad
                };
                out.push(Volume {
                    id,
                    disk_hw_id: hw,
                    disk_guid: disk_guid.map(|s| s.to_string()),
                    serial: serial.map(|s| s.to_string()),
                    // `MediaName` es el modelo del aparato; `vendor` arriba
                    // ya lo usa para `DiskHwId` cuando falta el serial.
                    model: Some(vendor.clone()).filter(|m| !m.is_empty()),
                    partition_offset: offset,
                    identity_state,
                    cloud_id,
                    possible_duplicate: identity_state == IdentityState::PosibleDuplicado,
                    uuid,
                    name,
                    mount: mount.clone(),
                    fs: fs.clone(),
                    total,
                    free,
                    local,
                    network,
                    cloud,
                    removable: mount.starts_with("/Volumes/") && mount != "/Volumes",
                    dev: 0,
                    method: if local && !cloud {
                        "getattrlistbulk".into()
                    } else {
                        "walk".into()
                    },
                    roots: vec![mount],
                    nodes: 0,
                });
            }
        }
        if cfg.include_cloud {
            for p in cloud_roots(cfg) {
                if !p.exists() {
                    continue;
                }
                let name = p
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let identity = alias_identity(cfg, &p)
                    .or_else(|| identity_from_mount(&p, &name, "cloudstorage"));
                let mount = p.to_string_lossy().into_owned();
                let cloud_id = identity.as_ref().map(|i| i.id());
                let state = if cloud_id.is_some() {
                    IdentityState::Cloud
                } else {
                    IdentityState::PosibleDuplicado
                };
                out.push(Volume {
                    id: cloud_id
                        .as_deref()
                        .map(VolumeId::from)
                        .unwrap_or_else(|| dir_volume_id(host, &mount)),
                    disk_hw_id: None,
                    disk_guid: None,
                    serial: None,
                    model: None,
                    partition_offset: 0,
                    identity_state: state,
                    cloud_id,
                    possible_duplicate: state == IdentityState::PosibleDuplicado,
                    uuid: [0; 16],
                    name,
                    mount: mount.clone(),
                    fs: "cloudstorage".into(),
                    total: 0,
                    free: 0,
                    local: true,
                    network: false,
                    cloud: true,
                    removable: false,
                    dev: 0,
                    method: "walk".into(),
                    roots: vec![mount],
                    nodes: 0,
                });
            }
        }
        out.sort_by(|a, b| a.mount.cmp(&b.mount));
        out
    }

    fn cstr(buf: &[libc::c_char]) -> String {
        unsafe { CStr::from_ptr(buf.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    }

    fn looks_cloud(mount: &str, name: &str, fs: &str) -> bool {
        let m = format!(
            "{} {} {}",
            mount.to_ascii_lowercase(),
            name.to_ascii_lowercase(),
            fs.to_ascii_lowercase()
        );
        [
            "cloudstorage",
            "google drive",
            "googledrive",
            "onedrive",
            "dropbox",
            "icloud",
            "gdrive",
            "rclone",
        ]
        .iter()
        .any(|s| m.contains(s))
    }

    pub fn cloud_roots(cfg: &Config) -> Vec<PathBuf> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        let mut out = Vec::new();
        for raw in &cfg.cloud_roots {
            let p = if let Some(rest) = raw.strip_prefix('~') {
                home.join(rest.trim_start_matches('/'))
            } else {
                PathBuf::from(raw)
            };
            if !p.exists() {
                continue;
            }
            if p.ends_with("CloudStorage") {
                if let Ok(rd) = std::fs::read_dir(&p) {
                    for ent in rd.flatten() {
                        let dom = ent.path();
                        if !dom.is_dir() {
                            continue;
                        }
                        let my = dom.join("My Drive");
                        if my.exists() {
                            out.push(my);
                        }
                        let shared = dom.join("Shared drives");
                        if let Ok(rd) = std::fs::read_dir(&shared) {
                            for s in rd.flatten() {
                                if s.path().is_dir() {
                                    out.push(s.path());
                                }
                            }
                        }
                    }
                }
            } else if p.ends_with("Mobile Documents") {
                let icloud = p.join("com~apple~CloudDocs");
                if icloud.exists() {
                    out.push(icloud);
                }
            }
        }
        out
    }

    fn volume_name(mount: &str) -> Option<String> {
        let c = CString::new(mount.as_bytes()).ok()?;
        let mut alist: libc::attrlist = unsafe { std::mem::zeroed() };
        alist.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        alist.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_NAME;
        let mut buf = [0u8; 1024];
        let rc = unsafe {
            libc::getattrlist(
                c.as_ptr(),
                &mut alist as *mut _ as *mut libc::c_void,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        let len = u32::from_ne_bytes(buf[0..4].try_into().ok()?) as usize;
        if len < 12 || len > buf.len() {
            return None;
        }
        let off = i32::from_ne_bytes(buf[4..8].try_into().ok()?) as isize;
        let slen = u32::from_ne_bytes(buf[8..12].try_into().ok()?) as usize;
        let start = 4isize.checked_add(off)? as usize;
        let end = start.checked_add(slen)?;
        if end > buf.len() || slen == 0 {
            return None;
        }
        let s = &buf[start..end];
        let s = if s.last() == Some(&0) {
            &s[..s.len() - 1]
        } else {
            s
        };
        Some(String::from_utf8_lossy(s).into_owned())
    }

    fn volume_uuid(mount: &str) -> Option<VolumeUuid> {
        let c = CString::new(mount.as_bytes()).ok()?;
        let mut alist: libc::attrlist = unsafe { std::mem::zeroed() };
        alist.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        alist.volattr = libc::ATTR_VOL_INFO | libc::ATTR_VOL_UUID;
        let mut buf = [0u8; 64];
        let rc = unsafe {
            libc::getattrlist(
                c.as_ptr(),
                &mut alist as *mut _ as *mut libc::c_void,
                buf.as_mut_ptr() as *mut libc::c_void,
                buf.len(),
                0,
            )
        };
        if rc != 0 {
            return None;
        }
        let len = u32::from_ne_bytes(buf[0..4].try_into().ok()?) as usize;
        if len < 20 {
            return None;
        }
        let mut uuid = [0u8; 16];
        uuid.copy_from_slice(&buf[4..20]);
        Some(uuid)
    }

    /// `diskutil info -plist` del volumen y del disco entero. Es la vía que
    /// expone `DiskUUID` (mejor que el serial cuando falta) sin leer `disk2`.
    fn disk_info(mount: &str) -> Option<DiskInfo> {
        let text = diskutil_plist(mount)?;
        let mut d = DiskInfo {
            mount: mount.into(),
            ..Default::default()
        };
        apply(&mut d, &text);
        let parent = if !d.parent.is_empty() {
            Some(d.parent.clone())
        } else {
            d.device.rsplit_once('s').map(|dev| dev.0.to_string())
        };
        if let Some(parent) = parent {
            if let Some(t) = diskutil_plist(&parent) {
                let mut whole = DiskInfo::default();
                apply(&mut whole, &t);
                d.disk_uuid = whole.disk_uuid;
                if d.serial.is_empty() {
                    d.serial = whole.serial;
                }
                if d.media_name.is_empty() {
                    d.media_name = whole.media_name;
                }
                if d.total == 0 {
                    d.total = whole.total;
                }
            }
        }
        Some(d)
    }

    fn diskutil_plist(dev: &str) -> Option<String> {
        let out = std::process::Command::new("diskutil")
            .args(["info", "-plist", dev])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        Some(String::from_utf8_lossy(&out.stdout).into_owned())
    }

    fn apply(d: &mut DiskInfo, xml: &str) {
        let p = plist_values(xml);
        if let Some(v) = p.get("DeviceIdentifier") {
            d.device = v.clone();
        }
        if let Some(v) = p.get("ParentWholeDisk") {
            d.parent = v.clone();
        }
        if let Some(v) = p.get("VolumeUUID") {
            d.volume_uuid = v.clone();
        }
        if let Some(v) = p.get("DiskUUID") {
            d.disk_uuid = v.clone();
        }
        if let Some(v) = p.get("MediaName") {
            d.media_name = v.clone();
        }
        if let Some(v) = p
            .get("SerialNumber")
            .or_else(|| p.get("IORegistryEntryName"))
        {
            d.serial = v.clone();
        }
        if let Some(v) = p.get("TotalSize").and_then(|s| s.parse().ok()) {
            d.total = v;
        }
        if let Some(v) = p
            .get("PartitionOffset")
            .or_else(|| p.get("PartitionMapPartitionOffset"))
            .and_then(|s| s.parse().ok())
        {
            d.offset = v;
        }
        if let Some(v) = p.get("MountPoint") {
            d.mount = v.clone();
        }
    }

    /// Test hook: mismos vectores que Linux.
    #[allow(dead_code)]
    pub(crate) fn _disk_info_for_tests(xml: &str) -> DiskInfo {
        let mut d = DiskInfo::default();
        apply(&mut d, xml);
        d
    }
}

#[cfg(not(target_os = "macos"))]
mod backend {
    use super::*;

    pub fn detect(cfg: &Config, host: &str) -> Vec<Volume> {
        let mut out = Vec::new();
        for raw in &cfg.roots {
            let p = PathBuf::from(raw);
            if p.exists() {
                let mut v = crate::source::synthetic_volume(&p, host);
                v.identity_state = IdentityState::SinIdentidad;
                out.push(v);
            }
        }
        out
    }

    #[allow(dead_code)]
    pub fn cloud_roots(_cfg: &Config) -> Vec<PathBuf> {
        Vec::new()
    }
}

pub use backend::detect;

/// UUID del hardware para sembrar `pc_id`. En macOS se consulta IOKit sin
/// dependencias; si falla, `None` y `pc_id` usa host + nonce persistente.
pub fn platform_uuid() -> Option<String> {
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("IOPlatformUUID") {
                if let Some(v) = line.split('=').nth(1) {
                    return Some(v.trim().trim_matches('"').to_string());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_shared_drive_two_mounts_same_cloud_id() {
        let a = identity_from_mount(
            Path::new(
                "/Users/lucio/Library/CloudStorage/GoogleDrive-lucio@example.com/Shared drives/Proyectos",
            ),
            "Google Drive",
            "cloudstorage",
        )
        .expect("Google Drive se atribuye");
        let b = CloudIdentity {
            provider: "gdrive".into(),
            account: "lucio@example.com".into(),
            unit: "proyectos".into(),
        };
        assert_eq!(a.id(), b.id());
        assert_eq!(a.id(), "db58638081bb5d6f");
    }

    #[test]
    fn cloud_id_test_vectors_are_shared() {
        assert_eq!(
            cloud_id("gdrive", "alice@gmail.com", "my-drive"),
            "28590f656b6fe252"
        );
        assert_eq!(
            cloud_id(
                "Google Drive",
                "LUCIO@example.com - Google Drive",
                "Shared drives/Proyectos"
            ),
            "db58638081bb5d6f"
        );
        assert_ne!(
            cloud_id("gdrive", "alice@gmail.com", "my-drive"),
            cloud_id("gdrive", "otra@gmail.com", "my-drive")
        );
    }

    #[test]
    fn unattributed_cloud_is_not_merged() {
        let id = identity_from_mount(Path::new("/Volumes/gdrive"), "rclone", "fuse");
        assert!(id.is_none(), "sin cuenta no se inventa cloud_id");
        let v = Volume {
            cloud: true,
            cloud_id: None,
            mount: "/Volumes/gdrive".into(),
            ..Default::default()
        };
        let (groups, dudosos) = group_cloud(&[v]);
        assert!(groups.is_empty());
        assert_eq!(dudosos, vec!["/Volumes/gdrive"]);
    }

    #[test]
    fn icloud_without_account_is_warning() {
        assert!(identity_from_mount(
            Path::new("/Users/lucio/Library/Mobile Documents/com~apple~CloudDocs"),
            "iCloud Drive",
            "apfs"
        )
        .is_none());
    }

    #[test]
    fn alias_wins_for_rclone() {
        let mut cfg = Config::default();
        cfg.cloud_aliases.push(crate::config::CloudAlias {
            path_prefix: "/Volumes/gdrive".into(),
            provider: "gdrive".into(),
            account: "lucio@example.com".into(),
            unit: "proyectos".into(),
        });
        let id = alias_identity(&cfg, Path::new("/Volumes/gdrive/Proyectos")).unwrap();
        assert_eq!(
            id.id(),
            cloud_id("gdrive", "lucio@example.com", "proyectos")
        );
    }

    #[test]
    fn federation_rejects_unknown_identity() {
        let v = Volume {
            identity_state: IdentityState::SinIdentidad,
            ..Default::default()
        };
        assert!(!federation_ok(&v));
    }

    #[test]
    fn parses_plist_immediate_values() {
        let xml = r#"<plist><dict>
            <key>DeviceIdentifier</key><string>disk4s1</string>
            <key>TotalSize</key><integer>4000787030016</integer>
            <key>MountPoint</key><string>/Volumes/Backup</string>
        </dict></plist>"#;
        let p = plist_values(xml);
        assert_eq!(
            p.get("DeviceIdentifier").map(String::as_str),
            Some("disk4s1")
        );
        assert_eq!(
            p.get("TotalSize").map(String::as_str),
            Some("4000787030016")
        );
        assert_eq!(
            p.get("MountPoint").map(String::as_str),
            Some("/Volumes/Backup")
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn parses_diskutil_plist_fields() {
        let xml = r#"<plist><dict>
            <key>DeviceIdentifier</key><string>disk4s1</string>
            <key>ParentWholeDisk</key><string>disk4</string>
            <key>VolumeUUID</key><string>VOL-UUID</string>
            <key>DiskUUID</key><string>DISK-UUID</string>
            <key>MediaName</key><string>Seagate Backup+</string>
            <key>TotalSize</key><integer>4000787030016</integer>
            <key>MountPoint</key><string>/Volumes/Backup</string>
        </dict></plist>"#;
        let d = backend::_disk_info_for_tests(xml);
        assert_eq!(d.device, "disk4s1");
        assert_eq!(d.parent, "disk4");
        assert_eq!(d.volume_uuid, "VOL-UUID");
        assert_eq!(d.disk_uuid, "DISK-UUID");
        assert_eq!(d.total, 4_000_787_030_016);
    }
}
