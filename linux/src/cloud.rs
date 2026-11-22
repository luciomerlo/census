//! Identidad de volúmenes de nube: `provider + cuenta + unidad`.
//!
//! El mismo Google Drive puede aparecer como `gvfs` en
//! `/run/user/1000/gvfs/...`, como `rclone` en `/mnt/rclone` y como
//! `google-drive-ocamlfuse` en `~/GoogleDrive`. La letra o el punto de montaje
//! no sirven para identificarlo: `cloud_id` sale de provider + cuenta + unidad
//! y no del camino.
//!
//! Cuando la cuenta no se puede atribuir con datos reales, NO se inventa:
//! `account = "desconocida:<mountpoint>"`, `attributed = false` y el catálogo
//! lo marca `posible_duplicado` (nunca fusión silenciosa). Un override manual
//! manda sobre la detección.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::plataforma::fnv1a64;

pub const GOOGLE_DRIVE: &str = "google-drive";
pub const ONEDRIVE: &str = "onedrive";
pub const DROPBOX: &str = "dropbox";
pub const S3: &str = "s3";

#[derive(Clone, Debug, Default)]
pub struct CloudId {
    pub provider: String,
    pub account: String,
    pub unit: String,
    /// `hash(provider || account || unidad)` en hex, con separador NUL.
    pub id: String,
    /// `false` si la cuenta es `desconocida:` o falta la unidad.
    pub attributed: bool,
    pub note: Option<String>,
}

impl CloudId {
    pub fn possible_duplicate(&self) -> bool {
        !self.attributed
    }
}

/// Override manual (config `cloud_overrides`). Manda sobre la detección.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct CloudOverride {
    /// Coincide por punto de montaje exacto.
    #[serde(default)]
    pub path: Option<String>,
    /// Coincide por `cloud_id` detectado (para unir dos caminos).
    #[serde(default)]
    pub cloud_id: Option<String>,
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub account: Option<String>,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
    #[serde(default)]
    pub date: Option<String>,
}

/// Contexto de deteccion: se carga una vez y se usa para todos los montajes.
#[derive(Clone, Debug, Default)]
pub struct CloudContext {
    /// `[remote] -> clave/valor` de `rclone.conf`.
    pub rclone: HashMap<String, HashMap<String, String>>,
    /// `~/.gdfuse/default/config`.
    pub gdfuse: HashMap<String, String>,
    pub overrides: Vec<CloudOverride>,
}

impl CloudContext {
    pub fn load() -> Self {
        let mut ctx = CloudContext::default();
        if let Some(path) = rclone_config_path() {
            if let Ok(text) = fs::read_to_string(&path) {
                ctx.rclone = parse_ini(&text);
            }
        }
        if let Some(path) = gdfuse_config_path() {
            if let Ok(text) = fs::read_to_string(&path) {
                ctx.gdfuse = parse_kv(&text);
            }
        }
        ctx
    }

    pub fn with_overrides(mut self, overrides: Vec<CloudOverride>) -> Self {
        self.overrides = overrides;
        self
    }
}

fn home() -> PathBuf {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("."))
}

fn rclone_config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CENSUS_RCLONE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    if let Ok(p) = std::env::var("RCLONE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let p = home().join(".config/rclone/rclone.conf");
    p.exists().then_some(p)
}

fn gdfuse_config_path() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CENSUS_GDFUSE_CONFIG") {
        return Some(PathBuf::from(p));
    }
    let p = home().join(".gdfuse/default/config");
    p.exists().then_some(p)
}

/// INI minimalista de rclone: secciones `[remote]` y `clave = valor`.
fn parse_ini(text: &str) -> HashMap<String, HashMap<String, String>> {
    let mut out: HashMap<String, HashMap<String, String>> = HashMap::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
            current = name.trim().to_string();
            out.entry(current.clone()).or_default();
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            if !current.is_empty() {
                out.entry(current.clone())
                    .or_default()
                    .insert(k.trim().to_lowercase(), v.trim().to_string());
            }
        }
    }
    out
}

fn parse_kv(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_lowercase(), v.trim().to_string());
        }
    }
    out
}

