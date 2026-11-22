//! CLI v1 de Census/Linux: index, query, top, stats, volumes y scan.
//!
//! Snapshot global en `CENSUS_DATA_DIR/index.cns`; `serve` (HTTP 8787) usa el
//! mismo `Index`. Ver `docs/MINIMO.md`.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use census::cloud;
use census::index::{Index, F_DIR, F_SYMLINK};
use census::query::{self, Sort, Spec};
use census::volumes;
use census::{config, scan, snapshot};

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(String::as_str).unwrap_or("help");
    match cmd {
        "version" | "--version" | "-V" => {
            println!("census-linux {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        "cloud-id" => {
            if args.len() < 4 {
                eprintln!("uso: census cloud-id <provider> <account> <unidad>");
                return ExitCode::FAILURE;
            }
            println!("{}", cloud::hash(&args[1], &args[2], &args[3]));
            ExitCode::SUCCESS
        }
        "mounts" | "volumes" => volumes_cmd(&args),
        "index" | "scan" | "bench" => index_cmd(&args),
        "query" => query_cmd(&args),
        "top" => top_cmd(&args),
        "stats" => stats_cmd(&args),
        "serve" => serve_cmd(&args),
        "help" | "--help" | "-h" => {
            println!(
                "census-linux {} (v1)\n\n\
                 census index [--json] [--full] [--cross-fs] [ruta...]\n\
                 census query <texto> [--path P] [--ext E] [--min-size N] [--sort S] [--limit N] [--json]\n\
                 census top [N] [--limit N] [--json]\n\
                 census stats [--json]\n\
                 census volumes [--json]\n\
                 census serve [--port 8787] [--lan]\n\n\
                 Snapshot: {}\n\
                 Sin args, index usa los montajes locales (sin red/nube).\n\
                 --lan exige token en config; API sólo lectura en /v1/.",
                env!("CARGO_PKG_VERSION"),
                snap_path().display()
            );
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("'{other}' no existe; probá `census help`");
            ExitCode::FAILURE
        }
    }
}

fn snap_path() -> PathBuf {
    config::data_dir().join("index.cns")
}

fn load_index() -> Result<Index, ExitCode> {
    let p = snap_path();
    match snapshot::load(&p) {
        Ok(ix) => Ok(ix),
        Err(e) => {
            eprintln!(
                "no hay índice en {} ({e}); corré `census index`",
                p.display()
            );
            Err(ExitCode::from(2))
        }
    }
}

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.windows(2)
        .find(|w| w[0] == name)
        .map(|w| w[1].as_str())
}

fn positional(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut skip = false;
    for a in args.iter().skip(1) {
        if skip {
            skip = false;
            continue;
        }
        if a.starts_with("--") {
            if !matches!(a.as_str(), "--json" | "--full" | "--cross-fs") {
                skip = true;
            }
            continue;
        }
        out.push(a.as_str());
    }
    out
}

fn volumes_cmd(args: &[String]) -> ExitCode {
    let ctx = cloud::CloudContext::load();
    let all = volumes::mounts_with(&ctx);
    let snap = snapshot::load(&snap_path()).ok();
    let pc_id = snap
        .as_ref()
        .map(|ix| ix.pc_id.clone())
        .unwrap_or_else(|| config::Config::load().pc_id);
    let built_at = snap.as_ref().map(|ix| ix.built_at).unwrap_or(0);
    let rows: Vec<serde_json::Value> = all
        .iter()
        .map(|m| {
            let marca = snap
                .as_ref()
                .filter(|ix| ix.roots.iter().any(|r| r.volume_id == m.volume_id))
                .map(|ix| serde_json::Value::String(ix.marca()))
                .unwrap_or(serde_json::Value::Null);
            serde_json::json!({
                "volume_id": m.volume_id,
                "disk_hw_id": m.disk_hw_id,
                "identity_status": m.identity_status,
                "federable": m.federable,
                "mount": m.path,
                "model": m.model,
                "serial": m.serial,
                "capacity": m.capacity,
                "partition_offset": m.partition_offset,
                "cloud_id": if m.cloud_id.is_empty() {
                    serde_json::Value::Null
                } else {
                    serde_json::Value::String(m.cloud_id.clone())
                },
                "marca": marca,
            })
        })
        .collect();
    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
            serde_json::json!({
                "pc_id": pc_id,
                "built_at": built_at,
                "volumes": rows,
            })
        );
        return ExitCode::SUCCESS;
    }
    for m in all {
        println!(
            "{:<24} {:<10} {:<12} {:>8} volume_id={}{}",
            m.path,
            m.kind,
            m.fs,
            census::fmt::human(m.total),
            m.volume_id,
            if m.cloud_id.is_empty() {
                String::new()
            } else {
                format!(" cloud_id={}", m.cloud_id)
            }
        );
    }
    ExitCode::SUCCESS
}

