//! IA en lenguaje natural sobre un endpoint compatible con OpenAI.
//!
//! Pensado para modelos gratuitos (Groq, OpenRouter `:free`, Gemini en modo
//! OpenAI, Cerebras...) pero sin atarse a ninguno: cualquier API paga o una IA
//! local (Ollama, LM Studio, llama.cpp, vLLM) sirve con tres campos:
//! `base_url`, `api_key` (vacia en local) y `model`.
//!
//! * `ModelMode::Auto`  — lista `GET {base}/models` y elige el mejor.
//! * `ModelMode::List`  — el usuario eligio uno de la lista; si falla, cae al resto.
//! * `ModelMode::Force` — usa el nombre tal cual, sin listar (APIs sin `/models`).
//! * Fallback entre modelos y entre endpoints ante 429/5xx/timeout/vacio.
//! * `tools=false` — para modelos sin llamadas a herramientas: el modelo solo
//!   traduce la pregunta a una consulta de Census y el resultado se arma aca.
//!
//! Solo lectura: las herramientas consultan el indice, no tocan archivos.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::index::Index;
use crate::query::{self, Sort, Spec};

pub const GROQ_URL: &str = "https://api.groq.com/openai/v1";
const MAX_STEPS: usize = 8;
const MAX_TOOL_CHARS: usize = 4_000;

/// Mejor primero. Los que el endpoint no liste se descartan; el resto se ordena
/// por parametros declarados en el nombre.
const PREFERRED: &[&str] = &[
    "openai/gpt-oss-120b",
    "moonshotai/kimi-k2-instruct-0905",
    "moonshotai/kimi-k2-instruct",
    "llama-3.3-70b-versatile",
    "qwen/qwen3-32b",
    "meta-llama/llama-4-maverick-17b-128e-instruct",
    "openai/gpt-oss-20b",
    "meta-llama/llama-4-scout-17b-16e-instruct",
    "llama-3.1-8b-instant",
];

const NOT_CHAT: &[&str] = &[
    "whisper",
    "tts",
    "guard",
    "playai",
    "orpheus",
    "embed",
    "distil",
    "compound",
    "moderation",
    "dall-e",
    "image",
    "rerank",
];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelMode {
    #[default]
    Auto,
    List,
    Force,
}

