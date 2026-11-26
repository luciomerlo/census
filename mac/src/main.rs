//! CLI de Census para macOS.
//!
//! Expone `version`, `volumes`, `cloud-id`, `fold` y `snapshot`
//! (`save`/`list`/`query`). `index`, `serve`, `mcp` y `atlas` siguen en
//! `PENDIENTE.md`.
//!
//! Regla de producto: indexar y consultar es **read-only**. Nada de lo que hay
//! acá borra o modifica archivos del usuario.

use std::path::PathBuf;

use serde::Serialize;

use census::catalogo::Catalog;
use census::config::Config;
use census::index::Builder;
use census::query::fold_str;
use census::snapshot::save_per_volume;
use census::source::{Fuente, Result as SourceResult, WalkFuente};
use census::types::{Marca, Provenance, Volume};

/// Fila de `census volumes --json`. Los nombres son contrato: los consume
/// Sort/Atlas y no se cambian.
#[derive(Serialize)]
struct VolumeRow {
    volume_id: String,
    disk_hw_id: Option<String>,
    identity_status: &'static str,
    federable: bool,
    mount: String,
    model: Option<String>,
    serial: Option<String>,
    capacity: u64,
    cloud_id: Option<String>,
}

/// Fila de `census snapshot save --json`.
#[derive(Serialize)]
struct SavedRow {
    volume_id: String,
    path: String,
    bytes: u64,
    nodes: u64,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    match cmd {
        "version" | "--version" | "-V" => {
            println!("census {} ({})", census::APP_VERSION, census::platform_name());
        }
        "volumes" => cmd_volumes(&args[1..]),
        "cloud-id" => {
            if args.len() < 4 {
                eprintln!("uso: census cloud-id <provider> <account> <unidad>");
                std::process::exit(2);
            }
            println!("{}", census::volumes::cloud_id(&args[1], &args[2], &args[3]));
        }
        "fold" => {
            if let Some(s) = args.get(1) {
                println!("{}", String::from_utf8_lossy(&fold_str(s)));
            }
        }
        "snapshot" => cmd_snapshot(&args[1..]),
        "help" | "--help" | "-h" => help(),
        other => {
            eprintln!("comando desconocido: {other} (probá `census help`)");
            std::process::exit(2);
        }
    }
}

fn help() {
    println!(
        "census {}\n\n\
         Uso:\n  census version\n  census volumes [--json]\n  census cloud-id <provider> <account> <unidad>\n  census fold <texto>\n\
         \n  census snapshot save [--root <ruta>]... [--json]   un .cnx por volume_id\n\
         \n  census snapshot list [--json]                      snapshots presentes y online/offline\n\
         \n  census snapshot query <consulta> [--json] [--todas] [--limit N] [--offset N] [--sort nombre]\n\n\
         En construcción: index, serve, mcp, atlas. Ver PENDIENTE.md.",
        census::APP_VERSION
    );
}

fn cmd_volumes(rest: &[String]) {
    let cfg = Config::load();
    let host = cfg.env_label();
    let vols = census::volumes::detect(&cfg, &host);
    if rest.iter().any(|a| a == "--json") {
        let mut rows: Vec<VolumeRow> = vols.iter().map(volume_row).collect();
        // Orden estable: por punto de montaje y, si empataran, por volume_id.
        rows.sort_by(|a, b| a.mount.cmp(&b.mount).then_with(|| a.volume_id.cmp(&b.volume_id)));
        print_json(&rows);
        return;
    }
    for v in &vols {
        println!(
            "{}\t{}\t{}\t{}\t{}",
            v.mount,
            v.fs,
            v.id,
            v.cloud_id.as_deref().unwrap_or("-"),
            v.identity_state.label()
        );
    }
}

fn volume_row(v: &Volume) -> VolumeRow {
    VolumeRow {
        volume_id: v.id.to_string(),
        disk_hw_id: v.disk_hw_id.clone(),
        identity_status: v.identity_state.label(),
        // §10: sin identidad inequívoca no se publica en federación.
        federable: census::volumes::federation_ok(v),
        mount: v.mount.clone(),
        model: v.model.clone(),
        serial: v.serial.clone(),
        capacity: v.total,
        cloud_id: v.cloud_id.clone(),
    }
}

fn cmd_snapshot(args: &[String]) {
    match args.first().map(|s| s.as_str()).unwrap_or("list") {
        "save" => snapshot_save(&args[1..]),
        "list" => snapshot_list(&args[1..]),
        "query" => snapshot_query(&args[1..]),
        other => {
            eprintln!("subcomando desconocido: snapshot {other} (save | list | query)");
            std::process::exit(2);
        }
    }
}

