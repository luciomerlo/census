//! Servidor MCP (Model Context Protocol) sobre stdio.
//!
//! Expone el índice como herramientas para un agente: buscar, contar, ver
//! estadísticas y sacar los más grandes / más recientes. No escribe nada:
//! todo es consulta.
//!
//! Regla del transporte: por stdout sólo van mensajes JSON-RPC, uno por línea.
//! Cualquier diagnóstico va por stderr.

use std::io::{BufRead, Write};
use std::path::Path;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use censusindex_ipc::protocol;
use censusindex_ipc::Client;

use crate::query::Sort;
use crate::time::filetime_to_unix;
use crate::{Index, Service};

const MAX_LIMIT: usize = 1000;
const SCAN_CAP: usize = 500_000;

#[derive(Clone, Debug)]
pub struct Hit {
    pub path: String,
    pub name: String,
    pub size: Option<u64>,
    pub mtime: i64,
    pub is_dir: bool,
}

fn join(path: &str, name: &str) -> String {
    if path.is_empty() {
        name.to_string()
    } else {
        format!("{path}\\{name}")
    }
}

pub enum Backend {
    /// Demonio ya corriendo (elevado) al que se le pregunta por IPC.
    Ipc(Mutex<Client>),
    /// Índice propio en este proceso.
    Local {
        index: Arc<Index>,
        service: Arc<Service>,
    },
}

impl Backend {
    pub fn open(root: Option<&str>, volume: char) -> Result<Self, String> {
        // CENSUS_MCP_LOCAL=1 fuerza el índice propio: lo usan los tests para no
        // depender de que haya (o no) un demonio corriendo en la máquina.
        let force_local = std::env::var("CENSUS_MCP_LOCAL")
            .map(|v| v != "0")
            .unwrap_or(false);
        if !force_local {
            if let Ok(client) = Client::new() {
                if client.censusindex_is_running() && client.is_ready() {
                    return Ok(Backend::Ipc(Mutex::new(client)));
                }
            }
        }
        let (index, _) = match root {
            Some(r) => crate::index_dir(Path::new(r))?,
            None => crate::index_volume(volume)?,
        };
        let service = Arc::new(Service::new(Arc::clone(&index)));
        Ok(Backend::Local { index, service })
    }

    pub fn describe(&self) -> String {
        match self {
            Backend::Ipc(_) => "backend: demonio CensusIndex por IPC".to_string(),
            Backend::Local { index, .. } => format!(
                "backend: índice local (volumen {}, {} nodos)",
                index.letter,
                index.len()
            ),
        }
    }

    pub fn search(
        &self,
        q: &str,
        sort: u32,
        offset: usize,
        limit: usize,
    ) -> Result<(usize, Vec<Hit>), String> {
        match self {
            Backend::Ipc(c) => {
                let mut c = c.lock().map_err(|_| "cliente IPC envenenado")?;
                let raw = c
                    .query_raw(q, sort, offset as u32, limit as u32, protocol::req::BASIC)
                    .ok_or("el demonio no respondió")?;
                let reply = protocol::parse_reply(&raw).ok_or("respuesta IPC inválida")?;
                let hits = reply
                    .items
                    .into_iter()
                    .map(|it| Hit {
                        path: join(&it.path, &it.name),
                        name: it.name,
                        size: it.size,
                        mtime: filetime_to_unix(it.modified_ft),
                        is_dir: it.is_dir,
                    })
                    .collect();
                Ok((reply.total as usize, hits))
            }
            Backend::Local { index, service } => {
                let (total, ids) = service.page(q, Sort::from_proto(sort), offset, limit);
                let hits = ids
                    .iter()
                    .map(|&id| {
                        let n = &index.nodes[id as usize];
                        Hit {
                            path: index.full_path(id),
                            name: index.name_string(id),
                            size: if n.size == u64::MAX {
                                None
                            } else {
                                Some(n.size)
                            },
                            mtime: n.mtime,
                            is_dir: n.flags & crate::index::F_DIR != 0,
                        }
                    })
                    .collect();
                Ok((total, hits))
            }
        }
    }