fn yes() -> bool {
    true
}
fn t90() -> u64 {
    90
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Endpoint {
    /// Etiqueta libre ("groq", "ollama-casa").
    pub name: String,
    /// Raiz de la API, sin `/chat/completions`: `https://api.groq.com/openai/v1`.
    pub base_url: String,
    /// Vacia en IA local. Se guarda en el archivo privado de la app.
    #[serde(default)]
    pub api_key: String,
    #[serde(default)]
    pub mode: ModelMode,
    /// Modelo elegido (`List`) o forzado (`Force`); opcional en `Auto`.
    #[serde(default)]
    pub model: String,
    /// Cadena explicita de respaldo (sobre todo para `Force`).
    #[serde(default)]
    pub fallback_models: Vec<String>,
    /// Si el modelo falla, probar otro.
    #[serde(default = "yes")]
    pub fallback: bool,
    /// El modelo soporta `tools`. `false` = modo traduccion a consulta.
    #[serde(default = "yes")]
    pub tools: bool,
    #[serde(default = "yes")]
    pub enabled: bool,
    /// IA local: permite `http://` y no exige clave.
    #[serde(default)]
    pub local: bool,
    #[serde(default = "t90")]
    pub timeout_s: u64,
}

impl Endpoint {
    pub fn groq(api_key: &str) -> Self {
        Self {
            name: "groq".into(),
            base_url: GROQ_URL.into(),
            api_key: api_key.into(),
            ..Self::blank()
        }
    }

    /// `kind`: `ollama`, `lmstudio`, `llamacpp`, `vllm`.
    pub fn local_preset(kind: &str) -> Option<Self> {
        let url = match kind {
            "ollama" => "http://127.0.0.1:11434/v1",
            "lmstudio" => "http://127.0.0.1:1234/v1",
            "llamacpp" => "http://127.0.0.1:8080/v1",
            "vllm" => "http://127.0.0.1:8000/v1",
            _ => return None,
        };
        Some(Self {
            name: kind.into(),
            base_url: url.into(),
            local: true,
            ..Self::blank()
        })
    }

    fn blank() -> Self {
        Self {
            name: String::new(),
            base_url: String::new(),
            api_key: String::new(),
            mode: ModelMode::Auto,
            model: String::new(),
            fallback_models: Vec::new(),
            fallback: true,
            tools: true,
            enabled: true,
            local: false,
            timeout_s: 90,
        }
    }

    /// Normaliza y valida. Devuelve la URL sin barra final.
    pub fn validate(&mut self) -> Result<(), String> {
        self.name = self.name.trim().to_string();
        if self.name.is_empty() {
            return Err("el endpoint necesita un nombre".into());
        }
        let url = self.base_url.trim().trim_end_matches('/').to_string();
        let https = url.starts_with("https://");
        if !https && !url.starts_with("http://") {
            return Err("base_url debe empezar con https:// o http://".into());
        }
        if !https && !self.local {
            return Err("http:// solo se admite en IA local (la clave viajaria sin cifrar)".into());
        }
        if url.ends_with("/chat/completions") {
            return Err("base_url es la raiz (…/v1), sin /chat/completions".into());
        }
        if !self.local && self.api_key.trim().is_empty() {
            return Err("falta la api_key (vacia solo en IA local)".into());
        }
        if self.mode == ModelMode::Force && self.model.trim().is_empty() {
            return Err("modo forzar: indica el nombre del modelo".into());
        }
        self.base_url = url;
        self.api_key = self.api_key.trim().to_string();
        self.model = self.model.trim().to_string();
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AiConfig {
    /// Se prueban en orden; el primero que conteste gana.
    #[serde(default)]
    pub endpoints: Vec<Endpoint>,
}

impl AiConfig {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
        }
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// Para mostrar en pantallas: la clave nunca sale completa.
    pub fn masked(&self) -> Value {
        let eps: Vec<Value> = self
            .endpoints
            .iter()
            .map(|e| {
                let mut v = serde_json::to_value(e).unwrap_or(Value::Null);
                v["api_key"] = Value::String(mask(&e.api_key));
                v
            })
            .collect();
        json!({ "endpoints": eps })
    }

    /// Reemplaza los endpoints. Una clave enmascarada (`gsk_…abcd`) conserva la
    /// que ya estaba guardada para ese nombre.
    pub fn replace(&mut self, mut incoming: Vec<Endpoint>) -> Result<(), String> {
        for e in &mut incoming {
            if e.api_key.contains('…') {
                e.api_key = self
                    .endpoints
                    .iter()
                    .find(|o| o.name == e.name)
                    .map(|o| o.api_key.clone())
                    .unwrap_or_default();
            }
            e.validate()?;
        }
        self.endpoints = incoming;
        Ok(())
    }
}

fn mask(key: &str) -> String {
    if key.len() <= 8 {
        return if key.is_empty() {
            String::new()
        } else {
            "…".into()
        };
    }
    format!("{}…{}", &key[..4], &key[key.len() - 4..])
}

// ------------------------------------------------------------- transporte

/// Acceso HTTP abstracto (se prueba contra un servidor falso).
pub trait Transport {
    fn get(&self, path: &str) -> Result<(u16, Value), String>;
    fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), String>;
}

#[cfg(any(feature = "tls", feature = "plain"))]
pub struct Http<'a>(pub &'a Endpoint);

#[cfg(any(feature = "tls", feature = "plain"))]
impl Http<'_> {
    fn agent(&self) -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(self.0.timeout_s.max(5)))
            .build()
    }

    fn with_auth(&self, req: ureq::Request) -> ureq::Request {
        if self.0.api_key.is_empty() {
            req
        } else {
            req.set("Authorization", &format!("Bearer {}", self.0.api_key))
        }
    }

    fn reply(r: Result<ureq::Response, ureq::Error>) -> Result<(u16, Value), String> {
        match r {
            Ok(resp) => {
                let code = resp.status();
                Ok((code, resp.into_json().unwrap_or(Value::Null)))
            }
            Err(ureq::Error::Status(code, resp)) => {
                Ok((code, resp.into_json().unwrap_or(Value::Null)))
            }
            Err(e) => Err(e.to_string()),
        }
    }
}

#[cfg(any(feature = "tls", feature = "plain"))]
impl Transport for Http<'_> {
    fn get(&self, path: &str) -> Result<(u16, Value), String> {
        Self::reply(
            self.with_auth(self.agent().get(&format!("{}{path}", self.0.base_url)))
                .call(),
        )
    }

    fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), String> {
        Self::reply(
            self.with_auth(self.agent().post(&format!("{}{path}", self.0.base_url)))
                .send_json(body.clone()),
        )
    }
}

// ----------------------------------------------------------------- modelos

