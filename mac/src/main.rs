//! CLI de Census para macOS.
//!
//! Esta primera entrega expone `version`, `volumes` y `fold` mientras se
//! completan `index`, `query`, `serve` y `mcp` (ver `MAC-PENDIENTE.md`).

use census::config::Config;
use census::query::fold_str;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cmd = args.first().map(|s| s.as_str()).unwrap_or("help");
    match cmd {
        "version" | "--version" | "-V" => {
            println!(
                "census {} ({})",
                census::APP_VERSION,
                census::platform_name()
            );
        }
        "volumes" => {
            let cfg = Config::load();
            let host = cfg.env_label();
            let vols = census::volumes::detect(&cfg, &host);
            for v in &vols {
                println!(
                    "{}\t{}\t{}\t{}\t{}",
                    v.mount,
                    v.fs,
                    v.id,
                    v.cloud_id.as_deref().unwrap_or("-"),
                    match v.identity_state {
                        census::types::IdentityState::Ok => "ok",
                        census::types::IdentityState::Cloud => "cloud",
                        census::types::IdentityState::PosibleDuplicado => "posible_duplicado",
                        census::types::IdentityState::SinIdentidad => "sin_identidad",
                    }
                );
            }
        }
        "cloud-id" => {
            if args.len() < 4 {
                eprintln!("uso: census cloud-id <provider> <account> <unidad>");
                std::process::exit(2);
            }
            println!(
                "{}",
                census::volumes::cloud_id(&args[1], &args[2], &args[3])
            );
        }
        "fold" => {
            if let Some(s) = args.get(1) {
                println!("{}", String::from_utf8_lossy(&fold_str(s)));
            }
        }
        "help" | "--help" | "-h" => {
            println!(
                "census {}\n\n\
                 Uso:\n  census version\n  census volumes\n  census cloud-id <provider> <account> <unidad>\n  census fold <texto>\n\n\
                 En construcción: index, query, serve, mcp, atlas. Ver MAC-PENDIENTE.md.",
                census::APP_VERSION
            );
        }
        other => {
            eprintln!("comando desconocido: {other} (probá `census help`)");
            std::process::exit(2);
        }
    }
}