fn snapshot_save(args: &[String]) {
    let mut roots: Vec<String> = Vec::new();
    let mut json = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--root" => {
                i += 1;
                match args.get(i) {
                    Some(p) => roots.push(p.clone()),
                    None => exit_uso("--root necesita una ruta"),
                }
            }
            flag => exit_uso(&format!("bandera desconocida: {flag}")),
        }
        i += 1;
    }

    let cfg = Config::load();
    let host = cfg.env_label();
    // Se empareja volumen y ruta desde el principio: ordenar uno solo de los
    // dos vectores deja el `zip` desalineado.
    let mut pares: Vec<(Volume, PathBuf)> = Vec::new();
    if roots.is_empty() {
        // Sin `--root` se indexan los volúmenes locales detectados.
        for v in census::volumes::detect(&cfg, &host) {
            if !v.is_indexable() {
                continue;
            }
            let p = PathBuf::from(&v.mount);
            pares.push((v, p));
        }
    } else {
        for raw in &roots {
            let p = PathBuf::from(raw);
            if !p.exists() {
                eprintln!("aviso: {} no existe, se saltea", p.display());
                continue;
            }
            let v = census::source::synthetic_volume(&p, &host);
            pares.push((v, p));
        }
    }
    // Un `volume_id` una sola vez: si dos rutas dan el mismo volumen, gana la
    // primera por orden de montaje (determinista).
    pares.sort_by(|a, b| a.0.mount.cmp(&b.0.mount));
    let mut vistos: std::collections::HashSet<String> = std::collections::HashSet::new();
    pares.retain(|(v, _)| vistos.insert(v.id.to_string()));
    if pares.is_empty() {
        exit_uso("no hay nada para indexar (probá `census snapshot save --root <ruta>`)");
    }
    let vols: Vec<Volume> = pares.iter().map(|(v, _)| v.clone()).collect();

    let mut builder = Builder::new(vols.clone());
    for (v, path) in &pares {
        let fuente = WalkFuente {
            volume_id: v.id.clone(),
            host: host.clone(),
            skip_mounts: true,
            excludes: cfg.exclude_paths(),
        };
        let mut emitir = |lote: &[census::types::Entrada]| -> SourceResult<()> {
            builder.push_batch(lote);
            Ok(())
        };
        if let Err(e) = fuente.escanear(path, &mut emitir) {
            eprintln!("aviso: no se pudo recorrer {}: {e}", path.display());
        }
    }
    let built_at = census::time::secs(census::time::now_ns());
    let ix = builder.finish(
        built_at,
        // Generación: hoy es fija; la persistencia real llega con FSEvents.
        1,
        Marca::default(),
        Provenance {
            pc_id: census::config::pc_id(),
            host: host.clone(),
            platform: census::platform_name().into(),
            arch: census::arch_name().into(),
            env: cfg.env_label(),
            app_version: census::APP_VERSION.into(),
        },
    );

    let dir = census::config::snapshots_dir();
    let (ok, fallos) = save_per_volume(&ix, &dir);
    for f in &fallos {
        eprintln!("aviso: no se pudo guardar el snapshot de {f}");
    }
    if json {
        let filas: Vec<SavedRow> = ok
            .iter()
            .map(|s| SavedRow {
                volume_id: s.volume_id.clone(),
                path: s.path.display().to_string(),
                bytes: s.bytes,
                nodes: ix
                    .volumes
                    .iter()
                    .position(|v| v.id.as_ref() == s.volume_id)
                    .map(|i| ix.nodes.iter().filter(|n| n.vol as usize == i).count() as u64)
                    .unwrap_or(0),
            })
            .collect();
        print_json(&filas);
    } else {
        for s in &ok {
            println!("{}\t{}\t{} B", s.volume_id, s.path.display(), s.bytes);
        }
        println!(
            "{} snapshot(s) en {}; {} fallo(s)",
            ok.len(),
            dir.display(),
            fallos.len()
        );
    }
    if ok.is_empty() {
        std::process::exit(1);
    }
}

fn snapshot_list(args: &[String]) {
    let json = args.iter().any(|a| a == "--json");
    let cfg = Config::load();
    let vols = census::volumes::detect(&cfg, &cfg.env_label());
    let mut cat = Catalog::load(&census::config::snapshots_dir());
    cat.reconcile(&vols);
    for n in &cat.notices {
        eprintln!("aviso: se ignora {}: {}", n.file, n.reason);
    }
    let filas = cat.describe();
    if json {
        print_json(&filas);
        return;
    }
    let mut rows = Vec::new();
    for f in &filas {
        rows.push(vec![
            f.volume_id.clone(),
            f.mount.clone(),
            if f.online { "online" } else { "offline" }.to_string(),
            f.nodes.to_string(),
            f.marca.clone(),
            census::fmt::stamp(f.built_at * census::time::NS),
        ]);
    }
    census::fmt::table(
        &["VOLUMEN", "MONTAJE", "ESTADO", "NODOS", "MARCA", "CONSTRUIDO"],
        &rows,
    );
}