fn params_b(id: &str) -> f32 {
    id.split(['-', '/', '_', ':'])
        .filter_map(|p| {
            p.strip_suffix(['b', 'B'])
                .and_then(|n| n.parse::<f32>().ok())
        })
        .fold(0.0, f32::max)
}

/// Modelos de chat, mejor primero.
pub fn rank_models(available: &[String]) -> Vec<String> {
    let chat: Vec<&String> = available
        .iter()
        .filter(|m| !NOT_CHAT.iter().any(|x| m.to_lowercase().contains(x)))
        .collect();
    let mut out: Vec<String> = PREFERRED
        .iter()
        .filter(|p| chat.iter().any(|m| m == p))
        .map(|s| s.to_string())
        .collect();
    let mut rest: Vec<&String> = chat.into_iter().filter(|m| !out.contains(m)).collect();
    rest.sort_by(|a, b| params_b(b).total_cmp(&params_b(a)));
    out.extend(rest.into_iter().cloned());
    out
}

#[derive(Debug)]
pub enum Fail {
    /// Credenciales: no sirve otro modelo del mismo endpoint.
    Auth(String),
    /// Probar el siguiente modelo.
    Model(String),
}

fn classify(code: u16, v: &Value) -> Fail {
    let msg = v
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    match code {
        401 | 403 => Fail::Auth(format!("HTTP {code}: credenciales rechazadas {msg}")),
        _ => Fail::Model(format!("HTTP {code}: {msg}")),
    }
}

/// Modelos disponibles del endpoint (ya ordenados). Sirve para el selector.
pub fn list_models(t: &dyn Transport) -> Result<Vec<String>, String> {
    let (code, v) = t.get("/models")?;
    if code != 200 {
        return Err(match classify(code, &v) {
            Fail::Auth(m) | Fail::Model(m) => m,
        });
    }
    let ids: Vec<String> = v
        .get("data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|m| m.get("active").and_then(Value::as_bool).unwrap_or(true))
        .filter_map(|m| m.get("id").and_then(Value::as_str).map(String::from))
        .collect();
    Ok(rank_models(&ids))
}

/// Modelos que se probaran, en orden, segun el modo.
pub fn plan(t: &dyn Transport, ep: &Endpoint) -> Result<Vec<String>, String> {
    let mut order: Vec<String> = Vec::new();
    match ep.mode {
        ModelMode::Force => {
            order.push(ep.model.clone());
            order.extend(ep.fallback_models.iter().cloned());
        }
        ModelMode::Auto | ModelMode::List => {
            let listed = list_models(t);
            if !ep.model.is_empty() {
                order.push(ep.model.clone());
            }
            order.extend(ep.fallback_models.iter().cloned());
            match listed {
                Ok(all) => {
                    // Auto con modelo "preferido" que ya no existe: se ignora.
                    if ep.mode == ModelMode::Auto
                        && !ep.model.is_empty()
                        && !all.contains(&ep.model)
                    {
                        order.remove(0);
                    }
                    order.extend(all);
                }
                Err(e) if order.is_empty() => return Err(e),
                Err(_) => {} // /models caido pero hay modelo explicito
            }
        }
    }
    let mut seen = std::collections::HashSet::new();
    order.retain(|m| !m.is_empty() && seen.insert(m.clone()));
    order.sort_by_key(|m| cooling(m)); // sin tocar el orden relativo (estable)
    if !ep.fallback {
        order.truncate(1);
    }
    if order.is_empty() {
        return Err("el endpoint no tiene modelos de chat".into());
    }
    Ok(order)
}

// Modelos que dieron 429 hace poco: van al final en vez de gastar un intento.
static COOLDOWN: Mutex<Option<HashMap<String, Instant>>> = Mutex::new(None);

fn cool_down(model: &str) {
    COOLDOWN
        .lock()
        .unwrap()
        .get_or_insert_with(Default::default)
        .insert(model.to_string(), Instant::now());
}

fn cooling(model: &str) -> bool {
    COOLDOWN
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m.get(model))
        .is_some_and(|t| t.elapsed() < Duration::from_secs(60))
}

// ------------------------------------------------------------ herramientas

