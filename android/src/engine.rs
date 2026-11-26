//! Estado del indice en el telefono: indice vigente + snapshot `.cns`.
//!
//! Las operaciones que reescriben (`full`, `sweep`, `dirty`) se serializan con
//! un mutex; las consultas leen un `Arc<Index>` y nunca esperan a un reescaneo.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use censusindex::ai::{AiConfig, Endpoint};
use censusindex::index::{Builder, Index};
use censusindex::query::{self, Sort, Spec};
use censusindex::snapshot;
use censusindex::volpolicy::{Decision, Known, Policy, VolPolicy};
use censusindex::volumes::Mount;
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
    policy: Mutex<VolPolicy>,
    /// Volumenes a los que el usuario dijo "si, esta vez".
    allow_once: Mutex<std::collections::HashSet<String>>,
    /// Extraibles reconectados: se recorren completos, sin poda por mtime.
    reconnected: Mutex<std::collections::HashSet<String>>,
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
            policy: Mutex::new(VolPolicy::load(&dir.join("volumes.json"))),
            allow_once: Mutex::default(),
            reconnected: Mutex::default(),
        }
    }

    /// Nombre efectivo del dispositivo en el catalogo.
    pub fn device_name(&self) -> String {
        censusindex::devname::effective(&self.custom_name.read().expect("name"), &self.os_name)
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
            censusindex::devname::validate(raw)?
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
        v["groq_url"] = json!(censusindex::ai::GROQ_URL);
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
        Ok(json!({"models": censusindex::ai::models_of(&ep)?}))
    }

    /// Pregunta en lenguaje natural. Bloquea (red): llamar fuera del hilo de UI.
    pub fn ask(&self, question: &str, spanish: bool) -> Result<Value, String> {
        let cfg = AiConfig::load(&self.ai_path());
        let a = censusindex::ai::ask(&cfg, question, &self.current(), spanish, &|_, _| {})?;
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
        let host = censusindex::index::HostInfo {
            hostname: self.device_name(),
            os: "android".into(),
            arch: std::env::consts::ARCH.into(),
            version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        };
        let full_volumes = self
            .reconnected
            .lock()
            .expect("re")
            .iter()
            .cloned()
            .collect();
        Options {
            full_volumes,
            excludes,
            pc_id: self.pc_id.clone(),
            host,
        }
    }

    fn run(&self, mode: Mode) -> Result<Value, String> {
        let _guard = self.work.lock().expect("work");
        let old = self.current();
        let mut mounts = self.eligible(&old);
        if mounts.is_empty() && self.mounts.lock().expect("mounts").is_empty() {
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
        self.reconnected.lock().expect("re").clear();
        let out = report_json(label, &rep, &ix, bytes);
        *self.ix.write().expect("ix") = Arc::new(ix);
        Ok(out)
    }

    fn policy_path(&self) -> PathBuf {
        self.dir.join("volumes.json")
    }

    fn is_indexed(ix: &Index, volume_id: &str) -> bool {
        ix.roots.iter().any(|r| r.volume_id == volume_id)
    }

    fn decide(&self, m: &Mount, ix: &Index) -> Decision {
        if self.allow_once.lock().expect("once").contains(&m.volume_id) {
            return Decision::Index;
        }
        self.policy.lock().expect("policy").decide(
            &m.volume_id,
            m.removable,
            m.is_network(),
            Self::is_indexed(ix, &m.volume_id),
        )
    }

    /// Volumenes montados que hay que mantener al dia segun la politica.
    /// Los que quedan afuera conservan su ultima copia (ver `delta::rescan`).
    fn eligible(&self, ix: &Index) -> Vec<Mount> {
        let all = self.mounts.lock().expect("mounts").clone();
        all.into_iter()
            .filter(|m| self.decide(m, ix) == Decision::Index)
            .collect()
    }

    /// Estado de todos los volumenes y que hacer con cada uno. Kotlin lo llama
    /// cuando el sistema avisa un cambio de volumenes (pendrive conectado).
    /// `ask`: volumenes por los que hay que preguntar **ahora**.
    /// `to_index`: volumenes que deben indexarse y todavia no estan.
    pub fn volumes_update(&self, volumes_json: &str) -> Result<Value, String> {
        let mounts = volumes::from_json(&self.pc_id, volumes_json)?;
        let ix = self.current();
        let before: std::collections::HashSet<String> = self
            .mounts
            .lock()
            .expect("mounts")
            .iter()
            .map(|m| m.volume_id.clone())
            .collect();
        *self.mounts.lock().expect("mounts") = mounts.clone();
        for m in &mounts {
            if m.removable && !before.contains(&m.volume_id) && Self::is_indexed(&ix, &m.volume_id)
            {
                self.reconnected
                    .lock()
                    .expect("re")
                    .insert(m.volume_id.clone());
            }
        }
        let now = censusindex::time::secs(censusindex::time::now_ns());
        let (mut rows, mut ask, mut to_index) = (Vec::new(), Vec::new(), Vec::new());
        {
            let mut pol = self.policy.lock().expect("policy");
            let present: Vec<String> = mounts.iter().map(|m| m.volume_id.clone()).collect();
            pol.retain_connected(&present);
            for m in &mounts {
                let indexed = Self::is_indexed(&ix, &m.volume_id);
                let once = self.allow_once.lock().expect("once").contains(&m.volume_id);
                let d = if once {
                    Decision::Index
                } else {
                    pol.decide(&m.volume_id, m.removable, m.is_network(), indexed)
                };
                let known = Known {
                    label: m.label.clone(),
                    path: m.path.clone(),
                    removable: m.removable,
                    last_seen: now,
                };
                if pol.note_seen(&m.volume_id, known, d) {
                    ask.push(m.volume_id.clone());
                }
                if d == Decision::Index && !indexed {
                    to_index.push(m.volume_id.clone());
                }
                rows.push(json!({
                    "volume_id": m.volume_id, "label": m.label, "path": m.path, "fs": m.fs,
                    "removable": m.removable, "total": m.total, "free": m.free,
                    "identity": m.identity_status, "indexed": indexed,
                    "policy": pol.effective(&m.volume_id), "decision": d,
                }));
            }
            let _ = pol.save(&self.policy_path());
        }
        Ok(json!({"volumes": rows, "ask": ask, "to_index": to_index}))
    }

    /// `volume_id` o `"*"` (global). `policy`: ask|always|never (o siempre/nunca/preguntar).
    pub fn set_policy(&self, volume_id: &str, policy: &str) -> Result<Value, String> {
        let p = Policy::parse(policy).ok_or_else(|| format!("politica desconocida: {policy}"))?;
        let mut pol = self.policy.lock().expect("policy");
        pol.set(volume_id, p);
        pol.save(&self.policy_path())?;
        Ok(serde_json::to_value(&*pol).unwrap_or(Value::Null))
    }

    pub fn policy_get(&self) -> Value {
        serde_json::to_value(&*self.policy.lock().expect("policy")).unwrap_or(Value::Null)
    }

    /// "Si, esta vez": indexa el volumen y, ya indexado, se mantiene al dia.
    pub fn index_once(&self, volume_id: &str) -> Result<Value, String> {
        self.allow_once
            .lock()
            .expect("once")
            .insert(volume_id.to_string());
        self.run(Mode::Dirty(Vec::new()))
    }

    /// Indexa los volumenes que la politica aprobo y todavia no estan (el resto
    /// se copia del indice anterior sin tocar el disco).
    pub fn index_pending(&self) -> Result<Value, String> {
        self.run(Mode::Dirty(Vec::new()))
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
        censusindex::http::serve_live(
            &move || me.current(),
            &censusindex::http::Options {
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
        let base = std::env::temp_dir().join(format!("censusindex-eng-{tag}-{}", std::process::id()));
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
        let d = std::env::temp_dir().join(format!("censusindex-name-{tag}-{}", std::process::id()));
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
        let cfg = json!({"endpoints":[{"name":"groq","base_url":censusindex::ai::GROQ_URL,
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

#[cfg(test)]
mod policy_tests {
    use super::*;
    use std::fs;

    fn setup(tag: &str) -> (Engine, PathBuf, PathBuf, PathBuf) {
        let base = std::env::temp_dir().join(format!("censusindex-pol-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let (int, usb) = (base.join("internal"), base.join("usb"));
        fs::create_dir_all(int.join("Download")).unwrap();
        fs::create_dir_all(usb.join("fotos")).unwrap();
        fs::write(int.join("Download/a.pdf"), b"1").unwrap();
        fs::write(usb.join("fotos/vacaciones.jpg"), b"1").unwrap();
        (
            Engine::open(&base.join("files"), "pc", "Pixel"),
            base,
            int,
            usb,
        )
    }

    fn vols(int: &Path, usb: Option<&Path>) -> String {
        let mut v =
            vec![json!({"path": int.to_string_lossy(), "primary": true, "total": 100, "free": 1})];
        if let Some(u) = usb {
            v.push(
                json!({"path": u.to_string_lossy(), "uuid": "AAAA-1111", "removable": true,
                          "label": "PENDRIVE", "total": 64}),
            );
        }
        Value::Array(v).to_string()
    }

    #[test]
    fn ask_policy_waits_for_the_user_then_indexes_the_whole_stick() {
        let (e, base, int, usb) = setup("ask");
        e.volumes_update(&vols(&int, None)).unwrap();
        e.full().unwrap();
        let r = e.volumes_update(&vols(&int, Some(&usb))).unwrap();
        assert_eq!(r["ask"].as_array().unwrap().len(), 1, "{r}");
        assert_eq!(r["volumes"][1]["decision"], "ask");
        e.sweep().unwrap();
        assert_eq!(
            e.search("vacaciones", 5)["total"],
            0,
            "sin respuesta no se indexa"
        );
        // Segunda notificacion del sistema: no se vuelve a preguntar.
        let again = e.volumes_update(&vols(&int, Some(&usb))).unwrap();
        assert!(again["ask"].as_array().unwrap().is_empty());
        // Se desconecta y vuelve: se pregunta de nuevo.
        e.volumes_update(&vols(&int, None)).unwrap();
        let back = e.volumes_update(&vols(&int, Some(&usb))).unwrap();
        assert_eq!(back["ask"].as_array().unwrap().len(), 1);
        let vid = r["ask"][0].as_str().unwrap().to_string();
        let rep = e.index_once(&vid).unwrap();
        assert_eq!(e.search("vacaciones", 5)["total"], 1);
        assert!(
            rep["dirs_listed"].as_u64().unwrap() <= 3,
            "solo el pendrive se recorre: {rep}"
        );
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn always_indexes_on_connect_and_never_skips_but_keeps_the_old_copy() {
        let (e, base, int, usb) = setup("always");
        e.volumes_update(&vols(&int, None)).unwrap();
        e.full().unwrap();
        e.set_policy("*", "siempre").unwrap();
        let r = e.volumes_update(&vols(&int, Some(&usb))).unwrap();
        assert_eq!(r["to_index"].as_array().unwrap().len(), 1);
        e.index_pending().unwrap();
        assert_eq!(e.search("vacaciones", 5)["total"], 1);
        // Pasa a "nunca": no se actualiza mas, pero lo ya indexado no se pierde.
        let vid = r["to_index"][0].as_str().unwrap();
        e.set_policy(vid, "never").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        fs::write(usb.join("fotos/nueva.jpg"), b"1").unwrap();
        e.volumes_update(&vols(&int, Some(&usb))).unwrap();
        e.sweep().unwrap();
        assert_eq!(e.search("vacaciones", 5)["total"], 1, "copia conservada");
        assert_eq!(e.search("nueva.jpg", 5)["total"], 0, "congelado");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn reconnected_stick_is_walked_in_full_not_pruned_by_mtime() {
        let (e, base, int, usb) = setup("recon");
        e.set_policy("*", "always").unwrap();
        e.volumes_update(&vols(&int, Some(&usb))).unwrap();
        e.full().unwrap();
        let quiet = e.sweep().unwrap();
        assert_eq!(quiet["dirs_listed"], 0, "sin reconexion se poda: {quiet}");
        e.volumes_update(&vols(&int, None)).unwrap(); // se desconecta
        e.volumes_update(&vols(&int, Some(&usb))).unwrap(); // vuelve
        let rep = e.sweep().unwrap();
        assert!(
            rep["dirs_listed"].as_u64().unwrap() >= 2,
            "recorrido completo del pendrive: {rep}"
        );
        assert_eq!(e.sweep().unwrap()["dirs_listed"], 0, "la marca se consume");
        let _ = fs::remove_dir_all(base);
    }

    #[test]
    fn never_from_the_start_never_touches_the_stick() {
        let (e, base, int, usb) = setup("never");
        e.volumes_update(&vols(&int, None)).unwrap();
        e.full().unwrap();
        e.set_policy("*", "never").unwrap();
        let r = e.volumes_update(&vols(&int, Some(&usb))).unwrap();
        assert!(
            r["ask"].as_array().unwrap().is_empty() && r["to_index"].as_array().unwrap().is_empty()
        );
        e.sweep().unwrap();
        assert_eq!(e.search("vacaciones", 5)["total"], 0);
        // La politica sobrevive a un reinicio.
        let again = Engine::open(&base.join("files"), "pc", "Pixel");
        assert_eq!(again.policy_get()["default"], "never");
        assert!(e.set_policy("*", "quizas").is_err());
        let _ = fs::remove_dir_all(base);
    }
}
