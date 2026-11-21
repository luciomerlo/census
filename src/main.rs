//! CLI de Census: indexar, consultar, benchmark y servir por IPC.

use std::collections::HashMap;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use census::{Index, Service, Sort};

fn usage() -> &'static str {
    "\
Census - indice instantaneo de archivos (NTFS)

  census index  [--volume C] [--root <dir>] [--json]
  census query  \"<expr>\" [--volume C] [--root <dir>] [--limit N] [--offset N] [--sort name|desc|size|date|path]
  census bench  [--volume C] [--repeat N]
  census serve  [--volume C] [--compat]
  census mcp    [--volume C] [--root <dir>]     servidor MCP por stdio para agentes
  census gui    [--volume C] [--root <dir>] [--theme dark|light|contrast] [--smoke]
  census drives  Unidades disponibles, su tipo y como se indexan
  census probe   Diagnostico del IPC: ventana, capacidades y estado del servidor
  census pause   [--until 22:00 | --for 8h]  Pausa el indexado de fondo
  census resume  Reanuda el indexado de fondo
  census ask <pregunta>   Lenguaje natural con Groq (usa las herramientas MCP)
  census key <clave>      Guarda la clave de Groq cifrada con DPAPI (--clear borra)

  --root <dir>   usa el recorrido de directorios (sin elevacion) en vez de la MFT
  --compat       ademas de CENSUS_IPC, registra la clase de ventana de Everything
"
}

struct Args {
    flags: HashMap<String, String>,
    positional: Vec<String>,
}

impl Args {
    fn parse(argv: Vec<String>) -> Self {
        let mut flags = HashMap::new();
        let mut positional = Vec::new();
        let mut it = argv.into_iter();
        while let Some(a) = it.next() {
            if let Some(name) = a.strip_prefix("--") {
                if ["json", "compat", "smoke"].contains(&name) {
                    flags.insert(name.to_string(), "1".to_string());
                    continue;
                }
                let value = it.next().unwrap_or_default();
                flags.insert(name.to_string(), value);
            } else {
                positional.push(a);
            }
        }
        Self { flags, positional }
    }

    fn get(&self, k: &str) -> Option<&str> {
        self.flags.get(k).map(|s| s.as_str())
    }

    fn has(&self, k: &str) -> bool {
        self.flags.contains_key(k)
    }

    fn num(&self, k: &str, default: usize) -> usize {
        self.get(k).and_then(|v| v.parse().ok()).unwrap_or(default)
    }
}

/// Antigüedad máxima del snapshot para reusarlo, en segundos. Sin delta por USN
/// un snapshot viejo miente, así que por defecto sólo sirve para arranques
/// seguidos (y `--max-age 0` obliga a releer la MFT siempre).
const MAX_AGE: u64 = 30;

/// Snapshot utilizable: existe, es reciente y cubre las mismas unidades que hoy.
fn cached_snapshot(args: &Args) -> Option<(Arc<Index>, String)> {
    let max_age: u64 = args
        .get("max-age")
        .and_then(|v| v.parse().ok())
        .unwrap_or(MAX_AGE);
    if max_age == 0 || args.has("rebuild") {
        return None;
    }
    let path = census::snapshot::default_path()?;
    let _ = &max_age;
    let age = std::fs::metadata(&path)
        .ok()?
        .modified()
        .ok()?
        .elapsed()
        .ok()?;
    // En pausa no tiene sentido reindexar: se usa el snapshot aunque esté
    // viejo, que es justo lo que el usuario pidió al pausar.
    let max_age = if census::pause::is_paused() {
        u64::MAX
    } else {
        max_age
    };
    if age.as_secs() > max_age {
        return None;
    }
    let ix = census::snapshot::load(&path).ok()?;
    let mut have: Vec<char> = ix.roots.iter().map(|&r| ix.letter_of(r)).collect();
    let mut want: Vec<char> = census::volumes::selected()
        .iter()
        .map(|d| d.letter())
        .collect();
    have.sort_unstable();
    want.sort_unstable();
    if have != want {
        return None;
    }
    let note = format!(
        "snapshot {} ({} nodos, {} s de antiguedad)",
        path.display(),
        ix.len(),
        age.as_secs()
    );
    Some((Arc::new(ix), note))
}

