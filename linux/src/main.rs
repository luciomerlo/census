//! Entrada WIP de Census/Linux.
//!
//! Mientras el scanner/snapshot/catalogo estan en curso, el binario sirve para
//! verificar la identidad de nube y listar montajes. Ver `linux/PENDIENTE.md`.

use std::process::ExitCode;

use census::cloud;
use census::volumes;

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
        "mounts" => {
            let ctx = cloud::CloudContext::load();
            for m in volumes::mounts_with(&ctx) {
                let cloud = if m.cloud_id.is_empty() {
                    String::new()
                } else {
                    format!(
                        " cloud_id={} provider={} account={} unit={}{}",
                        m.cloud_id,
                        m.cloud_provider,
                        m.cloud_account,
                        m.cloud_unit,
                        if m.possible_duplicate {
                            " posible_duplicado"
                        } else {
                            ""
                        }
                    )
                };
                println!(
                    "{:<24} {:<10} {:<12} {:>8} volume_id={}{}",
                    m.path,
                    m.kind,
                    m.fs,
                    census::fmt::human(m.total),
                    m.volume_id,
                    cloud
                );
            }
            ExitCode::SUCCESS
        }
        "help" | "--help" | "-h" => {
            println!(
                "census-linux {} (WIP)\n\n\
                 census version\n\
                 census cloud-id <provider> <account> <unidad>\n\
                 census mounts\n\n\
                 En desarrollo: index, query, top, volumes, stats, bench, serve,\n\
                 peers, pull, mirrors, mcp, watch. Ver linux/PENDIENTE.md.",
                env!("CARGO_PKG_VERSION")
            );
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("'{other}' todavia no esta implementado. Ver linux/PENDIENTE.md");
            ExitCode::FAILURE
        }
    }
}