pub fn tools_schema() -> Vec<Value> {
    let q = json!({"type":"string","description":
        "Census query: free words match name or path; path:/dir ; ext:pdf ; size:>1000000 ; size:<500"});
    let n = json!({"type":"integer","description":"max results (default 20, max 50)"});
    let f = |name: &str, desc: &str, props: Value| {
        json!({"type":"function","function":{"name":name,"description":desc,
            "parameters":{"type":"object","properties":props}}})
    };
    vec![
        f(
            "search",
            "Find files and folders by name or path.",
            json!({"query":q,"limit":n}),
        ),
        f(
            "count",
            "Count entries matching a query, without listing them.",
            json!({"query":q}),
        ),
        f(
            "largest",
            "Largest files matching a query.",
            json!({"query":q,"limit":n}),
        ),
        f(
            "recent",
            "Most recently modified entries matching a query.",
            json!({"query":q,"limit":n}),
        ),
        f(
            "stats",
            "Index state: volumes, files and folders.",
            json!({}),
        ),
    ]
}

fn spec_for(args: &Value, sort: Sort, files_only: bool) -> Spec {
    let mut s = Spec::new(args["query"].as_str().unwrap_or(""));
    s.sort = sort;
    s.files_only = files_only;
    s.limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
    s
}

fn rows(ix: &Index, total: usize, ids: &[u32]) -> Value {
    let items: Vec<Value> = ids
        .iter()
        .map(|&id| {
            let h = query::hit(ix, id);
            json!({"path":h.path,"size_bytes":h.size,"mtime_ns":h.mtime_ns,
                   "kind": if ix.is_dir(id) {"dir"} else {"file"}})
        })
        .collect();
    json!({"total": total, "returned": items.len(), "results": items})
}

/// Ejecuta una herramienta de solo lectura sobre el indice.
pub fn call_tool(ix: &Index, name: &str, args: &Value) -> Value {
    match name {
        "search" => {
            let s = spec_for(args, Sort::NameAsc, false);
            let (t, ids) = query::search(ix, &s);
            rows(ix, t, &ids)
        }
        "count" => {
            let mut s = spec_for(args, Sort::NameAsc, false);
            s.limit = 1;
            json!({"total": query::search(ix, &s).0})
        }
        "largest" => {
            let s = spec_for(args, Sort::SizeDesc, true);
            let (t, ids) = query::search(ix, &s);
            rows(ix, t, &ids)
        }
        "recent" => {
            let s = spec_for(args, Sort::DateDesc, false);
            let (t, ids) = query::search(ix, &s);
            rows(ix, t, &ids)
        }
        "stats" => {
            let dirs = (0..ix.len() as u32).filter(|&i| ix.is_dir(i)).count();
            json!({"volumes": ix.roots.len(), "entries": ix.len(), "dirs": dirs,
                   "files": ix.len().saturating_sub(dirs), "device": ix.host.hostname,
                   "built_at": ix.built_at})
        }
        other => json!({"error": format!("herramienta desconocida: {other}")}),
    }
}

fn system_prompt(ix: &Index, spanish: bool) -> String {
    let lang = if spanish {
        "Respondé en español, breve y concreto."
    } else {
        "Respond in English, brief and concrete."
    };
    format!(
        "You are the assistant of Census, an instant index of FILE AND FOLDER NAMES (no file \
         contents) of the device \"{}\" ({}). Answer using the tools; never invent paths or \
         numbers. Sizes are bytes: show KB/MB/GB with one decimal. Tools are read-only. {lang}",
        ix.host.hostname, ix.host.os
    )
}

fn clip(mut s: String) -> String {
    if s.len() > MAX_TOOL_CHARS {
        let cut = (0..=MAX_TOOL_CHARS)
            .rev()
            .find(|&i| s.is_char_boundary(i))
            .unwrap_or(0);
        s.truncate(cut);
        s.push_str("…(truncated)");
    }
    s
}

fn ask_tools(
    t: &dyn Transport,
    model: &str,
    question: &str,
    ix: &Index,
    spanish: bool,
) -> Result<String, Fail> {
    let tools = tools_schema();
    let mut messages = vec![
        json!({"role":"system","content":system_prompt(ix, spanish)}),
        json!({"role":"user","content":question}),
    ];
    for _ in 0..MAX_STEPS {
        let body = json!({"model":model,"messages":messages,"tools":tools,
                          "tool_choice":"auto","temperature":0.2});
        let (code, v) = t.post("/chat/completions", &body).map_err(Fail::Model)?;
        if code == 429 {
            cool_down(model);
        }
        if code != 200 {
            return Err(classify(code, &v));
        }
        let msg = v
            .pointer("/choices/0/message")
            .cloned()
            .unwrap_or(Value::Null);
        let calls = msg
            .get("tool_calls")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if calls.is_empty() {
            let text = msg["content"].as_str().unwrap_or("").trim().to_string();
            return if text.is_empty() {
                Err(Fail::Model("respuesta vacia".into()))
            } else {
                Ok(text)
            };
        }
        messages.push(
            json!({"role":"assistant","content":msg.get("content").cloned()
            .unwrap_or(Value::Null),"tool_calls":calls}),
        );
        for c in &calls {
            let name = c
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let args: Value = c
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| json!({}));
            messages.push(json!({"role":"tool","tool_call_id":c.get("id").cloned()
                .unwrap_or(Value::Null),"content":clip(call_tool(ix, name, &args).to_string())}));
        }
    }
    Err(Fail::Model("demasiados pasos".into()))
}