fn system_drive() -> char {
    std::env::var("SystemDrive")
        .ok()
        .and_then(|s| s.chars().next())
        .unwrap_or('C')
}

fn load(args: &Args) -> Result<(Arc<Index>, String), String> {
    if let Some(root) = args.get("root") {
        let (ix, st) = census::index_dir(std::path::Path::new(root))?;
        Ok((
            ix,
            format!(
                "dir {root} ({} registros en {} ms)",
                st.records, st.elapsed_ms
            ),
        ))
    } else if let Some(letter) = args.get("volume").and_then(|v| v.chars().next()) {
        let (ix, st) = census::index_volume(letter)?;
        Ok((
            ix,
            format!(
                "mft {letter}: ({} registros, {} usados, {:.1} MB leidos en {} ms)",
                st.records,
                st.used,
                st.bytes_read as f64 / (1024.0 * 1024.0),
                st.elapsed_ms
            ),
        ))
    } else {
        if let Some((ix, note)) = cached_snapshot(args) {
            return Ok((ix, note));
        }
        // Sin --volume ni --root: todas las unidades disponibles.
        let (ix, reports) = census::index_all()?;
        if !args.has("no-snapshot") {
            if let Some(path) = census::snapshot::default_path() {
                match census::snapshot::save(&ix, &path) {
                    Ok(bytes) => eprintln!("snapshot {} ({bytes} bytes)", path.display()),
                    Err(e) => eprintln!("aviso: no se pudo guardar el snapshot: {e}"),
                }
            }
        }
        let detail: Vec<String> = reports.iter().map(|r| r.summary()).collect();
        let fallos: Vec<String> = reports
            .iter()
            .filter_map(|r| r.note.as_ref().map(|n| format!("{}: {n}", r.name)))
            .collect();
        for f in &fallos {
            eprintln!("aviso: {f}");
        }
        let ram = (ix.len() * std::mem::size_of::<census::Node>() + ix.names.len() * 2) as f64
            / (1024.0 * 1024.0);
        let nodos = ix.len();
        Ok((
            ix,
            format!(
                "mft {} unidades ({}) · {nodos} nodos · {ram:.0} MB",
                reports.len(),
                detail.join(", ")
            ),
        ))
    }
}

fn cmd_index(args: &Args) -> Result<(), String> {
    let t0 = Instant::now();
    let (ix, note) = load(args)?;
    let ms = t0.elapsed().as_millis().max(1);
    let per_sec = ix.len() as f64 / (ms as f64 / 1000.0);
    let ram = (ix.nodes.len() * std::mem::size_of::<census::Node>() + ix.names.len() * 2) as f64
        / (1024.0 * 1024.0);

    if args.has("json") {
        println!(
            "{{\"nodes\":{},\"files\":{},\"dirs\":{},\"skipped\":{},\"ms\":{},\"nodes_per_sec\":{:.0},\"ram_mb\":{:.1}}}",
            ix.len(),
            ix.stats.files,
            ix.stats.dirs,
            ix.stats.skipped,
            ms,
            per_sec,
            ram
        );
        return Ok(());
    }
    println!("{note}");
    println!(
        "nodos {} (archivos {}, carpetas {}), descartados {}",
        ix.len(),
        ix.stats.files,
        ix.stats.dirs,
        ix.stats.skipped
    );
    println!(
        "indice construido en {ms} ms ({per_sec:.0} nodos/s), RAM estimada {ram:.1} MB (nombres {:.1} MB)",
        ix.names.len() as f64 * 2.0 / (1024.0 * 1024.0)
    );
    Ok(())
}

fn cmd_query(args: &Args) -> Result<(), String> {
    let expr = args.positional.first().cloned().unwrap_or_default();
    let (ix, note) = load(args)?;
    eprintln!("{note}");
    let service = Service::new(Arc::clone(&ix));
    let sort = args.get("sort").and_then(Sort::parse).unwrap_or_default();
    let limit = args.num("limit", 20);
    let offset = args.num("offset", 0);

    let t0 = Instant::now();
    let (total, ids) = service.page(&expr, sort, offset, limit);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;
    println!(
        "{total} resultados en {ms:.1} ms (mostrando {} desde {offset})",
        ids.len()
    );
    for id in ids {
        let n = &ix.nodes[id as usize];
        let size = if n.size == u64::MAX {
            String::from("<dir>")
        } else {
            n.size.to_string()
        };
        let ts = census::time::stamp_local(n.mtime);
        println!("{size:>12}  {ts}  {}", ix.full_path(id));
    }
    Ok(())
}

