//! Configuracion, rutas XDG y token de comparticion.
//!
//! Todo se puede redirigir con `CENSUS_DATA_DIR` y `CENSUS_CONFIG_DIR`, que es
//! lo que usan los tests para no tocar el perfil real.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

fn d_port() -> u16 {
    8478
}
fn d_bind() -> String {
    "auto".into()
}
fn d_tailscale() -> String {
    "tailscale".into()
}
fn d_true() -> bool {
    true
}

fn default_excludes() -> Vec<String> {
    [
        "/proc",
        "/sys",
        "/dev",
        "/run",
        "/var/lib/docker/overlay2",
        "/var/lib/containers",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    #[serde(default = "d_port")]
    pub port: u16,
    /// `auto`, `tailscale`, `local`, o una IP literal.
    #[serde(default = "d_bind")]
    pub bind: String,
    /// Si no esta vacio, la API exige `Authorization: Bearer <token>`.
    #[serde(default)]
    pub token: String,
    /// Etiqueta del entorno que se publica en el catalogo.
    #[serde(default)]
    pub env: String,
    /// Raices extra que se indexan ademas de los montajes detectados.
    #[serde(default)]
    pub roots: Vec<String>,
    #[serde(default = "default_excludes")]
    pub exclude: Vec<String>,
    /// 0 = un hilo por nucleo disponible.
    #[serde(default)]
    pub jobs: usize,
    /// Cuando dos PC tienen el mismo disco, preferir la copia local si esta
    /// empatada en version.
    #[serde(default)]
    pub prefer_local: bool,
    /// Comando de Tailscale (por si esta en otra ruta).
    #[serde(default = "d_tailscale")]
    pub tailscale: String,
    /// Escuchar cambios y reindexar en caliente al correr `serve`.
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
                eprintln!("aviso: {} no es JSON valido ({e}); se usan valores por defecto", path.display());
                Config::default()
            }),
            Err(_) => Config::default(),
        }
    }

    /// Guarda la configuracion. Crea el token si falta.
    pub fn save(&mut self) -> Result<(), String> {
        if self.token.trim().is_empty() {
            self.token = generate_token();
        }
        let path = config_path();
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir).map_err(|e| format!("crear {}: {e}", dir.display()))?;
        }
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let mut tmp = path.clone();
        tmp.set_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp).map_err(|e| format!("crear {}: {e}", tmp.display()))?;
            f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
            f.write_all(b"\n").ok();
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
        }
        fs::rename(&tmp, &path).map_err(|e| format!("renombrar {}: {e}", path.display()))?;
        Ok(())
    }

    /// Token existente o uno nuevo, sin escribir nada si ya habia.
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
}

pub fn hostname() -> String {
    if let Ok(h) = std::env::var("CENSUS_HOSTNAME") {
        if !h.trim().is_empty() {
            return h.trim().to_string();
        }
    }
    if let Ok(h) = fs::read_to_string("/etc/hostname") {
        let h = h.trim();
        if !h.is_empty() {
            return h.to_string();
        }
    }
    for key in ["HOSTNAME", "COMPUTERNAME"] {
        if let Ok(h) = std::env::var(key) {
            if !h.trim().is_empty() {
                return h.trim().to_string();
            }
        }
    }
    "linux".into()
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

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CENSUS_DATA_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(d) = std::env::var("XDG_DATA_HOME") {
        if !d.trim().is_empty() {
            return PathBuf::from(d).join("census");
        }
    }
    #[cfg(windows)]
    {
        if let Ok(d) = std::env::var("LOCALAPPDATA") {
            return PathBuf::from(d).join("Census");
        }
    }
    home().join(".local/share/census")
}

pub fn config_dir() -> PathBuf {
    if let Ok(d) = std::env::var("CENSUS_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    if let Ok(d) = std::env::var("XDG_CONFIG_HOME") {
        if !d.trim().is_empty() {
            return PathBuf::from(d).join("census");
        }
    }
    #[cfg(windows)]
    {
        if let Ok(d) = std::env::var("APPDATA") {
            return PathBuf::from(d).join("Census");
        }
    }
    home().join(".config/census")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.json")
}

pub fn index_path() -> PathBuf {
    data_dir().join("index.cns")
}

pub fn mirrors_dir() -> PathBuf {
    data_dir().join("mirrors")
}

pub fn mirror_path(peer: &str) -> PathBuf {
    mirrors_dir().join(format!("{}.cns", sanitize(peer)))
}

pub fn registry_path() -> PathBuf {
    mirrors_dir().join("index.json")
}

pub fn cache_path() -> PathBuf {
    data_dir().join("cache.cns")
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
    for d in [data_dir(), config_dir(), mirrors_dir()] {
        fs::create_dir_all(&d).map_err(|e| format!("crear {}: {e}", d.display()))?;
    }
    Ok(())
}

fn generate_token() -> String {
    let mut bytes = [0u8; 16];
    let mut ok = false;
    #[cfg(unix)]
    {
        if let Ok(mut f) = fs::File::open("/dev/urandom") {
            use std::io::Read;
            ok = f.read_exact(&mut bytes).is_ok();
        }
    }
    if !ok {
        // Fallback sin dependencias: tiempo + pid + direccion del stack.
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
    fn sanitize_peer_names() {
        assert_eq!(sanitize("nb-lucio.tailnet.ts.net."), "nb-lucio.tailnet.ts.net.");
        assert_eq!(sanitize("a/b c"), "a_b_c");
    }
}