/// Hash normativo de nube: FNV-1a 64 sobre `provider\0account\0unidad`,
/// hexadecimal en minusculas. El separador evita colisiones entre campos.
pub fn hash(provider: &str, account: &str, unit: &str) -> String {
    let s = format!("{provider}\0{account}\0{unit}");
    format!("{:016x}", fnv1a64(s.as_bytes()))
}

fn provider_for_rclone(t: &str) -> String {
    match t {
        "drive" | "google drive" => GOOGLE_DRIVE.into(),
        "onedrive" => ONEDRIVE.into(),
        "dropbox" => DROPBOX.into(),
        "s3" => S3.into(),
        "b2" => "backblaze-b2".into(),
        other => format!("rclone:{other}"),
    }
}

fn first(map: &HashMap<String, String>, keys: &[&str]) -> Option<String> {
    for k in keys {
        if let Some(v) = map.get(*k) {
            if !v.trim().is_empty() {
                return Some(v.trim().to_string());
            }
        }
    }
    None
}

fn with_unknown(mut id: CloudId, mountpoint: &str) -> CloudId {
    if id.account.trim().is_empty() {
        id.account = format!("desconocida:{mountpoint}");
        id.note = Some("cuenta no atribuible: se marca posible duplicado".into());
    }
    if id.unit.trim().is_empty() {
        id.unit = "root".into();
        id.note = Some("unidad no atribuible: se marca posible duplicado".into());
    }
    id.attributed = !id.account.starts_with("desconocida:") && !id.unit.is_empty();
    id.id = if id.attributed {
        hash(&id.provider, &id.account, &id.unit)
    } else {
        // Id local, distinto por camino: nunca fusiona dos desconocidos.
        hash(
            &id.provider,
            &format!("desconocida:{mountpoint}"),
            &id.unit,
        )
    };
    id
}

fn apply_override(mut id: CloudId, mountpoint: &str, ctx: &CloudContext) -> CloudId {
    for ov in &ctx.overrides {
        let matches_path = ov.path.as_deref() == Some(mountpoint);
        let matches_id = ov.cloud_id.as_deref() == Some(id.id.as_str());
        if !matches_path && !matches_id {
            continue;
        }
        if let Some(p) = &ov.provider {
            id.provider = p.clone();
        }
        if let Some(a) = &ov.account {
            id.account = a.clone();
        }
        if let Some(u) = &ov.unit {
            id.unit = u.clone();
        }
        id.note = ov.reason.clone().or(id.note);
        id.attributed = !id.account.starts_with("desconocida:") && !id.unit.is_empty();
        id.id = if id.attributed {
            hash(&id.provider, &id.account, &id.unit)
        } else {
            hash(&id.provider, &format!("desconocida:{mountpoint}"), &id.unit)
        };
        break;
    }
    id
}

/// Detecta la identidad de nube de un montaje. Devuelve `None` si no parece
/// nube (red SMB/NFS, disco local, etc.).
pub fn identify(
    ctx: &CloudContext,
    fs: &str,
    source: &str,
    mountpoint: &str,
    super_opts: &str,
) -> Option<CloudId> {
    let mut id = match fs {
        "fuse.rclone" => identify_rclone(ctx, source, mountpoint, super_opts)?,
        "fuse.google-drive-ocamlfuse" => identify_gdfuse(ctx, mountpoint),
        "fuse.s3fs" => identify_s3(source, super_opts, mountpoint),
        "fuse.gvfsd-fuse" => identify_gvfs(mountpoint)?,
        "fuse.onedrive" => basic("onedrive", source, mountpoint),
        "fuse.dropbox" => basic("dropbox", source, mountpoint),
        "fuse.juicefs" => basic("juicefs", source, mountpoint),
        _ if fs.starts_with("fuse.") => return None,
        _ => return None,
    };
    id = with_unknown(id, mountpoint);
    Some(apply_override(id, mountpoint, ctx))
}

fn basic(provider: &str, source: &str, mountpoint: &str) -> CloudId {
    CloudId {
        provider: provider.into(),
        account: source.to_string(),
        unit: source.to_string(),
        ..Default::default()
    }
    .with_mount(mountpoint)
}

