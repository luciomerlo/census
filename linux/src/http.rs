//! API HTTP v1 read-only: loopback por defecto, 6 GET bajo `/v1/`.
//!
//! `serve --lan` exige token Bearer y bindea 0.0.0.0. Sin discovery UDP; el
//! cliente usa IP/host manual. Ver `docs/MINIMO.md`.

use std::collections::HashMap;

use tiny_http::{Header, Request, Response, Server, StatusCode};

use crate::cloud::CloudContext;
use crate::index::{Index, F_DIR, F_SYMLINK};
use crate::query::{self, Sort, Spec};
use crate::volumes;

pub struct Options {
    pub port: u16,
    pub lan: bool,
    pub token: String,
    pub name: String,
}

pub fn serve(ix: &Index, opts: &Options) -> Result<(), String> {
    serve_with(&|| None, Some(ix), opts)
}

/// Variante para procesos de larga vida (Android): `current` entrega el indice
/// vigente en cada pedido, asi que un reindexado en segundo plano se ve sin
/// reiniciar el servidor. `fixed` es el camino de `serve`.
pub fn serve_live(
    current: &dyn Fn() -> std::sync::Arc<Index>,
    opts: &Options,
) -> Result<(), String> {
    serve_with(&|| Some(current()), None, opts)
}

fn serve_with(
    live: &dyn Fn() -> Option<std::sync::Arc<Index>>,
    fixed: Option<&Index>,
    opts: &Options,
) -> Result<(), String> {
    let host = if opts.lan { "0.0.0.0" } else { "127.0.0.1" };
    let server = Server::http((host, opts.port)).map_err(|e| {
        format!(
            "no pude escuchar en {host}:{} ({e}); ¿puerto ocupado?",
            opts.port
        )
    })?;
    eprintln!(
        "census {} en http://{host}:{}/v1/ (pc_id {})",
        env!("CARGO_PKG_VERSION"),
        opts.port,
        fixed
            .map(|i| i.pc_id.clone())
            .or_else(|| live().map(|i| i.pc_id.clone()))
            .unwrap_or_default()
    );
    let ctx = CloudContext::load();
    for request in server.incoming_requests() {
        match (live(), fixed) {
            (Some(arc), _) => {
                let _ = handle(&arc, opts, &ctx, request);
            }
            (None, Some(ix)) => {
                let _ = handle(ix, opts, &ctx, request);
            }
            _ => {}
        }
    }
    Ok(())
}

