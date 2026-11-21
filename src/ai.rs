//! Consultas en lenguaje natural: un modelo de Groq contesta usando las mismas
//! herramientas de sólo lectura que el servidor MCP (search, count, largest,
//! recent, stats) sobre el índice en memoria.
//!
//! La lógica es la de WizEverything: se listan los modelos de la cuenta, se
//! ordenan por preferencia y tamaño, se reintenta con el siguiente cuando uno
//! se queda sin cuota, y la clave se guarda cifrada con DPAPI para el usuario
//! de Windows (nunca en texto plano).

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::mcp;

pub const API: &str = "https://api.groq.com/openai/v1";

/// Mejor primero. Los modelos que la cuenta no liste se descartan; los
/// desconocidos se ordenan después por cantidad de parámetros.
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

/// Familias que no son modelos de chat.
const NOT_CHAT: &[&str] = &[
    "whisper", "tts", "guard", "playai", "orpheus", "embed", "distil", "compound",
];

const MAX_STEPS: usize = 8;
const MAX_TOOL_CHARS: usize = 4_000;

/// Última cuota conocida de un modelo, leída de las cabeceras de Groq.
#[derive(Clone, Copy, Debug)]
pub struct Quota {
    pub req_left: Option<u64>,
    pub tok_left: Option<u64>,
    pub at: std::time::Instant,
}

static QUOTAS: Mutex<Option<HashMap<String, Quota>>> = Mutex::new(None);

fn remember_quota(model: &str, v: &Value) {
    let rl = &v["_rl"];
    let q = Quota {
        req_left: rl["req_left"].as_u64(),
        tok_left: rl["tok_left"].as_u64(),
        at: std::time::Instant::now(),
    };
    if q.req_left.is_some() || q.tok_left.is_some() {
        QUOTAS
            .lock()
            .unwrap()
            .get_or_insert_with(Default::default)
            .insert(model.to_string(), q);
    }
}

pub fn quota(model: &str) -> Option<Quota> {
    QUOTAS.lock().unwrap().as_ref()?.get(model).copied()
}

/// Sin pedidos por hoy, o casi sin tokens en el minuto corriente (los tokens
/// se reponen en menos de un minuto).
fn exhausted(model: &str) -> bool {
    quota(model).is_some_and(|q| {
        q.req_left == Some(0)
            || (q.tok_left.is_some_and(|t| t < 1500) && q.at.elapsed().as_secs() < 60)
    })
}

/// Acceso HTTP, abstracto para poder probar el agente sin red.
pub trait Transport {
    fn get(&self, path: &str) -> Result<(u16, Value), String>;
    fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), String>;
}

pub struct Groq {
    key: String,
}

impl Groq {
    pub fn new(key: String) -> Self {
        Self { key }
    }

    fn agent() -> ureq::Agent {
        ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(90))
            .build()
    }

    fn rate_headers(resp: &ureq::Response) -> Value {
        let h = |n: &str| resp.header(n).and_then(|v| v.parse::<u64>().ok());
        json!({
            "req_left": h("x-ratelimit-remaining-requests"),
            "tok_left": h("x-ratelimit-remaining-tokens"),
            "req_limit": h("x-ratelimit-limit-requests"),
            "tok_limit": h("x-ratelimit-limit-tokens"),
        })
    }

    fn reply(r: Result<ureq::Response, ureq::Error>) -> Result<(u16, Value), String> {
        let (code, resp) = match r {
            Ok(resp) => (resp.status(), resp),
            Err(ureq::Error::Status(code, resp)) => (code, resp),
            Err(e) => return Err(e.to_string()),
        };
        let rl = Self::rate_headers(&resp);
        let mut v = resp.into_json::<Value>().unwrap_or(Value::Null);
        if v.is_object() {
            v["_rl"] = rl;
        }
        Ok((code, v))
    }
}

impl Transport for Groq {
    fn get(&self, path: &str) -> Result<(u16, Value), String> {
        Self::reply(
            Self::agent()
                .get(&format!("{API}{path}"))
                .set("Authorization", &format!("Bearer {}", self.key))
                .call(),
        )
    }

    fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), String> {
        Self::reply(
            Self::agent()
                .post(&format!("{API}{path}"))
                .set("Authorization", &format!("Bearer {}", self.key))
                .send_json(body.clone()),
        )
    }
}

/// Parámetros (en miles de millones) que declara el nombre del modelo.
fn params_b(id: &str) -> f32 {
    id.split(['-', '/', '_'])
        .filter_map(|p| p.strip_suffix('b').and_then(|n| n.parse::<f32>().ok()))
        .fold(0.0, f32::max)
}

/// Modelos de chat de la cuenta, mejor primero.
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

