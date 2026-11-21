//! Prueba de punta a punta del servidor MCP de Census sobre stdio.
//!
//! Lanza el binario `census mcp --root <dir>` como proceso hijo, le habla en
//! JSON-RPC 2.0 (una línea por mensaje, respuestas por stdout) y verifica el
//! protocolo completo: initialize, notifications/initialized (sin respuesta),
//! tools/list, las herramientas search/count/largest/stats, método
//! desconocido (-32601) y JSON inválido (-32700).
//!
//! Nota: `Backend::open` prefiere un demonio Census por IPC si hay uno
//! corriendo en la máquina. Esta prueba asume el backend local (backend ==
//! "local"); si `census serve` está activo, el arranque irá por IPC y el
//! chequeo de `stats` no se cumplirá (se ve por stderr: "backend: demonio").

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Cuánto esperamos cada respuesta (indexar el directorio temporal es rápido).
const LINE_TIMEOUT: Duration = Duration::from_secs(15);
/// Cuánto esperamos ruido tras una notificación que no debe responder.
const NOTIFY_QUIET: Duration = Duration::from_millis(500);
/// Cuánto esperamos la salida del proceso tras cerrar stdin.
const EXIT_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------- fixture --

/// Borra el directorio temporal al terminar la prueba (también en panic).
struct Fixture {
    root: PathBuf,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Best effort: si algo retiene los archivos, no enmascaramos el test.
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn build_fixture() -> (Fixture, Vec<FileSpec>) {
    let root = std::env::temp_dir().join(format!("census-mcp-test-{}", std::process::id()));
    // Restos de una corrida previa abortada: empezar limpio.
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub1")).expect("crear sub1");
    std::fs::create_dir_all(root.join("sub2")).expect("crear sub2");

    // 5 archivos: nombres, tamaños y extensiones todos distintos, para que
    // las verificaciones de búsqueda, conteo y "largest" no sean ambiguas.
    let files = [
        FileSpec { rel: "sub1/alpha.txt", size: 100 },
        FileSpec { rel: "sub1/delta.log", size: 200 },
        FileSpec { rel: "sub2/beta.txt", size: 500 },
        FileSpec { rel: "sub2/gamma.md", size: 1_000 }, // el más grande
        FileSpec { rel: "epsilon.dat", size: 50 },
    ];
    for f in &files {
        let mut bytes = Vec::with_capacity(f.size as usize);
        for i in 0..f.size {
            bytes.push((i % 251) as u8 ^ f.size as u8);
        }
        std::fs::write(root.join(f.rel), &bytes)
            .unwrap_or_else(|e| panic!("escribir {}: {e}", f.rel));
    }

    (Fixture { root }, files.to_vec())
}

#[derive(Clone, Copy)]
struct FileSpec {
    rel: &'static str,
    size: u64,
}

// ------------------------------------------------------------------ server --

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<String>,
}

impl Server {
    fn spawn(root: &Path) -> Server {
        let mut child = Command::new(env!("CARGO_BIN_EXE_census"))
            .args(["mcp", "--root", &root.to_string_lossy()])
            // Fuerza el indice propio: el test no depende de que haya un
            // demonio Census escuchando en la maquina.
            .env("CENSUS_MCP_LOCAL", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Los diagnósticos van por stderr; los heredamos para que se vean
            // en la salida del test si algo falla.
            .stderr(Stdio::inherit())
            .spawn()
            .expect("no se pudo lanzar `census mcp`");
        let stdout = child.stdout.take().expect("stdout piped");
        let stdin = child.stdin.take().expect("stdin piped");

        // Hilo lector + canal: permite leer una línea con timeout sin
        // bloquear el test para siempre.
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Server { child, stdin: Some(stdin), rx }
    }

    fn send(&mut self, value: &Value) {
        self.send_raw(&value.to_string());
    }

    fn send_raw(&mut self, line: &str) {
        let stdin = self.stdin.as_mut().expect("stdin sigue abierto");
        writeln!(stdin, "{line}").expect("escribir al stdin del servidor");
        stdin.flush().expect("flush del stdin del servidor");
    }

    /// Lee UNA línea de stdout (con timeout) y la parsea como JSON.
    fn read_reply(&self, what: &str) -> Value {
        let line = match self.rx.recv_timeout(LINE_TIMEOUT) {
            Ok(l) => l,
            Err(RecvTimeoutError::Timeout) => {
                panic!("timeout esperando la respuesta a {what}")
            }
            Err(RecvTimeoutError::Disconnected) => {
                panic!("el servidor cerró stdout antes de responder a {what}")
            }
        };
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("stdout no es JSON ({e}): {line:?}"))
    }