fn cmd_bench(args: &Args) -> Result<(), String> {
    let repeat = args.num("repeat", 3).max(1);

    // 1) Carga: lo que tiene que ser igual de rápido que Everything.
    let mut builds = Vec::new();
    let mut last: Option<Arc<Index>> = None;
    let mut note = String::new();
    for _ in 0..repeat {
        let t0 = Instant::now();
        let (ix, n) = load(args)?;
        builds.push(t0.elapsed().as_secs_f64() * 1000.0);
        last = Some(ix);
        note = n;
    }
    let ix = last.ok_or("no se construyo el indice")?;
    println!("{note}");
    builds.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let best = builds[0];
    let med = builds[builds.len() / 2];
    println!(
        "carga: mejor {best:.0} ms, mediana {med:.0} ms, {:.0} nodos/s, {} nodos, RAM {:.1} MB",
        ix.len() as f64 / (med / 1000.0),
        ix.len(),
        (ix.len() * std::mem::size_of::<census::Node>() + ix.names.len() * 2) as f64
            / (1024.0 * 1024.0)
    );

    // 2) Consultas: las cinco formas que emiten las apps.
    let service = Service::new(Arc::clone(&ix));
    let week_ago =
        census::time::format_local(census::time::now() - 7 * 86_400, "%Y-%m-%dT%H:%M:%S");
    let queries = [
        String::from(""),
        format!("path:\"{}\"", ix.dir_path(ix.root)),
        String::from("file:"),
        String::from("file: dm:today"),
        format!("file: rc:>={week_ago}"),
        String::from("node_modules"),
        String::from("ext:log"),
    ];
    println!("\nconsulta                                    resultados    ms");
    for q in &queries {
        // Primera pasada (sin caché) y segunda (paginación desde caché).
        let t0 = Instant::now();
        let (total, _) = service.page(q, Sort::NameAsc, 0, 1000);
        let cold = t0.elapsed().as_secs_f64() * 1000.0;
        let t1 = Instant::now();
        let _ = service.page(q, Sort::NameAsc, 250_000, 250_000);
        let warm = t1.elapsed().as_secs_f64() * 1000.0;
        let label: String = if q.is_empty() {
            "(todo)".into()
        } else {
            (*q).clone()
        };
        println!("{label:<42} {total:>10}  {cold:>7.1} ({warm:.1} pag)");
    }
    Ok(())
}

/// Pregunta en lenguaje natural contra el indice, con Groq.
fn cmd_ask(args: &Args) -> Result<(), String> {
    let question = args.positional.join(" ");
    if question.trim().is_empty() {
        return Err("falta la pregunta: census ask \"los pdf mas grandes de este mes\"".into());
    }
    let Some(key) = census::ai::load_key() else {
        return Err(
            "falta la clave de Groq: definí GROQ_API_KEY o corré `census key <clave>`".into(),
        );
    };
    let (ix, note) = load(args)?;
    eprintln!("{note}");
    let service = std::sync::Arc::new(census::Service::new(std::sync::Arc::clone(&ix)));
    let backend = census::mcp::Backend::Local { index: ix, service };
    let groq = census::ai::Groq::new(key);
    let (model, answer) = census::ai::ask_auto(&groq, None, &question, &backend, true, &|m| {
        eprintln!("[probando {m}]")
    })?;
    println!("{answer}");
    eprintln!("({model})");
    Ok(())
}

/// Guarda o borra la clave de Groq cifrada con DPAPI.
fn cmd_key(args: &Args) -> Result<(), String> {
    if args.has("clear") || args.positional.iter().any(|a| a == "borrar") {
        census::ai::clear_key()?;
        println!("clave borrada");
        return Ok(());
    }
    match args.positional.first() {
        Some(k) if !k.trim().is_empty() => {
            census::ai::save_key(k.trim())?;
            println!("clave guardada (cifrada con DPAPI para tu usuario de Windows)");
            Ok(())
        }
        _ => {
            println!(
                "clave configurada: {}",
                if census::ai::has_key() { "si" } else { "no" }
            );
            println!("uso: census key <clave-de-groq>   |   census key --clear");
            Ok(())
        }
    }
}

