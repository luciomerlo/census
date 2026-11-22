//! Configuración, rutas de macOS y selección explícita de volúmenes.
//!
//! Todo se puede redirigir con `CENSUS_DATA_DIR` y `CENSUS_CONFIG_DIR`, que es
//! lo que usan los tests para no tocar el perfil real.
//!
//! Reglas de Atlas que viven acá:
//! * las listas de activación usan `volume_id` o `cloud_id`, nunca la letra;
//! * los volúmenes offline no se leen por defecto: hay que marcarlos;
//! * un override manual manda sobre la detección automática.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

fn d_port() -> u16 {
    8478
}
fn d_bind() -> String {
    "loopback".into()
}
fn d_tailscale() -> String {
    "auto".into()
}
fn d_true() -> bool {
    true
}

fn default_excludes() -> Vec<String> {
    [
        "/dev",
        "/System/Volumes/VM",
        "/System/Volumes/Preboot",
        "/System/Volumes/Update",
        "/System/Volumes/xarts",
        "/System/Volumes/iSCPreboot",
        "/System/Volumes/Hardware",
        "/private/var/vm",
        "/private/var/db/dyld",
        "/Volumes/com.apple.TimeMachine.localsnapshots",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

fn default_cloud_roots() -> Vec<String> {
    vec![
        "~/Library/CloudStorage".into(),
        "~/Library/Mobile Documents".into(),
    ]
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OverrideAction {
    Include,
    Exclude,
    Join,
    Separate,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Override {
    /// `(pc_id, volume_id|cloud_id)` o sólo el id local.
    pub target: String,
    pub action: OverrideAction,
    #[serde(default)]
    pub other: String,
    #[serde(default)]
    pub motivo: String,
    /// Fecha ISO-8601 del momento en que se tomó la decisión.
    #[serde(default)]
    pub fecha: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OfflinePick {
    pub pc_id: String,
    pub volume_id: String,
}

/// Montaje que la heurística no puede atribuir (rclone, FUSE propio).
/// `path_prefix` es sólo detección local; la identidad sigue siendo
/// `provider/account/unit`.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CloudAlias {
    pub path_prefix: String,
    pub provider: String,
    pub account: String,
    pub unit: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "d_port")]
    pub port: u16,
    /// `loopback`, `tailscale`, `auto` o una IP literal.
    #[serde(default = "d_bind")]
    pub bind: String,
    /// Si no está vacío, la API exige `Authorization: Bearer <token>`.
    #[serde(default)]
    pub token: String,
    #[serde(default)]
    pub env: String,
    /// Raíces extra que se indexan además de los volúmenes detectados.
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default = "default_excludes")]
    pub exclude: Vec<String>,
    /// Volúmenes de red (`smb`, `nfs`, `afp`, `webdav`) fuera por defecto.
    #[serde(default)]
    pub include_network: bool,
    /// Nube (iCloud, Google Drive, OneDrive, Dropbox) fuera por defecto.
    #[serde(default)]
    pub include_cloud: bool,
    #[serde(default = "default_cloud_roots")]
    pub cloud_roots: Vec<String>,
    #[serde(default)]
    pub volumes_enabled: Vec<String>,
    #[serde(default)]
    pub clouds_enabled: Vec<String>,
    /// Alias manuales para montajes de nube sin cuenta/unidad en la ruta.
    #[serde(default)]
    pub cloud_aliases: Vec<CloudAlias>,
    #[serde(default)]
    pub offline_selection: Vec<OfflinePick>,
    #[serde(default)]
    pub overrides: Vec<Override>,
    /// 0 = un hilo por núcleo disponible.
    #[serde(default)]
    pub jobs: usize,
    #[serde(default)]
    pub prefer_local: bool,
    /// Comando de Tailscale (por si está en otra ruta).
    #[serde(default = "d_tailscale")]
    pub tailscale: String,
    /// Escuchar cambios FSEvents y reindexar en caliente al correr `serve`.
    #[serde(default = "d_true")]
    pub watch: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: d_port(),
            bind: d_bind(),
            token: String::new(),
            env: String::new(),
            roots: Vec::new(),
            exclude: default_excludes(),
            include_network: false,
            include_cloud: false,
            cloud_roots: default_cloud_roots(),
            volumes_enabled: Vec::new(),
            clouds_enabled: Vec::new(),
            cloud_aliases: Vec::new(),
            offline_selection: Vec::new(),
            overrides: Vec::new(),
            jobs: 0,
            prefer_local: false,
            tailscale: d_tailscale(),
            watch: true,
        }
    }
}