fn api_error(code: u16, v: &Value) -> String {
    let msg = v
        .pointer("/error/message")
        .and_then(Value::as_str)
        .unwrap_or("");
    match code {
        401 => "La clave de Groq no es válida / Invalid Groq API key".into(),
        429 | 413 => format!("Rate limited: {msg}"),
        _ => format!("Groq HTTP {code}: {msg}"),
    }
}

/// Modelos de chat que la cuenta puede usar, mejor primero.
pub fn list_models(t: &dyn Transport) -> Result<Vec<String>, String> {
    let (code, v) = t.get("/models")?;
    if code != 200 {
        return Err(api_error(code, &v));
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

/// Pregunta con failover: arranca por `preferred` si sigue existiendo y prueba
/// los demás por orden hasta que uno conteste. Sólo una clave inválida corta.
pub fn ask_auto(
    t: &dyn Transport,
    preferred: Option<&str>,
    question: &str,
    backend: &mcp::Backend,
    spanish: bool,
    on_model: &dyn Fn(&str),
) -> Result<(String, String), String> {
    let mut order = list_models(t)?;
    if let Some(p) = preferred {
        if let Some(i) = order.iter().position(|m| m == p) {
            let m = order.remove(i);
            order.insert(0, m);
        }
    }
    // Los que ya sabemos sin cuota van al final en vez de gastar un intento.
    order.sort_by_key(|m| exhausted(m));
    if order.is_empty() {
        return Err("No hay modelos de chat disponibles / No chat models available".into());
    }
    let mut errors = Vec::new();
    for m in order {
        on_model(&m);
        match ask(t, &m, question, backend, spanish) {
            Ok(answer) => return Ok((m, answer)),
            Err(e) if e.contains("Invalid Groq API key") => return Err(e),
            Err(e) => errors.push(format!("{m}: {e}")),
        }
    }
    Err(format!(
        "Ningún modelo pudo responder / No model could answer:\n{}",
        errors.join("\n")
    ))
}

fn system_prompt(spanish: bool) -> String {
    let lang = if spanish {
        "Respondé en español rioplatense, breve y concreto."
    } else {
        "Respond in English, brief and concrete."
    };
    format!(
        "You are the assistant of Census, an instant file index for Windows that reads the NTFS master \
         file table (and walks non-NTFS, network or cloud drives). Answer questions about this PC's files \
         using the tools; never guess numbers. Tool sizes are in bytes: show them as KB/MB/GB/TB with one \
         decimal. Paths use backslashes. The index covers every available drive, including cloud ones. \
         Tools are read-only: you cannot delete or move anything, but you can say what takes the most space. \
         {lang}"
    )
}

/// Pregunta; el modelo puede llamar herramientas de sólo lectura varias veces.
pub fn ask(
    t: &dyn Transport,
    model: &str,
    question: &str,
    backend: &mcp::Backend,
    spanish: bool,
) -> Result<String, String> {
    // `mcp::tools()` devuelve la respuesta de tools/list: las herramientas
    // están bajo la clave "tools".
    let tools: Vec<Value> = mcp::tools()
        .get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|tl| {
            json!({ "type": "function", "function": {
                "name": tl["name"], "description": tl["description"], "parameters": tl["inputSchema"],
            }})
        })
        .collect();

    let mut messages = vec![
        json!({ "role": "system", "content": system_prompt(spanish) }),
        json!({ "role": "user", "content": question }),
    ];

    for _ in 0..MAX_STEPS {
        let body = json!({
            "model": model,
            "messages": messages,
            "tools": tools,
            "tool_choice": "auto",
            "temperature": 0.2,
        });
        let (code, v) = t.post("/chat/completions", &body)?;
        remember_quota(model, &v);
        if code != 200 {
            return Err(api_error(code, &v));
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
            let text = msg
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            return if text.is_empty() {
                Err("Respuesta vacía (el modelo se cortó); probá de nuevo".into())
            } else {
                Ok(text)
            };
        }
        messages.push(json!({
            "role": "assistant",
            "content": msg.get("content").cloned().unwrap_or(Value::Null),
            "tool_calls": calls,
        }));
        for call in &calls {
            let name = call
                .pointer("/function/name")
                .and_then(Value::as_str)
                .unwrap_or("");
            let args: Value = call
                .pointer("/function/arguments")
                .and_then(Value::as_str)
                .and_then(|s| serde_json::from_str(s).ok())
                .unwrap_or_else(|| json!({}));
            let mut out = mcp::call(backend, name, &args).to_string();
            if out.len() > MAX_TOOL_CHARS {
                let cut = (0..=MAX_TOOL_CHARS)
                    .rev()
                    .find(|&i| out.is_char_boundary(i))
                    .unwrap_or(0);
                out.truncate(cut);
                out.push_str("…(truncated)");
            }
            messages.push(json!({
                "role": "tool",
                "tool_call_id": call.get("id").cloned().unwrap_or(Value::Null),
                "content": out,
            }));
        }
    }
    Err("La pregunta necesitó demasiados pasos / Too many steps".into())
}

// ------------------------------------------------------------------ la clave

fn settings_path() -> Option<std::path::PathBuf> {
    let base = std::env::var_os("APPDATA").map(std::path::PathBuf::from)?;
    Some(base.join("Census").join("settings.ini"))
}

/// Clave de Groq: `GROQ_API_KEY` primero, después la de `settings.ini`.
pub fn load_key() -> Option<String> {
    if let Ok(k) = std::env::var("GROQ_API_KEY") {
        if !k.trim().is_empty() {
            return Some(k.trim().to_string());
        }
    }
    let text = std::fs::read_to_string(settings_path()?).ok()?;
    for line in text.lines() {
        if let Some((k, v)) = line.split_once('=') {
            if k.trim() == "groq_key" {
                return unprotect(v.trim()).filter(|s| !s.is_empty());
            }
        }
    }
    None
}

pub fn has_key() -> bool {
    load_key().is_some()
}

/// Guarda la clave cifrada con DPAPI en `%APPDATA%\Census\settings.ini`.
pub fn save_key(plain: &str) -> Result<(), String> {
    let enc = protect(plain).ok_or("no se pudo cifrar la clave con DPAPI")?;
    let path = settings_path().ok_or("no hay APPDATA")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept: Vec<String> = previous
        .lines()
        .filter(|l| !l.trim_start().starts_with("groq_key"))
        .map(|l| l.to_string())
        .collect();
    kept.push(format!("groq_key={enc}"));
    std::fs::write(&path, kept.join("\n") + "\n").map_err(|e| e.to_string())
}

pub fn clear_key() -> Result<(), String> {
    let path = settings_path().ok_or("no hay APPDATA")?;
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    let kept: Vec<String> = previous
        .lines()
        .filter(|l| !l.trim_start().starts_with("groq_key"))
        .map(|l| l.to_string())
        .collect();
    std::fs::write(&path, kept.join("\n") + "\n").map_err(|e| e.to_string())
}

// -------------------------------------------------------------------- DPAPI

/// Cifra para el usuario de Windows actual; devuelve hex.
#[cfg(windows)]
pub fn protect(plain: &str) -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CRYPT_INTEGER_BLOB};
    let mut data = plain.as_bytes().to_vec();
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_mut_ptr(),
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptProtectData(
            &input,
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut out,
        )
    };
    if ok == 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    unsafe { LocalFree(out.pbData as _) };
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Descifra lo que produjo [`protect`] (sólo el mismo usuario de Windows).
#[cfg(windows)]
pub fn unprotect(hex: &str) -> Option<String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};
    let mut data: Vec<u8> = (0..hex.len() / 2)
        .filter_map(|i| u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok())
        .collect();
    if data.is_empty() {
        return None;
    }
    let input = CRYPT_INTEGER_BLOB {
        cbData: data.len() as u32,
        pbData: data.as_mut_ptr(),
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    let ok = unsafe {
        CryptUnprotectData(
            &input,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
            std::ptr::null(),
            0,
            &mut out,
        )
    };
    if ok == 0 {
        return None;
    }
    let bytes = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    unsafe { LocalFree(out.pbData as _) };
    String::from_utf8(bytes).ok()
}