/// Diagnostico: ve la ventana IPC de un `census serve` en ejecucion.
/// `census pause [--until 22:00 | --for 8h]`: corta el trabajo de fondo.
fn cmd_pause(args: &Args) -> Result<(), String> {
    let hasta = if let Some(hhmm) = args.get("until") {
        let (h, m) = hhmm
            .split_once(':')
            .ok_or_else(|| String::from("--until espera HH:MM"))?;
        let h: u32 = h.parse().map_err(|_| String::from("hora inválida"))?;
        let m: u32 = m.parse().map_err(|_| String::from("minuto inválido"))?;
        Some(census::pause::until_local(h, m).ok_or_else(|| String::from("hora inválida"))?)
    } else if let Some(horas) = args.get("for") {
        let h: f64 = horas
            .parse()
            .map_err(|_| String::from("--for espera horas"))?;
        Some((census::time::now() + (h * 3600.0) as i64) * 1000)
    } else {
        None
    };
    let p = census::pause::pause_until(hasta).map_err(|e| e.to_string())?;
    match p.resume_at {
        Some(at) => println!("en pausa hasta {}", census::time::stamp_local(at / 1000)),
        None => println!("en pausa hasta que ejecutes `census resume`"),
    }
    println!("las consultas siguen funcionando; se frena el indexado de fondo");
    Ok(())
}

fn cmd_resume() -> Result<(), String> {
    census::pause::resume().map_err(|e| e.to_string())?;
    println!("reanudado");
    Ok(())
}

fn cmd_probe() -> Result<(), String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::FindWindowW;
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    for cls in [census_ipc::CLASS, census_ipc::COMPAT_CLASS] {
        let name = wide(cls);
        let h = unsafe { FindWindowW(name.as_ptr(), std::ptr::null()) };
        println!("{cls}: hwnd = {h:p}");
    }
    let mut client = census_ipc::client::Client::new().map_err(|e| e.to_string())?;
    println!("census_is_running: {}", client.census_is_running());
    println!("is_ready: {}", client.is_ready());
    // Capacidades (WM_USER+402) y que el reply anuncie solo lo que trae.
    let caps = client.caps().unwrap_or(0);
    println!(
        "caps: 0x{caps:04x} (name={} path={} full={} size={} mtime={} atime={} attrs={} run_count={} parcial={})",
        bit(caps, census_ipc::protocol::req::NAME),
        bit(caps, census_ipc::protocol::req::PATH),
        bit(caps, census_ipc::protocol::req::FULL_PATH),
        bit(caps, census_ipc::protocol::req::SIZE),
        bit(caps, census_ipc::protocol::req::DATE_MODIFIED),
        bit(caps, census_ipc::protocol::req::DATE_ACCESSED),
        bit(caps, census_ipc::protocol::req::ATTRIBUTES),
        bit(caps, census_ipc::protocol::req::RUN_COUNT),
        bit(caps, census_ipc::protocol::caps::PARTIAL_METADATA),
    );
    println!(
        "  identity={}",
        bit(caps, census_ipc::protocol::req::IDENTITY),
    );
    let asked = census_ipc::protocol::req::BASIC
        | census_ipc::protocol::req::RUN_COUNT
        | census_ipc::protocol::req::IDENTITY;
    match client.query_raw("", 1, 0, 1, asked) {
        Some(raw) => {
            let reply = census_ipc::protocol::parse_reply(&raw).ok_or("respuesta ilegible")?;
            println!(
                "query flags: pedidos=0x{asked:04x} devueltos=0x{:04x} (run_count incluido: {})",
                reply.flags,
                bit(reply.flags, census_ipc::protocol::req::RUN_COUNT)
            );
            println!("total: {}", reply.total);
            if let Some(fila) = reply.items.first() {
                match fila.identity {
                    Some((serial, frn)) => println!(
                        "identidad: vol_serial={serial:016x} frn={frn:016x} ({})",
                        fila.name
                    ),
                    None => println!("identidad: no disponible en la primera fila"),
                }
            }
        }
        None => println!("query: sin respuesta"),
    }
    Ok(())
}

fn bit(flags: u32, field: u32) -> &'static str {
    if flags & field != 0 {
        "si"
    } else {
        "no"
    }
}