    /// Verifica que NO llegue ninguna línea (una notificación no responde).
    fn expect_silence(&self, what: &str) {
        match self.rx.recv_timeout(NOTIFY_QUIET) {
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                panic!("el servidor cerró stdout durante {what}")
            }
            Ok(line) => panic!("{what} produjo una respuesta inesperada: {line:?}"),
        }
    }

    /// Cierra stdin y exige que el proceso termine en EXIT_TIMEOUT.
    fn shutdown(mut self) {
        drop(self.stdin.take()); // EOF => el bucle de serve_stdio termina
        let deadline = Instant::now() + EXIT_TIMEOUT;
        loop {
            match self.child.try_wait().expect("consultar estado del proceso") {
                Some(status) => {
                    assert!(
                        status.success(),
                        "el servidor MCP terminó con estado anormal: {status}"
                    );
                    return;
                }
                None if Instant::now() >= deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    panic!(
                        "el servidor MCP no terminó en {} s después de cerrar stdin; lo matamos",
                        EXIT_TIMEOUT.as_secs()
                    );
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    }
}

// -------------------------------------------------------------------- test --

#[test]
fn mcp_stdio_end_to_end() {
    let (_fx, files) = build_fixture();
    // _fx limpia el directorio temporal al salir (Drop), incluso en panic.
    let root = std::env::temp_dir().join(format!("census-mcp-test-{}", std::process::id()));
    let mut srv = Server::spawn(&root);

    // ---- initialize ------------------------------------------------------
    srv.send(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {"protocolVersion": "2024-11-05"}
    }));
    let r = srv.read_reply("initialize");
    assert_eq!(r["jsonrpc"], "2.0", "respuesta initialize: {r}");
    assert_eq!(r["id"], 1, "respuesta initialize: {r}");
    assert!(
        r["result"]["protocolVersion"] == "2024-11-05",
        "protocolVersion no ecoado: {r}"
    );
    assert_eq!(
        r["result"]["serverInfo"]["name"],
        "census",
        "serverInfo.name: {r}"
    );
    assert!(
        r["result"]["capabilities"]["tools"].is_object(),
        "capabilities.tools no existe: {r}"
    );

    // ---- notifications/initialized: sin id => sin respuesta ---------------
    srv.send(&json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    }));
    srv.expect_silence("notifications/initialized");

    // ---- tools/list -------------------------------------------------------
    srv.send(&json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
    let r = srv.read_reply("tools/list");
    assert_eq!(r["id"], 2, "respuesta tools/list: {r}");
    let tools = r["result"]["tools"]
        .as_array()
        .expect("tools/list sin tools: {r}");
    let names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("tool sin name: {t}"))
        .collect();
    assert_eq!(names, ["search", "count", "largest", "recent", "stats"]);
    for t in tools {
        assert_eq!(
            t["inputSchema"]["type"],
            "object",
            "inputSchema.type de {}: {t}",
            t["name"]
        );
    }

    // ---- tools/call search ------------------------------------------------
    srv.send(&json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {
            "name": "search",
            "arguments": {"query": "file: ext:txt", "limit": 10}
        }
    }));
    let r = srv.read_reply("search");
    assert_eq!(r["id"], 3, "respuesta search: {r}");
    assert_eq!(r["result"]["isError"], false, "search marcó error: {r}");
    let text = r["result"]["content"][0]["text"]
        .as_str()
        .expect("search sin texto: {r}");
    assert!(text.contains("resultados"), "search sin 'resultados': {text:?}");
    let items = r["result"]["structuredContent"]["items"]
        .as_array()
        .expect("search sin structuredContent.items: {r}");
    let mut hit_names: Vec<&str> = items
        .iter()
        .map(|it| it["name"].as_str().expect("item sin name: {it}"))
        .collect();
    hit_names.sort_unstable();
    assert_eq!(
        hit_names,
        ["alpha.txt", "beta.txt"],
        "sólo los .txt deben aparecer: {items:?}"
    );

    // ---- tools/call count -------------------------------------------------
    srv.send(&json!({
        "jsonrpc": "2.0",
        "id": 4,
        "method": "tools/call",
        "params": {"name": "count", "arguments": {"query": "file:"}}
    }));
    let r = srv.read_reply("count");
    assert_eq!(r["id"], 4, "respuesta count: {r}");
    assert_eq!(
        r["result"]["structuredContent"]["total"],
        files.len() as u64,
        "count no coincide con los archivos creados: {r}"
    );

    // ---- tools/call largest -----------------------------------------------
    srv.send(&json!({
        "jsonrpc": "2.0",
        "id": 5,
        "method": "tools/call",
        "params": {"name": "largest", "arguments": {}}
    }));
    let r = srv.read_reply("largest");
    assert_eq!(r["id"], 5, "respuesta largest: {r}");
    let ranked = r["result"]["structuredContent"]
        .as_array()
        .expect("largest sin structuredContent: {r}");
    assert_eq!(
        ranked[0]["name"],
        "gamma.md",
        "el más grande (gamma.md, 1000 B) debe ir primero: {ranked:?}"
    );
    assert_eq!(ranked[0]["size"], 1_000, "tamaño del más grande: {ranked:?}");

    // ---- tools/call stats -------------------------------------------------
    srv.send(&json!({
        "jsonrpc": "2.0",
        "id": 6,
        "method": "tools/call",
        "params": {"name": "stats", "arguments": {}}
    }));
    let r = srv.read_reply("stats");
    assert_eq!(r["id"], 6, "respuesta stats: {r}");
    assert_eq!(
        r["result"]["structuredContent"]["backend"],
        "local",
        "stats debería reportar el backend local: {r}"
    );

    // ---- método desconocido ------------------------------------------------
    srv.send(&json!({"jsonrpc": "2.0", "id": 7, "method": "no/existe"}));
    let r = srv.read_reply("metodo desconocido");
    assert_eq!(r["id"], 7, "respuesta metodo desconocido: {r}");
    assert_eq!(
        r["error"]["code"],
        -32601,
        "método desconocido debe dar -32601: {r}"
    );

    // ---- JSON inválido ------------------------------------------------------
    srv.send_raw("{\"jsonrpc\": \"2.0\", \"id\": 8, \"method\": ");
    let r = srv.read_reply("json invalido");
    assert_eq!(
        r["error"]["code"],
        -32700,
        "JSON inválido debe dar -32700: {r}"
    );
    assert!(
        r["id"].is_null(),
        "respuesta a JSON inválido debe llevar id null: {r}"
    );

    // ---- cierre -------------------------------------------------------------
    srv.shutdown();
}