#[cfg(not(windows))]
pub fn protect(_plain: &str) -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn unprotect(_hex: &str) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Transporte falso: devuelve respuestas enlatadas y anota los pedidos.
    struct Fake {
        models: Value,
        replies: Mutex<Vec<Value>>,
        seen: Mutex<Vec<(String, Value)>>,
    }

    impl Fake {
        fn new(replies: Vec<Value>) -> Self {
            Self {
                models: json!({ "data": [
                    { "id": "whisper-large-v3" },
                    { "id": "llama-3.1-8b-instant" },
                    { "id": "llama-3.3-70b-versatile" },
                    { "id": "qwen/qwen3-32b" },
                    { "id": "org/model-405b" },
                ]}),
                replies: Mutex::new(replies),
                seen: Mutex::new(Vec::new()),
            }
        }
    }

    impl Transport for Fake {
        fn get(&self, path: &str) -> Result<(u16, Value), String> {
            assert_eq!(path, "/models");
            Ok((200, self.models.clone()))
        }
        fn post(&self, path: &str, body: &Value) -> Result<(u16, Value), String> {
            self.seen
                .lock()
                .unwrap()
                .push((path.to_string(), body.clone()));
            let mut r = self.replies.lock().unwrap();
            if r.is_empty() {
                return Err("sin respuestas".into());
            }
            Ok((200, r.remove(0)))
        }
    }

    fn answer(text: &str) -> Value {
        json!({ "choices": [{ "message": { "role": "assistant", "content": text } }] })
    }

    fn tool_call(name: &str, args: Value) -> Value {
        json!({ "choices": [{ "message": {
            "role": "assistant",
            "content": null,
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": { "name": name, "arguments": args.to_string() }
            }]
        }}]})
    }

    fn backend() -> mcp::Backend {
        // Índice vacío: las herramientas contestan sin tocar el disco.
        let index = std::sync::Arc::new(crate::Index::default());
        let service = std::sync::Arc::new(crate::Service::new(std::sync::Arc::clone(&index)));
        mcp::Backend::Local { index, service }
    }

    #[test]
    fn params_are_read_from_the_model_id() {
        assert_eq!(params_b("llama-3.3-70b-versatile"), 70.0);
        assert_eq!(params_b("qwen/qwen3-32b"), 32.0);
        assert_eq!(params_b("openai/gpt-oss-120b"), 120.0);
        assert_eq!(params_b("llama-3.1-8b-instant"), 8.0);
        assert_eq!(params_b("modelo-sin-tamano"), 0.0);
    }

    #[test]
    fn ranking_prefers_the_known_list_then_bigger_models() {
        let available: Vec<String> = [
            "whisper-large-v3",
            "llama-3.1-8b-instant",
            "qwen/qwen3-32b",
            "org/model-405b",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let ranked = rank_models(&available);
        assert_eq!(ranked[0], "qwen/qwen3-32b");
        assert_eq!(ranked[1], "llama-3.1-8b-instant");
        assert_eq!(ranked[2], "org/model-405b"); // desconocido, ordenado por tamaño
        assert!(!ranked.iter().any(|m| m.contains("whisper")));
    }

    #[test]
    fn api_errors_are_translated() {
        let v = json!({ "error": { "message": "bad key" } });
        assert!(api_error(401, &v).contains("no es válida"));
        assert!(api_error(429, &v).starts_with("Rate limited"));
        assert!(api_error(500, &v).starts_with("Groq HTTP 500"));
    }

    #[test]
    fn a_plain_answer_finishes_in_one_step() {
        let fake = Fake::new(vec![answer("Hay 3 PDF.")]);
        let (model, text) =
            ask_auto(&fake, None, "cuantos pdf hay", &backend(), true, &|_| {}).unwrap();
        // El orden de PREFERRED manda: 70b versátil antes que qwen3-32b.
        assert_eq!(model, "llama-3.3-70b-versatile");
        assert_eq!(text, "Hay 3 PDF.");
        // Se mandaron las herramientas del MCP y el pedido de idioma.
        let seen = fake.seen.lock().unwrap();
        let body = &seen[0].1;
        assert!(body["tools"].as_array().unwrap().len() >= 5);
        assert!(body["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("español"));
    }

    #[test]
    fn tool_calls_feed_results_back_into_the_conversation() {
        let fake = Fake::new(vec![
            tool_call("count", json!({ "query": "" })),
            answer("0 archivos."),
        ]);
        let (_, text) =
            ask_auto(&fake, None, "cuantos archivos", &backend(), true, &|_| {}).unwrap();
        assert_eq!(text, "0 archivos.");
        let seen = fake.seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        let msgs = seen[1].1["messages"].as_array().unwrap();
        // system, user, assistant(tool_calls), tool
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[3]["role"], "tool");
        assert!(msgs[3]["content"].as_str().unwrap().contains("total"));
    }

    #[test]
    fn failover_moves_to_the_next_model() {
        // El primero responde 429 y el segundo contesta.
        let fake = Fake::new(vec![
            json!({ "error": { "message": "rate limited" }, "_rl": { "req_left": 0 } }),
            answer("listo"),
        ]);
        let tried = std::sync::Mutex::new(Vec::new());
        let (model, text) = ask_auto(&fake, None, "hola", &backend(), true, &|m| {
            tried.lock().unwrap().push(m.to_string())
        })
        .unwrap();
        assert_eq!(text, "listo");
        let tried = tried.into_inner().unwrap();
        assert!(tried.len() >= 2, "debería haber reintentado: {tried:?}");
        assert_ne!(model, tried[0]);
    }

    #[test]
    fn an_empty_answer_is_an_error() {
        let fake = Fake::new(vec![answer("   ")]);
        assert!(ask(&fake, "m", "p", &backend(), true).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn dpapi_round_trip() {
        let secret = "gsk_prueba_123";
        let enc = protect(secret).expect("DPAPI disponible");
        assert_ne!(enc, secret);
        assert_eq!(unprotect(&enc).as_deref(), Some(secret));
        assert!(unprotect("no-es-hex").is_none());
        assert!(unprotect("").is_none());
    }
}