fn snapshot_query(args: &[String]) {
    let mut texto: Option<String> = None;
    let mut json = false;
    let mut todas = false;
    let mut limit = 50usize;
    let mut offset = 0usize;
    let mut sort = census::query::Sort::PathAsc;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--json" => json = true,
            "--todas" | "--offline" => todas = true,
            "--limit" => {
                i += 1;
                limit = args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| exit_uso("--limit necesita un número"));
            }
            "--offset" => {
                i += 1;
                offset = args
                    .get(i)
                    .and_then(|v| v.parse().ok())
                    .unwrap_or_else(|| exit_uso("--offset necesita un número"));
            }
            "--sort" => {
                i += 1;
                sort = args
                    .get(i)
                    .and_then(|v| census::query::Sort::parse(v))
                    .unwrap_or_else(|| exit_uso("--sort necesita nombre|ruta|tamano|fecha"));
            }
            flag if flag.starts_with("--") => exit_uso(&format!("bandera desconocida: {flag}")),
            term => {
                texto = Some(match texto {
                    Some(t) => format!("{t} {term}"),
                    None => term.to_string(),
                });
            }
        }
        i += 1;
    }
    let Some(texto) = texto else {
        exit_uso("falta la consulta: census snapshot query <consulta>");
    };

    let cfg = Config::load();
    let vols = census::volumes::detect(&cfg, &cfg.env_label());
    let mut cat = Catalog::load(&census::config::snapshots_dir());
    cat.reconcile(&vols);
    for n in &cat.notices {
        eprintln!("aviso: se ignora {}: {}", n.file, n.reason);
    }
    if cat.volumes.is_empty() {
        eprintln!(
            "no hay snapshots en {} (creá uno con `census snapshot save`)",
            census::config::snapshots_dir().display()
        );
        std::process::exit(1);
    }
    let hits = cat.search(&texto, sort, offset, limit, todas);
    if json {
        print_json(&hits);
        return;
    }
    let mut rows = Vec::new();
    for h in &hits {
        rows.push(vec![
            h.volume_id.chars().take(8).collect::<String>(),
            h.path.clone(),
            census::fmt::human(h.size),
            census::fmt::stamp(h.mtime_ns),
            if h.online { "online" } else { "offline" }.to_string(),
        ]);
    }
    census::fmt::table(&["VOLUMEN", "RUTA", "TAMAÑO", "FECHA", "ESTADO"], &rows);
    println!("{} resultado(s)", hits.len());
}

fn print_json<T: Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => {
            eprintln!("no se pudo serializar el JSON: {e}");
            std::process::exit(1);
        }
    }
}

fn exit_uso(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}

#[cfg(test)]
mod tests {
    use super::*;
    use census::types::IdentityState;
    use serde_json::Value;

    /// §10 y `tests/identity.rs`: nombres en `snake_case`, 16 hex sin prefijo
    /// en los ids, y `identity_status` dentro del conjunto acordado. Este test
    /// no corre la CLI (necesita volúmenes montados); fija **el contrato** del
    /// JSON, que es lo que consumen Sort y Atlas.
    #[test]
    fn volumes_json_uses_the_contract_field_names() {
        let row = VolumeRow {
            volume_id: "0123456789abcdef".into(),
            disk_hw_id: Some("fedcba9876543210".into()),
            identity_status: IdentityState::Ok.label(),
            federable: true,
            mount: "/Volumes/Backup".into(),
            model: Some("Seagate Backup+".into()),
            serial: Some("SN-1".into()),
            capacity: 4_000_787_030_016,
            cloud_id: None,
        };
        let v: Value = serde_json::to_value(&row).unwrap();
        let obj = v.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(|k| k.as_str()).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "capacity",
                "cloud_id",
                "disk_hw_id",
                "federable",
                "identity_status",
                "model",
                "mount",
                "serial",
                "volume_id",
            ],
            "los nombres del JSON son contrato: no se cambian"
        );
        // La clave `volume_id` es la del contrato, no `id`/`disk_id`.
        assert_eq!(obj["volume_id"], "0123456789abcdef");
        assert_eq!(obj["identity_status"], "ok");
        assert!(obj["capacity"].is_u64());
        assert!(obj["federable"].is_boolean());
        assert!(obj["cloud_id"].is_null());
    }

    #[test]
    fn identity_status_labels_are_stable() {
        assert_eq!(IdentityState::Ok.label(), "ok");
        assert_eq!(IdentityState::SinIdentidad.label(), "sin_identidad");
        assert_eq!(IdentityState::Cloud.label(), "cloud");
        assert_eq!(
            IdentityState::PosibleDuplicado.label(),
            "posible_duplicado"
        );
    }
}