/// Que unidades ve Census, de que tipo son y como se indexarian.
fn cmd_drives() -> Result<(), String> {
    let excluded = census::volumes::excluded_drives();
    if excluded.is_empty() {
        println!("excluidas: (ninguna)");
    } else {
        println!("excluidas: {}", excluded.join(", "));
    }
    println!(
        "{:<4} {:<8} {:<9} {:>10} {:>10}  {:<9} como",
        "unidad", "tipo", "fs", "total", "libre", "indexa"
    );
    for (d, on) in census::volumes::roster() {
        let how = if !on {
            "no"
        } else if d.uses_mft() {
            "mft"
        } else {
            "recorrido"
        };
        println!(
            "{:<4} {:<8} {:<9} {:>10} {:>10}  {:<9} {}",
            d.name,
            d.kind().label(),
            if d.fs.is_empty() { "-" } else { &d.fs },
            gib(d.total),
            gib(d.free),
            how,
            if d.label.is_empty() { "" } else { &d.label }
        );
    }
    Ok(())
}

fn gib(bytes: u64) -> String {
    if bytes == 0 {
        "-".to_string()
    } else {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

#[cfg(feature = "gui")]
fn cmd_gui(args: &Args) -> Result<(), String> {
    // La ventana no necesita la consola detras.
    census::win::hide_console();
    let (ix, note) = load(args)?;
    eprintln!("{note}");
    let theme = args
        .get("theme")
        .and_then(census::theme::Theme::parse)
        .unwrap_or(census::theme::Theme::Dark);
    let opts = census::gui::GuiOptions {
        theme,
        initial_query: args.positional.first().cloned().unwrap_or_default(),
        smoke_frames: args.has("smoke").then_some(30),
    };
    census::gui::run(ix, opts)
}

#[cfg(not(feature = "gui"))]
fn cmd_gui(_args: &Args) -> Result<(), String> {
    Err("este binario se compilo sin la feature `gui`".into())
}

fn cmd_mcp(args: &Args) -> Result<(), String> {
    let root = args.get("root").map(|s| s.to_string());
    let volume = args
        .get("volume")
        .and_then(|v| v.chars().next())
        .unwrap_or_else(system_drive);
    let backend = census::mcp::Backend::open(root.as_deref(), volume)?;
    census::mcp::serve_stdio(backend)
}

fn cmd_serve(args: &Args) -> Result<(), String> {
    let (ix, note) = load(args)?;
    eprintln!("{note}");
    let service = Arc::new(Service::new(Arc::clone(&ix)));

    let mut classes: Vec<&str> = vec![census_ipc::CLASS];
    if args.has("compat") {
        classes.push(census_ipc::COMPAT_CLASS);
    }
    let letter = ix.letter;
    println!(
        "Census sirviendo {} nodos de un volumen por servidor ({letter}:). Clases: {}. Ctrl+C para salir.",
        ix.len(),
        classes.join(", ")
    );
    println!(
        "Metadatos: run_count no disponible (siempre 0); accessed puede venir vacio. Respuesta: flags = campos incluidos, y WM_USER+402 devuelve las capacidades."
    );
    let used = census_ipc::server::serve(service, &classes)?;
    println!("clase activa: {used}");
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse(std::env::args().skip(1).collect());
    let cmd = args
        .positional
        .first()
        .cloned()
        .unwrap_or_else(|| "help".into());
    let rest = || Args {
        flags: args.flags.clone(),
        positional: args.positional[1..].to_vec(),
    };
    let result = match cmd.as_str() {
        "index" => cmd_index(&rest()),
        "query" => cmd_query(&rest()),
        "bench" => cmd_bench(&rest()),
        "serve" => cmd_serve(&rest()),
        "mcp" => cmd_mcp(&rest()),
        "gui" => cmd_gui(&rest()),
        "drives" => cmd_drives(),
        "pause" => cmd_pause(&rest()),
        "resume" => cmd_resume(),
        "probe" => cmd_probe(),
        "ask" => cmd_ask(&rest()),
        "key" => cmd_key(&rest()),
        "help" | "--help" | "-h" => {
            print!("{}", usage());
            Ok(())
        }
        other => Err(format!("comando desconocido: {other}\n\n{}", usage())),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