impl CloudId {
    fn with_mount(mut self, mountpoint: &str) -> Self {
        if self.account.trim().is_empty() {
            self.account = format!("desconocida:{mountpoint}");
        }
        if self.unit.trim().is_empty() {
            self.unit = "root".into();
        }
        self
    }
}

fn identify_rclone(
    ctx: &CloudContext,
    source: &str,
    mountpoint: &str,
    super_opts: &str,
) -> Option<CloudId> {
    // `rclone mount remote:path /mnt` deja source `remote:path`; algunos
    // montajes dejan `rclone` y el remote real en `fsname=remote:path`.
    let mut spec = source;
    if spec == "rclone" || spec.is_empty() {
        if let Some(v) = super_opts
            .split(',')
            .find_map(|kv| kv.strip_prefix("fsname="))
        {
            spec = v;
        }
    }
    let (remote, subpath) = spec.split_once(':').unwrap_or((spec, ""));
    let conf = ctx.rclone.get(remote).cloned().unwrap_or_default();
    let rtype = conf.get("type").cloned().unwrap_or_default();
    let provider = provider_for_rclone(&rtype);
    let account = first(&conf, &["user", "email", "account", "username", "user_id"])
        .unwrap_or_default();
    let unit = first(
        &conf,
        &[
            "root_folder_id",
            "team_drive",
            "drive_id",
            "bucket",
            "root_folder",
        ],
    )
    .or_else(|| {
        let p = subpath.trim_matches('/');
        (!p.is_empty()).then(|| p.to_string())
    })
    .unwrap_or_default();
    Some(CloudId {
        provider,
        account,
        unit,
        ..Default::default()
    })
}

fn identify_gdfuse(ctx: &CloudContext, mountpoint: &str) -> CloudId {
    let account = first(&ctx.gdfuse, &["account", "user", "email"]).unwrap_or_default();
    let unit = first(&ctx.gdfuse, &["root_folder", "root_folder_id"]).unwrap_or_default();
    CloudId {
        provider: GOOGLE_DRIVE.into(),
        account,
        unit,
        ..Default::default()
    }
    .with_mount(mountpoint)
}

fn identify_s3(source: &str, super_opts: &str, mountpoint: &str) -> CloudId {
    let kv: HashMap<String, String> = super_opts
        .split(',')
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let bucket = kv
        .get("bucket")
        .cloned()
        .or_else(|| {
            let s = source.trim_start_matches("s3fs");
            let s = s.trim_start_matches(':');
            (!s.is_empty()).then(|| s.to_string())
        })
        .unwrap_or_default();
    let prefix = kv.get("prefix").cloned().unwrap_or_default();
    let unit = if prefix.is_empty() {
        bucket
    } else {
        format!("{bucket}/{prefix}")
    };
    let account = kv
        .get("passwd_file")
        .map(|p| format!("passwd:{}", p))
        .unwrap_or_default();
    CloudId {
        provider: S3.into(),
        account,
        unit,
        ..Default::default()
    }
    .with_mount(mountpoint)
}

/// Un subdirectorio de gvfs: `google-drive:host=gmail.com,user=alice@gmail.com`.
fn identify_gvfs(mountpoint: &str) -> Option<CloudId> {
    let name = Path::new(mountpoint)
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (scheme, rest) = name.split_once(':')?;
    if !["google-drive", "onedrive", "dropbox"].contains(&scheme) {
        return None;
    }
    let kv: HashMap<String, String> = rest
        .split(',')
        .filter_map(|s| s.split_once('='))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let provider = scheme.to_string();
    let account = kv
        .get("user")
        .or_else(|| kv.get("account"))
        .or_else(|| kv.get("host"))
        .cloned()
        .unwrap_or_default();
    let unit = kv
        .get("shared_drive")
        .or_else(|| kv.get("team_drive"))
        .map(|s| format!("shared:{s}"))
        .unwrap_or_else(|| "my-drive".into());
    Some(CloudId {
        provider,
        account,
        unit,
        ..Default::default()
    })
}

