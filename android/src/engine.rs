//! Estado del indice en el telefono: indice vigente + snapshot `.cns`.
//!
//! Las operaciones que reescriben (`full`, `sweep`, `dirty`) se serializan con
//! un mutex; las consultas leen un `Arc<Index>` y nunca esperan a un reescaneo.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use census::ai::{AiConfig, Endpoint};
use census::index::{Builder, Index};
use census::query::{self, Sort, Spec};
use census::snapshot;
use census::volumes::Mount;
use serde_json::{json, Value};

use crate::delta::{self, Mode, Options, Report};
use crate::volumes;

/// Carpetas que Android no deja leer o que solo son ruido: se excluyen por
/// volumen. `Android/data` y `Android/obb` son `EACCES` desde Android 11.
const DEFAULT_EXCLUDES: &[&str] = &[
    "Android/data",
    "Android/obb",
    ".thumbnails",
    ".Trash",
    ".trashed",
];

pub struct Engine {
    dir: PathBuf,
    pc_id: String,
    /// Nombre que da el sistema operativo (por defecto).
    os_name: String,
    /// Nombre propio sin espacios; vacio = usar el del sistema.
    custom_name: RwLock<String>,
    ix: RwLock<Arc<Index>>,
    mounts: Mutex<Vec<Mount>>,
    work: Mutex<()>,
}

impl Engine {
    /// `dir` es `Context.filesDir`; `pc_id` sale de `ANDROID_ID` hasheado (lo
    /// pasa Kotlin) y `device` es `Build.MODEL`.
    pub fn open(dir: &Path, pc_id: &str, device: &str) -> Engine {
        let _ = std::fs::create_dir_all(dir);
        let ix = snapshot::load(&dir.join("index.cns")).unwrap_or_else(|_| empty(pc_id));
        Engine {
            dir: dir.to_path_buf(),
            pc_id: pc_id.to_string(),
            os_name: device.to_string(),
            custom_name: RwLock::new(
                std::fs::read_to_string(dir.join("device_name.txt"))
                    .map(|t| t.trim().to_string())
                    .unwrap_or_default(),
            ),
            ix: RwLock::new(Arc::new(ix)),
            mounts: Mutex::new(Vec::new()),
            work: Mutex::new(()),
        }
    }

    /// Nombre efectivo del dispositivo en el catalogo.
    pub fn device_name(&self) -> String {
        census::devname::effective(&self.custom_name.read().expect("name"), &self.os_name)
    }

    pub fn device_info(&self) -> Value {
        let custom = self.custom_name.read().expect("name").clone();
        json!({"name": self.device_name(), "os_name": self.os_name, "custom": custom})
    }

    /// `""` vuelve al nombre del sistema; si no, debe ser valido (sin espacios).
    /// Se refleja en el catalogo en el proximo reescaneo.
    pub fn set_device_name(&self, raw: &str) -> Result<Value, String> {
        let name = if raw.trim().is_empty() {
            String::new()
        } else {
            census::devname::validate(raw)?
        };
        let path = self.dir.join("device_name.txt");
        if name.is_empty() {
            let _ = std::fs::remove_file(&path);
        } else {
            std::fs::write(&path, &name).map_err(|e| e.to_string())?;
        }
        *self.custom_name.write().expect("name") = name;
        Ok(self.device_info())
    }

    fn ai_path(&self) -> PathBuf {
        self.dir.join("ai.json")
    }

    /// Configuracion de IA con la clave enmascarada + presets disponibles.
    pub fn ai_get(&self) -> Value {
        let mut v = AiConfig::load(&self.ai_path()).masked();
        v["presets"] = json!(["groq", "ollama", "lmstudio", "llamacpp", "vllm", "custom"]);
        v["groq_url"] = json!(census::ai::GROQ_URL);
        v
    }

    /// `{"endpoints":[...]}`; una clave con `…` conserva la guardada.
    pub fn ai_set(&self, text: &str) -> Result<Value, String> {
        let v: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let eps: Vec<Endpoint> = serde_json::from_value(v["endpoints"].clone())
            .map_err(|e| format!("endpoints: {e}"))?;
        let mut cfg = AiConfig::load(&self.ai_path());
        cfg.replace(eps)?;
        cfg.save(&self.ai_path())?;
        Ok(cfg.masked())
    }