fn index_cmd(args: &[String]) -> ExitCode {
    let mut cfg = config::Config::load();
    let pc_id = cfg.ensure_pc_id();
    let json = args.iter().any(|a| a == "--json");
    let full = args.iter().any(|a| a == "--full");
    let one_fs = !args.iter().any(|a| a == "--cross-fs");
    let paths = positional(args);
    let roots = if paths.is_empty() {
        volumes::auto_roots()
    } else {
        paths
            .iter()
            .map(|p| volumes::root_meta_for(Path::new(*p)))
            .collect()
    };
    if roots.is_empty() {
        eprintln!("no hay volúmenes para indexar; pasá una ruta o revisá los montajes");
        return ExitCode::FAILURE;
    }
    let opts = scan::ScanOptions {
        full,
        host: cfg.host_info(),
        pc_id,
        excludes: cfg
            .exclude
            .iter()
            .filter(|s| !s.trim().is_empty())
            .map(PathBuf::from)
            .collect(),
        one_fs,
        ..Default::default()
    };
    let started = std::time::Instant::now();
    let (ix, report) = match scan::index_roots(roots, None, &opts) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("index: {e}");
            return ExitCode::FAILURE;
        }
    };
    let _ = config::ensure_dirs();
    let path = snap_path();
    let bytes = match snapshot::save(&ix, &path) {
        Ok(n) => n,
        Err(e) => {
            eprintln!("snapshot: {e}");
            return ExitCode::FAILURE;
        }
    };
    if json {
        let rows: Vec<serde_json::Value> = report
            .roots
            .iter()
            .map(|r| {
                serde_json::json!({
                    "path": r.path,
                    "volume_id": r.volume_id,
                    "nodes": r.nodes,
                    "files": r.files,
                    "dirs": r.dirs,
                    "complete": r.complete,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::json!({
                "pc_id": ix.pc_id,
                "built_at": ix.built_at,
                "generation": ix.generation,
                "total_ms": started.elapsed().as_millis(),
                "walk_ms": report.walk_ms,
                "assemble_ms": report.assemble_ms,
                "nodes": report.nodes,
                "files": report.files,
                "dirs": report.dirs,
                "volumes": rows,
                "snapshot": path.display().to_string(),
                "snapshot_bytes": bytes,
            })
        );
    } else {
        for r in &report.roots {
            println!(
                "{:<28} {:>10} nodos {:>10} archivos {:>8} carpetas {} walk={}ms",
                r.path,
                r.nodes,
                r.files,
                r.dirs,
                if r.complete { "ok" } else { "incompleto" },
                report.walk_ms
            );
        }
        println!(
            "total: {} nodos, {} archivos, {} carpetas · walk {} ms · assemble {} ms",
            report.nodes, report.files, report.dirs, report.walk_ms, report.assemble_ms
        );
        println!("snapshot: {} ({} bytes)", path.display(), bytes);
    }
    ExitCode::SUCCESS
}

fn query_cmd(args: &[String]) -> ExitCode {
    let ix = match load_index() {
        Ok(ix) => ix,
        Err(c) => return c,
    };
    let mut spec = Spec::new(&positional(args).join(" "));
    if let Some(v) = flag(args, "--path") {
        spec.path = Some(census::index::normalize_path(v));
    }
    if let Some(v) = flag(args, "--ext") {
        spec.ext = Some(v.trim_start_matches('.').to_ascii_lowercase());
    }
    if let Some(v) = flag(args, "--min-size") {
        spec.min_size = v.parse().ok();
    }
    if let Some(v) = flag(args, "--sort") {
        match Sort::parse(v) {
            Some(s) => spec.sort = s,
            None => {
                eprintln!("sort inválido: {v}");
                return ExitCode::FAILURE;
            }
        }
    }
    if let Some(v) = flag(args, "--limit") {
        spec.limit = v.parse().unwrap_or(spec.limit);
    }
    let (total, hits) = query::search(&ix, &spec);
    print_hits(&ix, &spec.raw, total, &hits, args, false)
}

fn top_cmd(args: &[String]) -> ExitCode {
    let ix = match load_index() {
        Ok(ix) => ix,
        Err(c) => return c,
    };
    let mut spec = Spec::new("");
    spec.sort = Sort::SizeDesc;
    spec.files_only = true;
    spec.limit = 50;
    if let Some(v) = flag(args, "--limit") {
        spec.limit = v.parse().unwrap_or(50);
    } else if let Some(v) = args.get(1).and_then(|s| s.parse::<usize>().ok()) {
        spec.limit = v;
    }
    let (total, hits) = query::search(&ix, &spec);
    print_hits(&ix, "", total, &hits, args, true)
}

fn print_hits(
    ix: &Index,
    q: &str,
    total: usize,
    hits: &[u32],
    args: &[String],
    top: bool,
) -> ExitCode {
    let json = args.iter().any(|a| a == "--json");
    let results: Vec<serde_json::Value> = hits
        .iter()
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
        .collect();
    if json {
        if top {
            println!(
                "{}",
                serde_json::json!({
                    "pc_id": ix.pc_id,
                    "built_at": ix.built_at,
                    "results": results,
                })
            );
        } else {
            println!(
                "{}",
                serde_json::json!({
                    "query": q,
                    "total": total,
                    "returned": results.len(),
                    "results": results,
                })
            );
        }
    } else {
        for r in &results {
            println!(
                "{:>10}  {:<19}  {}",
                r["size_bytes"].as_u64().unwrap_or(0),
                r["mtime_ns"]
                    .as_i64()
                    .map(census::time::stamp_local)
                    .unwrap_or_default(),
                r["path"].as_str().unwrap_or("")
            );
        }
        if total > results.len() {
            println!("({} de {} resultados)", results.len(), total);
        }
    }
    ExitCode::SUCCESS
}

fn serve_cmd(args: &[String]) -> ExitCode {
    let ix = match load_index() {
        Ok(ix) => ix,
        Err(c) => return c,
    };
    let cfg = config::Config::load();
    let port = flag(args, "--port")
        .and_then(|s| s.parse().ok())
        .unwrap_or(cfg.port);
    let lan = args.iter().any(|a| a == "--lan");
    if lan && cfg.token.trim().is_empty() {
        eprintln!("--lan requiere token en la config (census config token <secreto>)");
        return ExitCode::FAILURE;
    }
    let opts = census::http::Options {
        port,
        lan,
        token: cfg.token,
        name: config::hostname(),
    };
    match census::http::serve(&ix, &opts) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("serve: {e}");
            ExitCode::FAILURE
        }
    }
}

fn stats_cmd(args: &[String]) -> ExitCode {
    let ix = match load_index() {
        Ok(ix) => ix,
        Err(c) => return c,
    };
    let bytes: u64 = ix.roots.iter().map(|r| ix.node_size(r.id)).sum();
    let age = census::time::now_ns().saturating_sub(ix.built_at) / 1_000_000_000;
    if args.iter().any(|a| a == "--json") {
        println!(
            "{}",
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
            })
        );
    } else {
        println!("pc_id: {}", ix.pc_id);
        println!(
            "generación: {} · snapshot: {}",
            ix.generation,
            census::time::stamp_local(ix.built_at)
        );
        println!(
            "{} volúmenes · {} nodos · {} archivos · {} carpetas · {}",
            ix.roots.len(),
            ix.stats.nodes,
            ix.stats.files,
            ix.stats.dirs,
            census::fmt::human(bytes)
        );
        for r in &ix.roots {
            println!(
                "  {:<28} {:>10} nodos  {}  {}",
                r.path,
                r.nodes,
                if r.complete { "ok" } else { "incompleto" },
                if r.identity_status.is_empty() {
                    "-"
                } else {
                    &r.identity_status
                }
            );
        }
    }
    ExitCode::SUCCESS
}