impl Config {
    pub fn load() -> Self {
        let path = config_path();
        match fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
                eprintln!(
                    "aviso: {} no es JSON válido ({e}); se usan valores por defecto",
                    path.display()
                );
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    pub fn save(&mut self) -> Result<(), String> {
        if self.token.trim().is_empty() {
            self.token = generate_token();
        }
        let path = config_path();
        write_json(&path, self)
    }

    /// Token existente o uno nuevo, sin escribir nada si ya había.
    pub fn token_or_create(&mut self) -> String {
        if self.token.trim().is_empty() {
            self.token = generate_token();
            let _ = self.save();
        }
        self.token.clone()
    }

    pub fn env_label(&self) -> String {
        let e = self.env.trim();
        if !e.is_empty() {
            e.to_string()
        } else {
            hostname()
        }
    }

    pub fn exclude_paths(&self) -> Vec<PathBuf> {
        self.exclude.iter().map(PathBuf::from).collect()
    }

    pub fn volume_enabled(&self, volume_id: &str, cloud_id: Option<&str>) -> bool {
        // Override manual primero.
        if let Some(o) = self
            .overrides
            .iter()
            .rev()
            .find(|o| o.target == volume_id || cloud_id == Some(o.target.as_str()))
        {
            return o.action == OverrideAction::Include;
        }
        if let Some(cloud) = cloud_id {
            if !self.clouds_enabled.is_empty() {
                return self.clouds_enabled.iter().any(|v| v == cloud);
            }
            return true;
        }
        if !self.volumes_enabled.is_empty() {
            return self.volumes_enabled.iter().any(|v| v == volume_id);
        }
        true
    }

    pub fn offline_included(&self, pc_id: &str, volume_id: &str) -> bool {
        self.offline_selection
            .iter()
            .any(|p| p.pc_id == pc_id && p.volume_id == volume_id)
    }

    pub fn set_override(&mut self, o: Override) {
        self.overrides.push(o);
    }
}

pub fn hostname() -> String {
    if let Ok(h) = std::env::var("CENSUS_HOSTNAME") {
        if !h.trim().is_empty() {
            return h.trim().to_string();
        }
    }
    #[cfg(unix)]
    {
        let mut buf = [0u8; 256];
        let rc = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) };
        if rc == 0 {
            if let Some(end) = buf.iter().position(|&b| b == 0) {
                let h = String::from_utf8_lossy(&buf[..end]).trim().to_string();
                if !h.is_empty() {
                    return h;
                }
            }
        }
    }
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(h) = std::env::var(key) {
            if !h.trim().is_empty() {
                return h.trim().to_string();
            }
        }
    }
    "mac".into()
}

fn home() -> PathBuf {
    for key in ["HOME", "USERPROFILE"] {
        if let Ok(h) = std::env::var(key) {
            if !h.trim().is_empty() {
                return PathBuf::from(h);
            }
        }
    }
    std::env::temp_dir()
}

/// En macOS, `~/Library/Application Support/Census`. En otros sistemas,
/// equivalente XDG para poder correr los tests en Windows.
#[allow(unreachable_code)]
pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CENSUS_DATA_DIR") {
        return PathBuf::from(d);
    }
    #[cfg(target_os = "macos")]
    {
        return home().join("Library/Application Support/Census");
    }
    #[cfg(windows)]
    {
        if let Ok(d) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(d).join("Census");
        }
    }
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        if !d.trim().is_empty() {
            return PathBuf::from(d).join("census");
        }
    }
    home().join(".local/share/census")
}

#[allow(unreachable_code)]
pub fn config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CENSUS_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    #[cfg(target_os = "macos")]
    {
        return home().join("Library/Application Support/Census");
    }
    #[cfg(windows)]
    {
        if let Ok(d) = std::env::var("APPDATA") {
            return PathBuf::from(d).join("Census");
        }
    }
    if let Ok(d) = std::env::var("XDG_CONFIG_HOME") {
        if !d.trim().is_empty() {
            return PathBuf::from(d).join("census");
        }
    }
    home().join(".config/census")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn index_path() -> PathBuf {
    data_dir().join("index.cnx")
}

pub fn wcat_path() -> PathBuf {
    data_dir().join("index.wcat")
}

pub fn manifest_path() -> PathBuf {
    data_dir().join("manifest.json")
}

pub fn mirrors_dir() -> PathBuf {
    data_dir().join("mirrors")
}

pub fn mirror_path(peer: &str, volume_id: &str, version: &str) -> PathBuf {
    mirrors_dir()
        .join(sanitize(peer))
        .join(sanitize(volume_id))
        .join(format!("{}.cnx", sanitize(version)))
}