    /// Modelos que ofrece un endpoint (para el selector). Acepta el JSON de un
    /// endpoint aun sin guardar.
    pub fn ai_models(&self, endpoint_json: &str) -> Result<Value, String> {
        let mut ep: Endpoint = serde_json::from_str(endpoint_json).map_err(|e| e.to_string())?;
        if ep.api_key.contains('…') {
            ep.api_key = AiConfig::load(&self.ai_path())
                .endpoints
                .iter()
                .find(|o| o.name == ep.name)
                .map(|o| o.api_key.clone())
                .unwrap_or_default();
        }
        ep.validate()?;
        Ok(json!({"models": census::ai::models_of(&ep)?}))
    }

    /// Pregunta en lenguaje natural. Bloquea (red): llamar fuera del hilo de UI.
    pub fn ask(&self, question: &str, spanish: bool) -> Result<Value, String> {
        let cfg = AiConfig::load(&self.ai_path());
        let a = census::ai::ask(&cfg, question, &self.current(), spanish, &|_, _| {})?;
        Ok(json!({"endpoint": a.endpoint, "model": a.model, "text": a.text}))
    }

    pub fn current(&self) -> Arc<Index> {
        self.ix.read().expect("ix").clone()
    }

    pub fn snapshot_path(&self) -> PathBuf {
        self.dir.join("index.cns")
    }

    pub fn set_volumes(&self, json_text: &str) -> Result<usize, String> {
        let v = volumes::from_json(&self.pc_id, json_text)?;
        let n = v.len();
        *self.mounts.lock().expect("mounts") = v;
        Ok(n)
    }

    fn options(&self, mounts: &[Mount]) -> Options {
        let mut excludes = Vec::new();
        for m in mounts {
            for rel in DEFAULT_EXCLUDES {
                excludes.push(Path::new(&m.path).join(rel));
            }
        }
        let host = census::index::HostInfo {
            hostname: self.device_name(),
            os: "android".into(),
            arch: std::env::consts::ARCH.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        };
        Options {
            excludes,
            pc_id: self.pc_id.clone(),
            host,
        }
    }

    fn run(&self, mode: Mode) -> Result<Value, String> {
        let _guard = self.work.lock().expect("work");
        let mut mounts = self.mounts.lock().expect("mounts").clone();
        let old = self.current();
        if mounts.is_empty() {
            mounts = mounts_from_index(&old);
        }
        if mounts.is_empty() {
            return Err("sin volumenes: llamar a set_volumes primero".into());
        }
        // Sin indice previo no hay nada que reutilizar: todo es un Full.
        let (mode, prev) = if old.is_empty() {
            (Mode::Full, None)
        } else {
            (mode, Some(old.as_ref()))
        };
        let label = match &mode {
            Mode::Full => "full",
            Mode::Sweep => "sweep",
            Mode::Dirty(_) => "dirty",
        };
        let (ix, rep) = delta::rescan(prev, &mounts, &mode, &self.options(&mounts))?;
        let bytes = snapshot::save(&ix, &self.snapshot_path())?;
        let out = report_json(label, &rep, &ix, bytes);
        *self.ix.write().expect("ix") = Arc::new(ix);
        Ok(out)
    }

    pub fn full(&self) -> Result<Value, String> {
        self.run(Mode::Full)
    }

    pub fn sweep(&self) -> Result<Value, String> {
        self.run(Mode::Sweep)
    }

    pub fn dirty(&self, dirs: Vec<String>) -> Result<Value, String> {
        if dirs.is_empty() {
            return Ok(json!({"mode": "dirty", "dirs": 0, "noop": true}));
        }
        self.run(Mode::Dirty(dirs))
    }