fn handle(ix: &Index, opts: &Options, ctx: &CloudContext, request: Request) -> Result<(), String> {
    let url = request.url().to_string();
    let (path, qs) = url.split_once('?').unwrap_or((&url, ""));
    let params = parse_qs(qs);
    if !opts.token.is_empty() && !authorized(&request, &opts.token) {
        return respond(
            request,
            StatusCode(401),
            serde_json::json!({"error":"unauthorized"}),
        );
    }
    match path {
        "/v1/info" => respond(
            request,
            StatusCode(200),
            serde_json::json!({
                "protocol": "census-v1",
                "pc_id": ix.pc_id,
                "name": opts.name,
                "version": env!("CARGO_PKG_VERSION"),
                "port": opts.port,
            }),
        ),
        "/v1/health" => respond(request, StatusCode(200), serde_json::json!({"ok": true})),
        "/v1/volumes" => {
            let rows: Vec<serde_json::Value> = volumes::mounts_with(ctx)
                .into_iter()
                .map(|m| {
                    let root = ix.roots.iter().find(|r| r.volume_id == m.volume_id);
                    let used = m.total.saturating_sub(m.free);
                    serde_json::json!({
                        "volume_id": m.volume_id,
                        "label": m.label,
                        "mount": m.path,
                        "fs": m.fs,
                        "total_bytes": m.total,
                        "free_bytes": m.free,
                        "used_bytes": used,
                        "removable": m.removable,
                        "online": true,
                        "built_at": root.map(|_| ix.built_at).unwrap_or(0),
                        "model": m.model,
                        "serial": m.serial,
                        "partition_offset": m.partition_offset,
                        "cloud_id": if m.cloud_id.is_empty() {
                            serde_json::Value::Null
                        } else {
                            serde_json::Value::String(m.cloud_id)
                        },
                        "identity_status": m.identity_status,
                        "federable": m.federable,
                        "marca": root
                            .map(|_| serde_json::Value::String(ix.marca()))
                            .unwrap_or(serde_json::Value::Null),
                    })
                })
                .collect();
            respond(
                request,
                StatusCode(200),
                serde_json::json!({
                    "pc_id": ix.pc_id,
                    "built_at": ix.built_at,
                    "volumes": rows,
                }),
            )
        }
        "/v1/search" => {
            let raw = params.get("q").map(String::as_str).unwrap_or("");
            let mut spec = Spec::new(raw);
            if let Some(p) = params.get("path") {
                spec.path = Some(crate::index::normalize_path(p));
            }
            if let Some(e) = params.get("ext") {
                spec.ext = Some(e.trim_start_matches('.').to_ascii_lowercase());
            }
            if let Some(n) = params.get("min_size") {
                spec.min_size = n.parse().ok();
            }
            if let Some(s) = params.get("sort") {
                if let Some(sort) = Sort::parse(s) {
                    spec.sort = sort;
                }
            }
            if let Some(n) = params.get("limit") {
                spec.limit = n.parse().unwrap_or(spec.limit);
            }
            let (total, hits) = query::search(ix, &spec);
            let results = hits_json(ix, &hits);
            respond(
                request,
                StatusCode(200),
                serde_json::json!({
                    "query": raw,
                    "total": total,
                    "returned": results.len(),
                    "results": results,
                }),
            )
        }
        "/v1/top" => {
            let mut spec = Spec::new("");
            spec.sort = Sort::SizeDesc;
            spec.files_only = true;
            spec.limit = params
                .get("limit")
                .and_then(|s| s.parse().ok())
                .unwrap_or(50);
            let (_total, hits) = query::search(ix, &spec);
            let results = hits_json(ix, &hits);
            respond(
                request,
                StatusCode(200),
                serde_json::json!({
                    "pc_id": ix.pc_id,
                    "built_at": ix.built_at,
                    "results": results,
                }),
            )
        }
        "/v1/stats" => {
            let bytes: u64 = ix.roots.iter().map(|r| ix.node_size(r.id)).sum();
            let age = crate::time::now_ns().saturating_sub(ix.built_at) / 1_000_000_000;
            respond(
                request,
                StatusCode(200),
                serde_json::json!({
                    "pc_id": ix.pc_id,
                    "built_at": ix.built_at,
                    "age_s": age,
                    "generation": ix.generation,
                    "volumes": ix.roots.len(),
                    "nodes": ix.stats.nodes,
                    "files": ix.stats.files,
                    "dirs": ix.stats.dirs,
                    "bytes": bytes,
                }),
            )
        }
        _ => respond(
            request,
            StatusCode(404),
            serde_json::json!({"error":"not_found"}),
        ),
    }
}

fn hits_json(ix: &Index, hits: &[u32]) -> Vec<serde_json::Value> {
    hits.iter()
        .map(|&id| {
            let h = query::hit(ix, id);
            let flags = ix.nodes[id as usize].flags;
            let kind = if flags & F_DIR != 0 {
                "dir"
            } else if flags & F_SYMLINK != 0 {
                "symlink"
            } else {
                "file"
            };
            serde_json::json!({
                "path": h.path,
                "name": h.name,
                "size_bytes": h.size,
                "alloc_bytes": h.alloc,
                "mtime_ns": h.mtime_ns,
                "kind": kind,
                "volume_id": h.volume_id,
                "source": {"pc_id": ix.pc_id, "state": "online"},
            })
        })
        .collect()
}

fn respond(request: Request, status: StatusCode, body: serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string(&body).map_err(|e| e.to_string())?;
    let header =
        Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).map_err(|_| "header")?;
    request
        .respond(
            Response::from_string(text)
                .with_status_code(status)
                .with_header(header),
        )
        .map_err(|e| e.to_string())
}

fn authorized(request: &Request, token: &str) -> bool {
    let want = format!("Bearer {token}");
    request
        .headers()
        .iter()
        .find(|h| {
            h.field
                .as_str()
                .as_str()
                .eq_ignore_ascii_case("authorization")
        })
        .map(|h| h.value.as_str() == want)
        .unwrap_or(false)
}

fn parse_qs(qs: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in qs.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(decode(k), decode(v));
    }
    out
}

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%' && i + 2 < b.len() {
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
                i += 3;
                continue;
            }
        }
        if b[i] == b'+' {
            out.push(b' ');
        } else {
            out.push(b[i]);
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}