pub fn registry_path() -> PathBuf {
    mirrors_dir().join("index.json")
}

pub fn selection_path() -> PathBuf {
    data_dir().join("atlas").join("selection.json")
}

pub fn overrides_path() -> PathBuf {
    data_dir().join("atlas").join("overrides.json")
}

pub fn pc_id_path() -> PathBuf {
    data_dir().join("pc_id")
}

pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

pub fn ensure_dirs() -> Result<(), String> {
    for d in [
        data_dir(),
        config_dir(),
        mirrors_dir(),
        data_dir().join("atlas"),
    ] {
        fs::create_dir_all(&d).map_err(|e| format!("crear {}: {e}", d.display()))?;
    }
    Ok(())
}

/// Escritura atómica: tmp en el mismo directorio y `rename`.
pub fn write_json<T: Serialize>(path: &std::path::Path, value: &T) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("crear {}: {e}", dir.display()))?;
    }
    let text = serde_json::to_string_pretty(value)
        .map_err(|e| format!("serializar {}: {e}", path.display()))?;
    let mut tmp = path.to_path_buf();
    tmp.set_extension("json.tmp");
    {
        let mut f = fs::File::create(&tmp).map_err(|e| format!("crear {}: {e}", tmp.display()))?;
        f.write_all(text.as_bytes())
            .map_err(|e| format!("escribir {}: {e}", tmp.display()))?;
        f.write_all(b"\n").ok();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path).map_err(|e| format!("renombrar {}: {e}", path.display()))
}

/// `pc_id` estable y persistente. No usa la MAC ni el nombre: un hash de
/// hardware id + un nonce guardado, para que no cambie al renombrar la Mac.
pub fn pc_id() -> String {
    if let Ok(v) = std::env::var("CENSUS_PC_ID") {
        if !v.trim().is_empty() {
            return v.trim().to_string();
        }
    }
    let path = pc_id_path();
    if let Ok(s) = fs::read_to_string(&path) {
        let s = s.trim().to_string();
        if !s.is_empty() {
            return s;
        }
    }
    #[allow(unused_mut)]
    let mut seed = format!(
        "{}:{}:{}",
        hostname(),
        std::process::id(),
        crate::time::now_ns()
    );
    #[cfg(target_os = "macos")]
    {
        if let Some(uuid) = crate::volumes::platform_uuid() {
            seed.push(':');
            seed.push_str(&uuid);
        }
    }
    let h1 = crate::types::fnv1a64(&[seed.as_bytes()]);
    let h2 = crate::types::fnv1a64(&[seed.as_bytes(), b":census-mac"]);
    let id = format!("{h1:016x}{h2:016x}");
    let _ = fs::create_dir_all(data_dir());
    let _ = fs::write(&path, format!("{id}\n"));
    id
}

fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    #[allow(unused_mut)]
    let mut ok = false;
    #[cfg(unix)]
    {
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            use std::io::Read;
            ok = f.read_exact(&mut bytes).is_ok();
        }
    }
    if !ok {
        let mut seed = crate::time::now_ns() as u64
            ^ ((std::process::id() as u64) << 32)
            ^ (&bytes as *const _ as usize as u64);
        for b in &mut bytes {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *b = seed as u8;
        }
    }
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_32_hex() {
        let t = generate_token();
        assert_eq!(t.len(), 32);
        assert!(t.chars().all(|c| c.is_ascii_hexdigit()));
        assert_ne!(t, generate_token());
    }

    #[test]
    fn sanitize_ids() {
        assert_eq!(sanitize("0123456789abcdef"), "0123456789abcdef");
        assert_eq!(sanitize("a/b c"), "a_b_c");
    }

    #[test]
    fn manual_override_wins() {
        let mut c = Config {
            volumes_enabled: vec!["vol1".into()],
            ..Default::default()
        };
        assert!(c.volume_enabled("vol1", None));
        assert!(!c.volume_enabled("vol2", None));
        c.set_override(Override {
            target: "vol2".into(),
            action: OverrideAction::Include,
            other: String::new(),
            motivo: "prueba".into(),
            fecha: "2026-11-22".into(),
        });
        assert!(c.volume_enabled("vol2", None));
    }

    #[test]
    fn offline_is_opt_in() {
        let mut c = Config::default();
        assert!(!c.offline_included("pc", "vol"));
        c.offline_selection.push(OfflinePick {
            pc_id: "pc".into(),
            volume_id: "vol".into(),
        });
        assert!(c.offline_included("pc", "vol"));
    }
}