    pub fn search(&self, raw: &str, limit: usize) -> Value {
        let ix = self.current();
        let mut spec = Spec::new(raw);
        spec.limit = if limit == 0 { 200 } else { limit };
        if raw.contains("sort:") {
            for t in raw.split_whitespace() {
                if let Some(s) = t.strip_prefix("sort:").and_then(Sort::parse) {
                    spec.sort = s;
                }
            }
        }
        let (total, hits) = query::search(&ix, &spec);
        let results: Vec<Value> = hits
            .iter()
            .map(|&id| {
                let h = query::hit(&ix, id);
                json!({
                    "path": h.path, "name": h.name, "size_bytes": h.size,
                    "alloc_bytes": h.alloc, "mtime_ns": h.mtime_ns,
                    "kind": if ix.is_dir(id) { "dir" } else { "file" },
                    "volume_id": h.volume_id,
                    "source": {"pc_id": ix.pc_id, "state": "online"},
                })
            })
            .collect();
        json!({"query": raw, "total": total, "returned": results.len(), "results": results})
    }

    pub fn stats(&self) -> Value {
        let ix = self.current();
        let dirs = (0..ix.len() as u32).filter(|&i| ix.is_dir(i)).count();
        json!({
            "pc_id": ix.pc_id, "built_at": ix.built_at, "generation": ix.generation,
            "marca": ix.marca(), "volumes": ix.roots.len(), "nodes": ix.len(),
            "dirs": dirs, "files": ix.len().saturating_sub(dirs),
        })
    }

    /// API HTTP v1 (la del contrato MINIMO) sobre el indice vigente. Bloquea;
    /// Kotlin la corre en un hilo. Con `lan` exige token.
    pub fn serve(self: &Arc<Self>, port: u16, lan: bool, token: &str) -> Result<(), String> {
        let me = self.clone();
        census::http::serve_live(
            &move || me.current(),
            &census::http::Options {
                port,
                lan,
                token: token.to_string(),
                name: self.device_name(),
            },
        )
    }
}

fn empty(pc_id: &str) -> Index {
    let mut b = Builder::with_capacity(0);
    b.pc_id = pc_id.to_string();
    b.finish()
}

/// Si Kotlin no volvio a declarar volumenes, se reutilizan las raices guardadas.
fn mounts_from_index(ix: &Index) -> Vec<Mount> {
    ix.roots
        .iter()
        .map(|r| Mount {
            path: r.path.clone(),
            volume_id: r.volume_id.clone(),
            fs_uuid: r.fs_uuid.clone(),
            fs: r.fs.clone(),
            label: r.label.clone(),
            source: r.source.clone(),
            kind: r.kind.clone(),
            total: r.total,
            free: r.free,
            identity_status: r.identity_status.clone(),
            federable: r.federable,
            ..Default::default()
        })
        .collect()
}