/// Identidad para una ruta suelta (raiz explicita): gvfs, gdfuse o rclone.
pub fn identify_path(ctx: &CloudContext, path: &Path) -> Option<CloudId> {
    let p = crate::index::path_string(path);
    if p.contains("/gvfs/") {
        return identify_gvfs(&p).map(|id| apply_override(with_unknown(id, &p), &p, ctx));
    }
    let mounts = crate::volumes::mounts();
    let m = mounts
        .into_iter()
        .filter(|m| crate::index::path_prefix_matches(&p, &m.path))
        .max_by_key(|m| m.path.len())?;
    identify(ctx, &m.fs, &m.source, &m.path, "").map(|id| {
        let mut id = id;
        id.id = hash(&id.provider, &id.account, &id.unit);
        id
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_has_nul_separator_and_known_examples() {
        // Ejemplos que se pasan al chat de Windows/Atlas.
        assert_eq!(
            hash("google-drive", "alice@gmail.com", "my-drive"),
            "304122b7badeb551"
        );
        assert_eq!(
            hash("google-drive", "alice@gmail.com", "shared-drive-0ABcD"),
            "77e64295ad12829f"
        );
        assert_eq!(
            hash("google-drive", "desconocida:/mnt/rclone", "my-drive"),
            "67693e0fd92e5de2"
        );
        // No es el mismo hash que concatenar sin separadores.
        assert_ne!(
            hash("google-drive", "alice@gmail.com", "my-drive"),
            format!("{:016x}", fnv1a64(b"google-drivealice@gmail.commy-drive"))
        );
    }

    fn rclone_ctx() -> CloudContext {
        let mut conf = HashMap::new();
        conf.insert("type".to_string(), "drive".to_string());
        conf.insert("user".to_string(), "alice@gmail.com".to_string());
        conf.insert("root_folder_id".to_string(), "my-drive".to_string());
        let mut rclone = HashMap::new();
        rclone.insert("gdrive".to_string(), conf);
        CloudContext {
            rclone,
            ..Default::default()
        }
    }

    #[test]
    fn gvfs_and_rclone_same_drive_hash_matches() {
        let gvfs = identify(
            &CloudContext::default(),
            "fuse.gvfsd-fuse",
            "gvfsd-fuse",
            "/run/user/1000/gvfs/google-drive:host=gmail.com,user=alice@gmail.com",
            "",
        )
        .unwrap();
        let rclone = identify(
            &rclone_ctx(),
            "fuse.rclone",
            "gdrive:",
            "/mnt/rclone",
            "",
        )
        .unwrap();
        assert_eq!(gvfs.provider, "google-drive");
        assert_eq!(rclone.provider, "google-drive");
        assert_eq!(gvfs.account, "alice@gmail.com");
        assert_eq!(rclone.account, "alice@gmail.com");
        assert_eq!(gvfs.unit, "my-drive");
        assert_eq!(rclone.unit, "my-drive");
        assert_eq!(gvfs.id, rclone.id);
        assert_eq!(gvfs.id, "304122b7badeb551");
    }

    #[test]
    fn unknown_account_is_flagged_not_invented() {
        let id = identify(
            &CloudContext::default(),
            "fuse.rclone",
            "otro:/x",
            "/mnt/otro",
            "",
        )
        .unwrap();
        assert!(!id.attributed);
        assert!(id.possible_duplicate());
        assert!(id.account.starts_with("desconocida:"));
        // Tampoco coincide con el camino atribuido.
        assert_ne!(id.id, "304122b7badeb551");
    }

    #[test]
    fn override_wins() {
        let ctx = CloudContext {
            overrides: vec![CloudOverride {
                path: Some("/mnt/rclone".into()),
                account: Some("alice@gmail.com".into()),
                unit: Some("my-drive".into()),
                provider: Some("google-drive".into()),
                reason: Some("mismo drive que gvfs".into()),
                ..Default::default()
            }],
            ..Default::default()
        };
        let id = identify(&ctx, "fuse.rclone", "gdrive:", "/mnt/rclone", "").unwrap();
        assert_eq!(id.id, "304122b7badeb551");
        assert!(id.attributed);
    }
}