/// Sin `tools`: el modelo devuelve `{"query","sort","limit"}` y la respuesta se
/// arma aca. Sirve para modelos locales chicos que no soportan funciones.
fn ask_translate(
    t: &dyn Transport,
    model: &str,
    question: &str,
    ix: &Index,
) -> Result<String, Fail> {
    let sys = "Translate the user's request into a file-name search. Reply with ONLY a JSON \
        object: {\"query\":\"...\",\"sort\":\"name|size|date\",\"limit\":20}. Query syntax: free \
        words match name or path; path:/dir ; ext:pdf ; size:>1000000 ; size:<500. \
        Use sort=size for 'largest/biggest', sort=date for 'recent/newest'.";
    let body = json!({"model":model,"temperature":0,"messages":[
        {"role":"system","content":sys},{"role":"user","content":question}]});
    let (code, v) = t.post("/chat/completions", &body).map_err(Fail::Model)?;
    if code == 429 {
        cool_down(model);
    }
    if code != 200 {
        return Err(classify(code, &v));
    }
    let text = v
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .unwrap_or("");
    let (a, b) = (text.find('{'), text.rfind('}'));
    let plan: Value = match (a, b) {
        (Some(a), Some(b)) if b > a => serde_json::from_str(&text[a..=b]).unwrap_or(Value::Null),
        _ => Value::Null,
    };
    let Some(q) = plan["query"].as_str() else {
        return Err(Fail::Model("el modelo no devolvio una consulta".into()));
    };
    let mut s = Spec::new(q);
    s.limit = plan["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
    s.sort = match plan["sort"].as_str().unwrap_or("name") {
        "size" => Sort::SizeDesc,
        "date" => Sort::DateDesc,
        _ => Sort::NameAsc,
    };
    let (total, ids) = query::search(ix, &s);
    let mut out = format!("{total} resultados para `{q}`\n");
    for id in ids {
        let h = query::hit(ix, id);
        out.push_str(&format!("{} ({} B)\n", h.path, h.size));
    }
    Ok(out)
}

#[derive(Debug, Clone)]
pub struct Answer {
    pub endpoint: String,
    pub model: String,
    pub text: String,
}

/// Pregunta con fallback entre modelos y entre endpoints.
pub fn ask_with(
    ep: &Endpoint,
    t: &dyn Transport,
    question: &str,
    ix: &Index,
    spanish: bool,
    on_model: &dyn Fn(&str, &str),
) -> Result<Answer, String> {
    let mut errors = Vec::new();
    for m in plan(t, ep)? {
        on_model(&ep.name, &m);
        let r = if ep.tools {
            ask_tools(t, &m, question, ix, spanish)
        } else {
            ask_translate(t, &m, question, ix)
        };
        match r {
            Ok(text) => {
                return Ok(Answer {
                    endpoint: ep.name.clone(),
                    model: m,
                    text,
                });
            }
            Err(Fail::Auth(e)) => return Err(e),
            Err(Fail::Model(e)) => errors.push(format!("{m}: {e}")),
        }
    }
    Err(errors.join("\n"))
}

#[cfg(any(feature = "tls", feature = "plain"))]
pub fn ask(
    cfg: &AiConfig,
    question: &str,
    ix: &Index,
    spanish: bool,
    on_model: &dyn Fn(&str, &str),
) -> Result<Answer, String> {
    let active: Vec<&Endpoint> = cfg.endpoints.iter().filter(|e| e.enabled).collect();
    if active.is_empty() {
        return Err("no hay un endpoint de IA configurado".into());
    }
    let mut errors = Vec::new();
    for ep in active {
        match ask_with(ep, &Http(ep), question, ix, spanish, on_model) {
            Ok(a) => return Ok(a),
            Err(e) => errors.push(format!("[{}] {e}", ep.name)),
        }
    }
    Err(errors.join("\n"))
}

#[cfg(any(feature = "tls", feature = "plain"))]
pub fn models_of(ep: &Endpoint) -> Result<Vec<String>, String> {
    list_models(&Http(ep))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    type Log = Arc<Mutex<Vec<(String, Option<String>, Value)>>>;

    /// Servidor OpenAI falso: `handler(path, body, call_n)` -> (status, json).
    struct Mock {
        base: String,
        log: Log,
        stop: Arc<AtomicBool>,
    }

    impl Mock {
        fn start(handler: impl Fn(&str, &Value, usize) -> (u16, Value) + Send + 'static) -> Mock {
            let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
            let port = server.server_addr().to_ip().unwrap().port();
            let log: Log = Arc::default();
            let stop = Arc::new(AtomicBool::new(false));
            let (l, s) = (log.clone(), stop.clone());
            std::thread::spawn(move || {
                let mut n = 0usize;
                while !s.load(Ordering::Relaxed) {
                    let Ok(Some(mut req)) = server.recv_timeout(Duration::from_millis(50)) else {
                        continue;
                    };
                    let path = req.url().to_string();
                    let auth = req
                        .headers()
                        .iter()
                        .find(|h| h.field.equiv("Authorization"))
                        .map(|h| h.value.to_string());
                    let mut body = String::new();
                    let _ = req.as_reader().read_to_string(&mut body);
                    let body: Value = serde_json::from_str(&body).unwrap_or(Value::Null);
                    l.lock().unwrap().push((path.clone(), auth, body.clone()));
                    let (code, v) = handler(&path, &body, n);
                    n += 1;
                    let resp = tiny_http::Response::from_string(v.to_string())
                        .with_status_code(code)
                        .with_header(
                            tiny_http::Header::from_bytes("Content-Type", "application/json")
                                .unwrap(),
                        );
                    let _ = req.respond(resp);
                }
            });
            Mock {
                base: format!("http://127.0.0.1:{port}/v1"),
                log,
                stop,
            }
        }

        fn paths(&self) -> Vec<String> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .map(|x| x.0.clone())
                .collect()
        }

        fn models_used(&self) -> Vec<String> {
            self.log
                .lock()
                .unwrap()
                .iter()
                .filter(|x| x.0.ends_with("/chat/completions"))
                .map(|x| x.2["model"].as_str().unwrap_or("").to_string())
                .collect()
        }
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    fn models(ids: &[&str]) -> Value {
        json!({"data": ids.iter().map(|i| json!({"id":i})).collect::<Vec<_>>()})
    }

    fn final_msg(t: &str) -> Value {
        json!({"choices":[{"message":{"role":"assistant","content":t}}]})
    }

    fn tool_msg(name: &str, args: Value) -> Value {
        json!({"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[
            {"id":"c1","type":"function","function":{"name":name,"arguments":args.to_string()}}]}}]})
    }

    fn sample() -> Index {
        use crate::index::{Builder, Root, F_DIR, F_ROOT};
        let mut b = Builder::with_capacity(8);
        let r = b.push(b"/sd", F_DIR | F_ROOT, 0, 0, 0, 0, 0, 0, 1, 1, 0o755);
        b.nodes[r as usize].parent = r;
        b.push(b"informe.pdf", 0, r, 9_000, 0, 5, 0, 0, 2, 1, 0o644);
        b.push(b"foto.jpg", 0, r, 100, 0, 9, 0, 0, 3, 1, 0o644);
        b.roots.push(Root {
            id: r,
            volume_id: "v".into(),
            path: "/sd".into(),
            ..Default::default()
        });
        b.host.hostname = "tel-casa".into();
        b.finish()
    }

    fn local(base: &str) -> Endpoint {
        Endpoint {
            name: "mock".into(),
            base_url: base.into(),
            local: true,
            ..Endpoint::local_preset("ollama").unwrap()
        }
    }

    fn quiet(_: &str, _: &str) {}

    #[test]
    fn auto_picks_best_model_and_runs_tools() {
        let m = Mock::start(|path, body, _| match path {
            "/v1/models" => (
                200,
                models(&[
                    "whisper-large-v3",
                    "llama-3.1-8b-instant",
                    "openai/gpt-oss-120b",
                ]),
            ),
            _ if body["messages"]
                .as_array()
                .is_some_and(|m| m.last().unwrap()["role"] == "tool") =>
            {
                (200, final_msg("El más grande es informe.pdf"))
            }
            _ => (200, tool_msg("largest", json!({"query":"","limit":3}))),
        });
        let ix = sample();
        let ep = local(&m.base);
        let a = ask_with(
            &ep,
            &Http(&ep),
            "¿el archivo más grande?",
            &ix,
            true,
            &quiet,
        )
        .unwrap();
        assert_eq!(a.model, "openai/gpt-oss-120b");
        assert!(a.text.contains("informe.pdf"));
        assert!(!m.models_used().iter().any(|x| x.contains("whisper")));
        // El resultado de la herramienta llego al modelo.
        let log = m.log.lock().unwrap();
        let last = log.last().unwrap();
        assert!(last.2.to_string().contains("informe.pdf"));
    }

    #[test]
    fn falls_back_to_next_model_on_429_then_5xx() {
        let m = Mock::start(|path, body, _| match path {
            "/v1/models" => (
                200,
                models(&[
                    "openai/gpt-oss-120b",
                    "llama-3.3-70b-versatile",
                    "qwen/qwen3-32b",
                ]),
            ),
            _ => match body["model"].as_str().unwrap() {
                "openai/gpt-oss-120b" => (429, json!({"error":{"message":"rate"}})),
                "llama-3.3-70b-versatile" => (503, json!({})),
                _ => (200, final_msg("ok")),
            },
        });
        let ep = local(&m.base);
        let a = ask_with(&ep, &Http(&ep), "hola", &sample(), true, &quiet).unwrap();
        assert_eq!(a.model, "qwen/qwen3-32b");
        assert_eq!(m.models_used().len(), 3);
    }

    #[test]
    fn fallback_off_stops_after_first_model() {
        let m = Mock::start(|path, _, _| match path {
            "/v1/models" => (200, models(&["openai/gpt-oss-120b", "qwen/qwen3-32b"])),
            _ => (500, json!({})),
        });
        let mut ep = local(&m.base);
        ep.fallback = false;
        assert!(ask_with(&ep, &Http(&ep), "x", &sample(), true, &quiet).is_err());
        assert_eq!(m.models_used().len(), 1);
    }

    #[test]
    fn force_mode_never_lists_models_and_uses_exact_name() {
        let m = Mock::start(|_, _, _| (200, final_msg("listo")));
        let mut ep = local(&m.base);
        ep.mode = ModelMode::Force;
        ep.model = "mi-modelo:Q4".into();
        let a = ask_with(&ep, &Http(&ep), "x", &sample(), true, &quiet).unwrap();
        assert_eq!(a.model, "mi-modelo:Q4");
        assert!(!m.paths().iter().any(|p| p.ends_with("/models")));
    }

    #[test]
    fn list_mode_uses_chosen_model_first_then_the_rest() {
        let m = Mock::start(|path, body, _| match path {
            "/v1/models" => (
                200,
                models(&["openai/gpt-oss-120b", "llama-3.1-8b-instant"]),
            ),
            _ if body["model"] == "llama-3.1-8b-instant" => (500, json!({})),
            _ => (200, final_msg("ok")),
        });
        let mut ep = local(&m.base);
        ep.mode = ModelMode::List;
        ep.model = "llama-3.1-8b-instant".into();
        let a = ask_with(&ep, &Http(&ep), "x", &sample(), true, &quiet).unwrap();
        assert_eq!(m.models_used()[0], "llama-3.1-8b-instant");
        assert_eq!(a.model, "openai/gpt-oss-120b");
    }

    #[test]
    fn auth_error_skips_models_and_moves_to_next_endpoint() {
        let bad = Mock::start(|_, _, _| (401, json!({"error":{"message":"bad key"}})));
        let good = Mock::start(|path, _, _| match path {
            "/v1/models" => (200, models(&["qwen/qwen3-32b"])),
            _ => (200, final_msg("desde el segundo")),
        });
        let (mut a, mut b) = (local(&bad.base), local(&good.base));
        a.name = "a".into();
        b.name = "b".into();
        let cfg = AiConfig {
            endpoints: vec![a, b],
        };
        let r = ask(&cfg, "x", &sample(), true, &quiet).unwrap();
        assert_eq!(r.endpoint, "b");
        assert_eq!(
            bad.log.lock().unwrap().len(),
            1,
            "401 no reintenta otros modelos"
        );
    }

    #[test]
    fn local_sends_no_authorization_header_remote_sends_bearer() {
        let m = Mock::start(|path, _, _| match path {
            "/v1/models" => (200, models(&["m"])),
            _ => (200, final_msg("ok")),
        });
        let ep = local(&m.base);
        ask_with(&ep, &Http(&ep), "x", &sample(), true, &quiet).unwrap();
        assert!(m.log.lock().unwrap().iter().all(|x| x.1.is_none()));
        let mut with_key = local(&m.base);
        with_key.api_key = "sk-secret".into();
        ask_with(&with_key, &Http(&with_key), "x", &sample(), true, &quiet).unwrap();
        assert!(m
            .log
            .lock()
            .unwrap()
            .iter()
            .any(|x| x.1.as_deref() == Some("Bearer sk-secret")));
    }

    #[test]
    fn translate_mode_works_without_tool_support() {
        let m = Mock::start(|path, body, _| match path {
            "/v1/models" => (200, models(&["tiny-3b"])),
            _ => {
                assert!(body.get("tools").is_none());
                (
                    200,
                    final_msg(
                        "```json\n{\"query\":\"ext:pdf\",\"sort\":\"size\",\"limit\":5}\n```",
                    ),
                )
            }
        });
        let mut ep = local(&m.base);
        ep.tools = false;
        let a = ask_with(&ep, &Http(&ep), "mis pdf", &sample(), true, &quiet).unwrap();
        assert!(a.text.starts_with("1 resultados"));
        assert!(a.text.contains("/sd/informe.pdf"));
    }

    #[test]
    fn models_endpoint_down_but_forced_model_still_works() {
        let m = Mock::start(|path, _, _| match path {
            "/v1/models" => (404, json!({})),
            _ => (200, final_msg("ok")),
        });
        let mut ep = local(&m.base);
        ep.model = "x".into();
        assert_eq!(
            ask_with(&ep, &Http(&ep), "q", &sample(), true, &quiet)
                .unwrap()
                .model,
            "x"
        );
        ep.model.clear();
        assert!(ask_with(&ep, &Http(&ep), "q", &sample(), true, &quiet).is_err());
    }

    #[test]
    fn validation_protects_the_key() {
        let mut e = Endpoint::groq("gsk_x");
        assert!(e.validate().is_ok());
        let mut e = Endpoint::groq("gsk_x");
        e.base_url = "http://api.groq.com/openai/v1".into();
        assert!(e
            .validate()
            .unwrap_err()
            .contains("solo se admite en IA local"));
        let mut e = Endpoint::groq("");
        assert!(e.validate().unwrap_err().contains("api_key"));
        let mut e = Endpoint::local_preset("ollama").unwrap();
        assert!(e.validate().is_ok());
        let mut e = Endpoint::groq("k");
        e.mode = ModelMode::Force;
        assert!(e.validate().is_err());
        let mut e = Endpoint::groq("k");
        e.base_url = "https://x/v1/chat/completions".into();
        assert!(e.validate().is_err());
        let mut e = Endpoint::groq("k");
        e.base_url = "https://x/v1/".into();
        e.validate().unwrap();
        assert_eq!(e.base_url, "https://x/v1");
    }

    #[test]
    fn masked_key_never_leaks_and_round_trips() {
        let mut cfg = AiConfig::default();
        cfg.replace(vec![Endpoint::groq("gsk_ABCDEFGH1234")])
            .unwrap();
        let shown = cfg.masked();
        let s = shown.to_string();
        assert!(!s.contains("ABCDEFGH") && s.contains("gsk_…1234"));
        // La UI reenvia lo mostrado: la clave real se conserva.
        let back: Vec<Endpoint> = serde_json::from_value(shown["endpoints"].clone()).unwrap();
        cfg.replace(back).unwrap();
        assert_eq!(cfg.endpoints[0].api_key, "gsk_ABCDEFGH1234");
    }

    #[test]
    fn rank_prefers_known_then_larger_and_drops_non_chat() {
        let r = rank_models(&[
            "whisper-large-v3".into(),
            "tiny-1b".into(),
            "big-70b".into(),
            "llama-3.3-70b-versatile".into(),
            "text-embedding-3".into(),
        ]);
        assert_eq!(r, vec!["llama-3.3-70b-versatile", "big-70b", "tiny-1b"]);
    }

    #[test]
    fn config_file_round_trip_has_private_permissions() {
        let p = std::env::temp_dir().join(format!("census-ai-{}.json", std::process::id()));
        let mut cfg = AiConfig::default();
        cfg.replace(vec![Endpoint::groq("gsk_1234567890")]).unwrap();
        cfg.save(&p).unwrap();
        assert_eq!(AiConfig::load(&p).endpoints[0].api_key, "gsk_1234567890");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        let _ = std::fs::remove_file(&p);
    }
}