fn report_json(mode: &str, r: &Report, ix: &Index, snapshot_bytes: u64) -> Value {
    json!({
        "mode": mode, "ms": r.ms as u64, "nodes": r.nodes,
        "dirs_listed": r.dirs_listed, "dirs_statted": r.dirs_statted,
        "subtrees_copied": r.subtrees_copied, "nodes_copied": r.nodes_copied,
        "nodes_new": r.nodes_new, "offline_volumes": r.volumes_kept_offline,
        "errors": r.errors, "generation": ix.generation, "marca": ix.marca(),
        "snapshot_bytes": snapshot_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn sandbox(tag: &str) -> (PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("census-eng-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let data = base.join("files");
        let sd = base.join("storage");
        fs::create_dir_all(sd.join("Download")).unwrap();
        fs::create_dir_all(sd.join("Android/data/app")).unwrap();
        fs::write(sd.join("Download/informe.pdf"), b"pdf").unwrap();
        fs::write(sd.join("Android/data/app/x.db"), b"db").unwrap();
        (data, sd)
    }

    fn vols(sd: &Path) -> String {
        json!([{"path": sd.to_string_lossy(), "primary": true, "total": 1000, "free": 1}])
            .to_string()
    }

    #[test]
    fn full_then_search_then_reload_from_snapshot() {
        let (data, sd) = sandbox("full");
        let e = Engine::open(&data, "pc-test", "Pixel");
        e.set_volumes(&vols(&sd)).unwrap();
        let r = e.full().unwrap();
        assert_eq!(r["mode"], "full");
        let s = e.search("informe", 10);
        assert_eq!(s["total"], 1);
        assert_eq!(e.search("x.db", 10)["total"], 0, "Android/data excluido");
        // Reabrir: el snapshot .cns sobrevive y responde sin reescanear.
        let e2 = Engine::open(&data, "pc-test", "Pixel");
        assert_eq!(e2.search("informe", 10)["total"], 1);
        let _ = fs::remove_dir_all(data.parent().unwrap());
    }

    #[test]
    fn dirty_picks_up_a_new_download_and_search_sees_it() {
        let (data, sd) = sandbox("dirty");
        let e = Engine::open(&data, "pc-test", "Pixel");
        e.set_volumes(&vols(&sd)).unwrap();
        e.full().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(sd.join("Download/nuevo.apk"), b"apk").unwrap();
        assert_eq!(e.search("nuevo", 10)["total"], 0);
        let r = e
            .dirty(vec![sd.join("Download").to_string_lossy().into_owned()])
            .unwrap();
        assert_eq!(r["mode"], "dirty");
        assert_eq!(r["dirs_listed"], 1);
        assert_eq!(e.search("nuevo", 10)["total"], 1);
        let _ = fs::remove_dir_all(data.parent().unwrap());
    }

    #[test]
    fn sweep_works_without_redeclaring_volumes_after_reopen() {
        let (data, sd) = sandbox("sweep");
        {
            let e = Engine::open(&data, "pc-test", "Pixel");
            e.set_volumes(&vols(&sd)).unwrap();
            e.full().unwrap();
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(sd.join("Download/otro.zip"), b"z").unwrap();
        let e = Engine::open(&data, "pc-test", "Pixel");
        let r = e.sweep().unwrap(); // mounts salen de las raices del snapshot
        assert_eq!(r["mode"], "sweep");
        assert_eq!(e.search("otro", 10)["total"], 1);
        let _ = fs::remove_dir_all(data.parent().unwrap());
    }

    #[test]
    fn no_volumes_is_an_error_not_a_panic() {
        let (data, _sd) = sandbox("novol");
        let e = Engine::open(&data, "pc-test", "Pixel");
        assert!(e.sweep().is_err());
        let _ = fs::remove_dir_all(data.parent().unwrap());
    }
}

#[cfg(test)]
mod name_tests {
    use super::*;

    fn eng(tag: &str) -> (Engine, PathBuf) {
        let d = std::env::temp_dir().join(format!("census-name-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        (Engine::open(&d, "pc", "Pixel 8 de Lucio"), d)
    }

    #[test]
    fn default_is_os_name_custom_must_have_no_spaces_and_persists() {
        let (e, d) = eng("a");
        assert_eq!(e.device_name(), "Pixel 8 de Lucio");
        assert!(e
            .set_device_name("Pixel 8")
            .unwrap_err()
            .contains("Pixel-8"));
        assert_eq!(
            e.device_name(),
            "Pixel 8 de Lucio",
            "un rechazo no cambia nada"
        );
        assert_eq!(e.set_device_name("tel-lucio").unwrap()["name"], "tel-lucio");
        let again = Engine::open(&d, "pc", "Pixel 8 de Lucio");
        assert_eq!(again.device_name(), "tel-lucio");
        assert_eq!(
            again.set_device_name("").unwrap()["name"],
            "Pixel 8 de Lucio"
        );
        let _ = std::fs::remove_dir_all(d);
    }

    #[test]
    fn ai_config_masks_key_and_keeps_it_on_resave() {
        let (e, d) = eng("b");
        let cfg = json!({"endpoints":[{"name":"groq","base_url":census::ai::GROQ_URL,
            "api_key":"gsk_SECRET123456"}]});
        let shown = e.ai_set(&cfg.to_string()).unwrap();
        assert!(!shown.to_string().contains("SECRET"));
        let back = json!({"endpoints": e.ai_get()["endpoints"].clone()});
        e.ai_set(&back.to_string()).unwrap();
        let on_disk = std::fs::read_to_string(d.join("ai.json")).unwrap();
        assert!(on_disk.contains("gsk_SECRET123456"));
        assert!(e
            .ai_set(r#"{"endpoints":[{"name":"x","base_url":"http://a/v1","api_key":"k"}]}"#)
            .is_err());
        let _ = std::fs::remove_dir_all(d);
    }
}