    /// Trae coincidencias para ordenar por tamaño o fecha del lado del agente.
    /// Acotado a `SCAN_CAP` para no arrastrar un millón de items.
    pub fn collect(&self, q: &str) -> Result<(usize, Vec<Hit>), String> {
        match self {
            Backend::Ipc(c) => {
                let mut c = c.lock().map_err(|_| "cliente IPC envenenado")?;
                let reply = c.query_all(q, protocol::sort::NAME_ASC, 100_000)?;
                let mut hits: Vec<Hit> = reply
                    .items
                    .into_iter()
                    .take(SCAN_CAP)
                    .map(|it| Hit {
                        path: join(&it.path, &it.name),
                        name: it.name,
                        size: it.size,
                        mtime: filetime_to_unix(it.modified_ft),
                        is_dir: it.is_dir,
                    })
                    .collect();
                hits.truncate(SCAN_CAP);
                Ok((reply.total as usize, hits))
            }
            Backend::Local { index, service } => {
                let (total, ids) = service.page(q, Sort::NameAsc, 0, SCAN_CAP);
                let hits = ids
                    .iter()
                    .map(|&id| {
                        let n = &index.nodes[id as usize];
                        Hit {
                            path: index.full_path(id),
                            name: index.name_string(id),
                            size: if n.size == u64::MAX {
                                None
                            } else {
                                Some(n.size)
                            },
                            mtime: n.mtime,
                            is_dir: n.flags & crate::index::F_DIR != 0,
                        }
                    })
                    .collect();
                Ok((total, hits))
            }
        }
    }

    pub fn stats(&self) -> Value {
        match self {
            Backend::Ipc(c) => {
                let mut c = match c.lock() {
                    Ok(c) => c,
                    Err(_) => return json!({"error": "cliente IPC envenenado"}),
                };
                match c.query_raw("", protocol::sort::NAME_ASC, 0, 1, protocol::req::BASIC) {
                    Some(raw) => match protocol::parse_reply(&raw) {
                        Some(r) => json!({"backend": "ipc", "items": r.total}),
                        None => json!({"error": "respuesta IPC inválida"}),
                    },
                    None => json!({"error": "el demonio no respondió"}),
                }
            }
            Backend::Local { index, .. } => json!({
                "backend": "local",
                "volume": index.letter.to_string(),
                "items": index.len(),
                "files": index.stats.files,
                "dirs": index.stats.dirs,
                "records": index.stats.records,
                "skipped": index.stats.skipped,
                "name_bytes": index.stats.name_units * 2,
                "build_ms": index.stats.build_ms,
                "built_at": index.built_at,
            }),
        }
    }
}

fn human_size(bytes: u64) -> String {
    const U: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut i = 0;
    while v >= 1024.0 && i + 1 < U.len() {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", U[i])
    }
}

fn stamp(unix: i64) -> String {
    let s = crate::time::stamp_local(unix);
    if s.is_empty() {
        "?".into()
    } else {
        s
    }
}

fn format_hits(total: usize, offset: usize, hits: &[Hit]) -> String {
    let mut s = format!(
        "{total} resultados (mostrando {} desde {offset})",
        hits.len()
    );
    for h in hits {
        let size = match h.size {
            Some(b) if !h.is_dir => human_size(b),
            _ => "<dir>".to_string(),
        };
        s.push('\n');
        s.push_str(&format!("{:>10}  {}  {}", size, stamp(h.mtime), h.path));
    }
    s
}

fn hits_json(hits: &[Hit]) -> Value {
    Value::Array(
        hits.iter()
            .map(|h| {
                json!({
                    "path": h.path,
                    "name": h.name,
                    "size": h.size,
                    "modified": h.mtime,
                    "is_dir": h.is_dir,
                })
            })
            .collect(),
    )
}

fn ok_text(text: String, structured: Option<Value>) -> Value {
    let mut v = json!({
        "content": [{"type": "text", "text": text}],
        "isError": false,
    });
    if let Some(s) = structured {
        v["structuredContent"] = s;
    }
    v
}

fn tool_error(msg: &str) -> Value {
    json!({
        "content": [{"type": "text", "text": msg}],
        "isError": true,
    })
}

pub(crate) fn tools() -> Value {
    json!({"tools": [
        {
            "name": "search",
            "description": "Busca archivos y carpetas en el indice de CensusIndex. Acepta la misma sintaxis que las apps: terminos sueltos, \"path:\\\"C:\\\\dir\\\"\", file:, folder:, ext:rs,toml, size:>10mb, dm:today, rc:>=2024-01-31T10:00:00",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Consulta. Vacio = todo."},
                    "limit": {"type": "integer", "description": "Cuantos devolver (1-1000, default 50)."},
                    "offset": {"type": "integer", "description": "Paginacion (default 0)."},
                    "sort": {"type": "string", "enum": ["name", "name-desc"], "description": "Orden (default name)."}
                },
                "required": ["query"]
            }
        },
        {
            "name": "count",
            "description": "Cuenta cuantas entradas cumplen una consulta, sin traerlas.",
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string"}},
                "required": ["query"]
            }
        },
        {
            "name": "largest",
            "description": "Los archivos mas grandes que cumplen una consulta.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string", "description": "Filtro opcional. Vacio = todo el indice."},
                    "limit": {"type": "integer", "description": "1-1000, default 20."}
                }
            }
        },
        {
            "name": "recent",
            "description": "Los archivos modificados mas recientemente que cumplen una consulta.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "integer", "description": "1-1000, default 20."}
                }
            }
        },
        {
            "name": "stats",
            "description": "Estado del indice: cantidad de nodos, archivos, carpetas y de donde salen los datos.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ]})
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn arg_num(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| (v as usize).clamp(0, MAX_LIMIT))
        .unwrap_or(default)
}

fn sort_of(args: &Value) -> u32 {
    match arg_str(args, "sort").unwrap_or("name") {
        "name-desc" | "desc" => protocol::sort::NAME_DESC,
        _ => protocol::sort::NAME_ASC,
    }
}

pub(crate) fn call(backend: &Backend, name: &str, args: &Value) -> Value {
    match name {
        "search" => {
            let q = arg_str(args, "query").unwrap_or("");
            let limit = arg_num(args, "limit", 50).max(1);
            let offset = arg_num(args, "offset", 0);
            match backend.search(q, sort_of(args), offset, limit) {
                Ok((total, hits)) => {
                    let text = format_hits(total, offset, &hits);
                    ok_text(
                        text,
                        Some(json!({"total": total, "offset": offset, "items": hits_json(&hits)})),
                    )
                }
                Err(e) => tool_error(&e),
            }
        }
        "count" => {
            let q = arg_str(args, "query").unwrap_or("");
            match backend.search(q, protocol::sort::NAME_ASC, 0, 1) {
                Ok((total, _)) => {
                    ok_text(format!("{total} resultados"), Some(json!({"total": total})))
                }
                Err(e) => tool_error(&e),
            }
        }
        "largest" => {
            let q = arg_str(args, "query").unwrap_or("");
            let limit = arg_num(args, "limit", 20).max(1);
            match backend.collect(q) {
                Ok((_, mut hits)) => {
                    hits.sort_unstable_by_key(|h| std::cmp::Reverse(h.size.unwrap_or(0)));
                    hits.truncate(limit);
                    let text = format!(
                        "{} mas grandes\n{}",
                        hits.len(),
                        format_hits(hits.len(), 0, &hits)
                    );
                    ok_text(text, Some(hits_json(&hits)))
                }
                Err(e) => tool_error(&e),
            }
        }
        "recent" => {
            let q = arg_str(args, "query").unwrap_or("");
            let limit = arg_num(args, "limit", 20).max(1);
            match backend.collect(q) {
                Ok((_, mut hits)) => {
                    hits.sort_unstable_by_key(|h| std::cmp::Reverse(h.mtime));
                    hits.truncate(limit);
                    let text = format!(
                        "{} mas recientes\n{}",
                        hits.len(),
                        format_hits(hits.len(), 0, &hits)
                    );
                    ok_text(text, Some(hits_json(&hits)))
                }
                Err(e) => tool_error(&e),
            }
        }
        "stats" => {
            let s = backend.stats();
            ok_text(s.to_string(), Some(s))
        }
        other => tool_error(&format!("herramienta desconocida: {other}")),
    }
}

fn write_msg(out: &mut impl Write, v: &Value) {
    let _ = writeln!(out, "{v}");
    let _ = out.flush();
}

fn write_error(out: &mut impl Write, id: Value, code: i32, message: &str) {
    write_msg(
        out,
        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    );
}

/// Bucle principal del MCP. Bloquea hasta que se cierra stdin.
pub fn serve_stdio(backend: Backend) -> Result<(), String> {
    eprintln!("{}", backend.describe());
    let stdin = std::io::stdin();
    let mut out = std::io::stdout();

    for line in stdin.lock().lines() {
        let line = line.map_err(|e| format!("stdin: {e}"))?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let req: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                write_error(
                    &mut out,
                    Value::Null,
                    -32700,
                    &format!("JSON invalido: {e}"),
                );
                continue;
            }
        };
        let Some(id) = req.get("id").cloned() else {
            // Notificación (initialized, cancelled, ...): no lleva respuesta.
            continue;
        };
        let method = req.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = req.get("params").cloned().unwrap_or_else(|| json!({}));

        let reply = match method {
            "initialize" => {
                let version = params
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("2024-11-05")
                    .to_string();
                Ok(json!({
                    "protocolVersion": version,
                    "capabilities": {"tools": {"listChanged": false}},
                    "serverInfo": {"name": "censusindex", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Indice instantaneo del disco. Usa search para encontrar archivos por nombre, carpeta, extension, tamano o fecha; count para saber cuantos hay; largest y recent para rankings; stats para ver el estado del indice."
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tools()),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params
                    .get("arguments")
                    .cloned()
                    .unwrap_or_else(|| json!({}));
                Ok(call(&backend, name, &args))
            }
            "resources/list" => Ok(json!({"resources": []})),
            "prompts/list" => Ok(json!({"prompts": []})),
            other => Err((-32601, format!("metodo no soportado: {other}"))),
        };

        match reply {
            Ok(result) => write_msg(
                &mut out,
                &json!({"jsonrpc": "2.0", "id": id, "result": result}),
            ),
            Err((code, msg)) => write_error(&mut out, id, code, &msg),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_handles_the_root() {
        assert_eq!(join("", "C:"), "C:");
        assert_eq!(join("C:\\Users", "Lucio"), "C:\\Users\\Lucio");
    }

    #[test]
    fn sizes_are_human_readable() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(999), "999 B");
        assert_eq!(human_size(4096), "4.0 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
    }

    #[test]
    fn tool_schemas_are_complete() {
        let t = tools();
        let list = t["tools"].as_array().unwrap();
        assert_eq!(list.len(), 5);
        for tool in list {
            assert!(tool["name"].is_string());
            assert!(tool["description"].is_string());
            assert_eq!(tool["inputSchema"]["type"], "object");
        }
    }

    #[test]
    fn limits_are_clamped() {
        let args = json!({"limit": 10_000});
        assert_eq!(arg_num(&args, "limit", 50), MAX_LIMIT);
        assert_eq!(arg_num(&json!({}), "limit", 50), 50);
    }

    #[test]
    fn unknown_tool_is_an_error_not_a_panic() {
        let backend = Backend::Local {
            index: Arc::new(Index::default()),
            service: Arc::new(Service::new(Arc::new(Index::default()))),
        };
        let v = call(&backend, "nope", &json!({}));
        assert_eq!(v["isError"], true);
    }

    #[test]
    fn hits_json_has_the_documented_shape() {
        let hits = vec![Hit {
            path: "C:\\a\\b.txt".into(),
            name: "b.txt".into(),
            size: Some(12),
            mtime: 1_700_000_000,
            is_dir: false,
        }];
        let v = hits_json(&hits);
        assert_eq!(v[0]["path"], "C:\\a\\b.txt");
        assert_eq!(v[0]["size"], 12);
        assert_eq!(v[0]["is_dir"], false);
    }
}
