//! Prueba de punta a punta del IPC: servidor + cliente por ventana Win32 real.
//!
//! Acá no se simula nada del transporte: `server::serve` registra la clase
//! `CENSUS_IPC`, crea la ventana y corre su bucle de mensajes en un hilo;
//! `client::Client` encuentra esa ventana con `FindWindowW` y le manda el
//! `QUERY2W` por `WM_COPYDATA`. Lo único falso es el motor: un `Searcher` con
//! un indice fijo en memoria. No se toca disco, ni MFT, ni elevacion.
//!
//! Todo (casos 1 a 6) vive en UN SOLO `#[test]` a proposito: `serve` guarda el
//! `Searcher` en un slot global del proceso y `RegisterClassW` es global
//! tambien. Partir los casos en varios tests los haria pisarse entre si.

#![cfg(windows)]

use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::Arc;
use std::time::{Duration, Instant};

use censusindex_ipc::client::Client;
use censusindex_ipc::protocol::{item, parse_reply, req, sort, Reply, ReplyItem};
use censusindex_ipc::server::{serve, Searcher, CLASS};

/// Clase libre para el caso 7: no la usa ni CensusIndex ni el Everything real.
const CUSTOM_CLASS: &str = "CENSUS_IPC_TEST_X";

/// FILETIME de referencia: fechas fijas, cero reloj del sistema.
const MOD0: u64 = 133_000_000_000_000_000;
const ACC0: u64 = 132_000_000_000_000_000;

#[derive(Clone, Copy)]
struct Row {
    name: &'static str,
    path: &'static str,
    size: Option<u64>,
    is_dir: bool,
    is_drive: bool,
}

/// El indice entero, ya en orden `NAME_ASC`. Una unidad, dos carpetas, tres
/// archivos (el del medio con ñ, para que el UTF-16 viaje de punta a punta).
const ROWS: &[Row] = &[
    Row {
        name: "C:",
        path: "",
        size: None,
        is_dir: true,
        is_drive: true,
    },
    Row {
        name: "Users",
        path: "C:",
        size: None,
        is_dir: true,
        is_drive: false,
    },
    Row {
        name: "Docs",
        path: "C:\\Users\\Lucio",
        size: None,
        is_dir: true,
        is_drive: false,
    },
    Row {
        name: "a.txt",
        path: "C:\\Users\\Lucio\\Docs",
        size: Some(1_024),
        is_dir: false,
        is_drive: false,
    },
    Row {
        name: "ñandú.pdf",
        path: "C:\\Users\\Lucio\\Docs",
        size: Some(4_096),
        is_dir: false,
        is_drive: false,
    },
    Row {
        name: "zoo.csv",
        path: "C:\\Users\\Lucio\\Docs",
        size: Some(0),
        is_dir: false,
        is_drive: false,
    },
];

impl Row {
    /// Llena sólo los campos que el pedido pidio: el servidor enmascara `flags`
    /// antes de serializar, pero un motor de verdad tampoco paga por lo que no
    /// le solicitaron.
    fn materialize(&self, flags: u32) -> ReplyItem {
        let mut it = ReplyItem {
            is_dir: self.is_dir,
            is_drive: self.is_drive,
            ..Default::default()
        };
        if flags & req::NAME != 0 {
            it.name = self.name.to_string();
        }
        if flags & (req::PATH | req::FULL_PATH) != 0 {
            it.path = self.path.to_string();
        }
        if flags & req::SIZE != 0 {
            it.size = self.size;
        }
        if flags & req::DATE_MODIFIED != 0 {
            it.modified_ft = MOD0 + (self.name.chars().count() as u64);
        }
        if flags & req::DATE_ACCESSED != 0 {
            it.accessed_ft = ACC0;
        }
        if flags & req::ATTRIBUTES != 0 {
            it.attributes = if self.is_dir { 0x10 } else { 0x20 };
        }
        if flags & req::RUN_COUNT != 0 {
            it.run_count = 3;
        }
        it
    }
}

/// Motor deterministico: respeta busqueda, orden, `offset` y `max`.
struct FakeIndex;

impl Searcher for FakeIndex {
    fn is_ready(&self) -> bool {
        true
    }

    fn search(
        &self,
        search: &str,
        sort_kind: u32,
        offset: u32,
        max: u32,
        flags: u32,
    ) -> (u32, Vec<ReplyItem>) {
        let needle = search.trim().to_lowercase();
        let mut hits: Vec<&Row> = ROWS
            .iter()
            .filter(|r| {
                needle.is_empty()
                    || r.name.to_lowercase().contains(&needle)
                    || format!("{}\\{}", r.path, r.name)
                        .to_lowercase()
                        .contains(&needle)
            })
            .collect();
        if sort_kind == sort::NAME_DESC {
            hits.reverse();
        }
        let total = hits.len() as u32;
        let start = (offset as usize).min(hits.len());
        let room = hits.len() - start;
        let take = if max == 0 {
            room
        } else {
            (max as usize).min(room)
        };
        let items = hits[start..start + take]
            .iter()
            .map(|r| r.materialize(flags))
            .collect();
        (total, items)
    }
}

/// Items esperado de `ROWS` para un pedido `flags`, paginado completo.
fn expected(flags: u32, desc: bool) -> Vec<ReplyItem> {
    let mut v: Vec<ReplyItem> = ROWS.iter().map(|r| r.materialize(flags)).collect();
    if desc {
        v.reverse();
    }
    v
}

fn parse(raw: &[u8]) -> Reply {
    parse_reply(raw).expect("la respuesta del servidor deberia ser parseable")
}

#[test]
fn ipc_e2e_servidor_y_cliente_por_ventana() {
    // ------------------------------------------------------------------
    // Caso 1: antes de arrancar el servidor el cliente existe y no encuentra
    // un demonio de CensusIndex. Nada de esto puede paniquear.
    // ------------------------------------------------------------------
    let mut client = Client::new().expect("Client::new deberia crear su ventana de respuesta");
    assert!(
        !client.censusindex_is_running(),
        "la clase {CLASS} no deberia estar registrada todavia"
    );

    // `find_server` cae a la clase de compatibilidad cuando CensusIndex no esta, y el
    // Everything real registra esa clase en la maquina de cualquiera. Si hay uno
    // escuchando, "nadie contesta" no es una asercion que valga; lo que si vale
    // es que nada de esto paniquea y de que sigue sin haber un demonio de CensusIndex.
    // En una maquina limpia, en cambio, el cliente no tiene a quien escribirle.
    if client.is_server_running() {
        eprintln!("aviso: hay un IPC de compatibilidad (Everything real) escuchando; caso 1 en modo parcial");
        let _ = client.is_ready();
        let raw = client.query_raw("", sort::NAME_ASC, 0, 10, req::BASIC);
        assert!(
            !client.censusindex_is_running(),
            "lo que contesto no puede haber registrado {CLASS}"
        );
        if let Some(raw) = raw {
            let ajeno = parse(&raw);
            assert_ne!(
                ajeno.items,
                expected(req::BASIC, false),
                "antes de arrancar el servidor no puede responder el indice de CensusIndex"
            );
        }
    } else {
        assert!(!client.is_ready(), "sin CensusIndex no hay indice listo");
        assert!(
            client
                .query_raw("", sort::NAME_ASC, 0, 10, req::BASIC)
                .is_none(),
            "sin servidor la consulta no puede responder"
        );
    }

    // ------------------------------------------------------------------
    // Caso 2: el servidor arranca en otro hilo y corre su bucle ahi.
    // ------------------------------------------------------------------
    let (done_tx, done_rx) = mpsc::channel::<Result<String, String>>();
    std::thread::spawn(move || {
        let outcome = serve(Arc::new(FakeIndex), &[CLASS]);
        // En exito esto nunca se alcanza: `serve` bloquea en `GetMessageW`.
        let _ = done_tx.send(outcome);
    });

    // Caso 3: esperar (<= 5 s, durmiendo 10 ms por vuelta) a que la ventana este visible.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.censusindex_is_running() {
        if Instant::now() >= deadline {
            match done_rx.try_recv() {
                Ok(Err(e)) => panic!("serve devolvio Err antes de publicarse: {e}"),
                Ok(Ok(c)) => panic!("serve termino antes de la espera, devolvio {c}"),
                Err(_) => panic!("el servidor no registro la clase {CLASS} en 5 s"),
            }
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // Caso 4: el indice esta listo (WM_USER + IS_DB_LOADED responde distinto de 0).
    let deadline = Instant::now() + Duration::from_secs(5);
    while !client.is_ready() {
        if Instant::now() >= deadline {
            panic!("is_ready() nunca dio true");
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    // ------------------------------------------------------------------
    // Caso 5: paginacion offset 0 / 2 / 4 con max = 2.
    // ------------------------------------------------------------------
    let mut pages: Vec<Reply> = Vec::new();
    for offset in [0u32, 2, 4] {
        let raw = client
            .query_raw("", sort::NAME_ASC, offset, 2, req::BASIC)
            .unwrap_or_else(|| panic!("el servidor no contesto la pagina en offset {offset}"));
        let page = parse(&raw);
        assert_eq!(
            page.total,
            ROWS.len() as u32,
            "total constante en toda pagina (offset {offset})"
        );
        assert_eq!(
            page.offset, offset,
            "el servidor debe devolver el offset pedido"
        );
        assert_eq!(page.items.len(), 2, "pagina de 2 en offset {offset}");
        assert_eq!(
            page.flags,
            req::BASIC,
            "el servidor responde los flags del pedido"
        );
        pages.push(page);
    }

    let mut paged: Vec<ReplyItem> = Vec::new();
    for page in &pages {
        paged.extend(page.items.iter().cloned());
    }
    assert_eq!(paged.len(), ROWS.len());
    let mut vistos: Vec<(String, String)> = paged
        .iter()
        .map(|i| (i.name.clone(), i.path.clone()))
        .collect();
    vistos.sort();
    vistos.dedup();
    assert_eq!(
        vistos.len(),
        ROWS.len(),
        "ningun item puede repetirse entre paginas"
    );

    // El orden concatenado tiene que ser el de `query_all` con una pagina grande.
    let all = client
        .query_all("", sort::NAME_ASC, 100)
        .expect("query_all deberia agotar total");
    assert_eq!(all.total, ROWS.len() as u32);
    assert_eq!(
        all.items.len(),
        ROWS.len(),
        "query_all no puede dejar items en el tintero"
    );
    assert_eq!(
        paged, all.items,
        "paginar de a 2 y concatenar debe dar el mismo orden"
    );
    assert_eq!(
        all.items,
        expected(req::BASIC, false),
        "el indice debe salir en NAME_ASC"
    );

    // Caso 5 (espejo): el orden inverso tambien cierra por la misma via.
    let all_desc = client
        .query_all("", sort::NAME_DESC, 2)
        .expect("query_all descendente");
    assert_eq!(all_desc.items, expected(req::BASIC, true));

    // ------------------------------------------------------------------
    // Caso 6: semantica de cada tipo de item.
    // ------------------------------------------------------------------
    let drive = all
        .items
        .iter()
        .find(|i| i.is_drive)
        .expect("deberia haber una unidad");
    assert_eq!(drive.name, "C:");
    assert!(drive.is_dir, "una unidad es un nodo de directorio");
    assert_eq!(drive.flags(), item::DRIVE);
    assert_eq!(drive.size, None, "la unidad no tiene tamano conocido");

    let file = all
        .items
        .iter()
        .find(|i| i.name == "ñandú.pdf")
        .expect("deberia estar el archivo");
    assert!(!file.is_dir, "un archivo no es directorio");
    assert!(!file.is_drive);
    assert_eq!(file.flags(), 0);
    assert_eq!(file.size, Some(4_096));
    assert_eq!(file.path, "C:\\Users\\Lucio\\Docs");
    assert!(
        file.modified_ft > 0,
        "la fecha de modificacion fija debe viajar"
    );
    assert_eq!(
        file.accessed_ft, 0,
        "DATE_ACCESSED no se pidio, no puede venir"
    );

    let folder = all
        .items
        .iter()
        .find(|i| i.name == "Docs")
        .expect("deberia estar la carpeta");
    assert!(folder.is_dir && !folder.is_drive);
    assert_eq!(folder.flags(), item::FOLDER);
    assert_eq!(
        folder.size, None,
        "las carpetas viajan con tamano desconocido"
    );

    // ------------------------------------------------------------------
    // Extras de contrato: filtro, y que el servidor recorte los campos que el
    // motor no sabe completar (flags -> bytes que viajan).
    // ------------------------------------------------------------------
    let solo = parse(
        &client
            .query_raw("Docs", sort::NAME_ASC, 0, 10, req::BASIC)
            .expect("filtro por carpeta"),
    );
    assert_eq!(
        solo.total, 4,
        "el filtro debe recortar el total: la carpeta y sus tres archivos"
    );
    assert_eq!(solo.items.len(), 4);
    assert!(solo
        .items
        .iter()
        .all(|i| i.path.contains("Docs") || i.name.contains("Docs")));
    let vacio = parse(
        &client
            .query_raw("zz_no_existe", sort::NAME_ASC, 0, 10, req::BASIC)
            .expect("filtro vacio"),
    );
    assert_eq!(
        vacio.total, 0,
        "una busqueda sin coincidencias no puede inventar items"
    );
    assert!(vacio.items.is_empty());

    let uni = parse(
        &client
            .query_raw(".pdf", sort::NAME_ASC, 0, 10, req::BASIC)
            .expect("filtro por extension"),
    );
    assert_eq!(uni.total, 1);
    assert_eq!(
        uni.items[0].name, "ñandú.pdf",
        "el nombre con ñ sobrevive el viaje UTF-16"
    );

    let name_only = parse(
        &client
            .query_raw("", sort::NAME_ASC, 0, 2, req::NAME)
            .expect("pagina con NAME"),
    );
    assert_eq!(name_only.flags, req::NAME);
    assert_eq!(name_only.items.len(), 2);
    assert!(
        name_only.items.iter().all(|i| !i.name.is_empty()),
        "NAME debe traer el nombre"
    );
    assert!(
        name_only
            .items
            .iter()
            .all(|i| i.path.is_empty() && i.size.is_none()),
        "no se paga lo que no se pidio"
    );

    let off_end = parse(
        &client
            .query_raw("", sort::NAME_ASC, ROWS.len() as u32, 2, req::BASIC)
            .expect("offset final"),
    );
    assert_eq!(off_end.total, ROWS.len() as u32);
    assert!(
        off_end.items.is_empty(),
        "offset == total debe dar pagina vacia"
    );

    // Y el servidor sigue ahi, bloqueado a proposito: si `serve` hubiera
    // vuelto, es que su bucle de mensajes se cayo.
    assert!(
        matches!(
            done_rx.recv_timeout(Duration::from_millis(200)),
            Err(RecvTimeoutError::Timeout)
        ),
        "serve debe seguir en su bucle de mensajes"
    );
}

/// Caso 7: `serve` con una clase custom. Va en su propio `#[test]` porque toca
/// el registro global de clases, y el hilo **queda bloqueado a proposito**:
/// `serve` solo devuelve cuando termina su bucle de mensajes, y aca nadie le
/// manda `WM_DESTROY`. Al terminar el proceso de test, Windows destruye la
/// ventana y la clase; no hay nada que limpiar.
///
/// Como el `Ok(nombre)` es inalcanzable mientras el servidor corre, la prueba
/// es por descarte y es a prueba de carreras: se lanzan DOS `serve` con la
/// misma clase custom. `RegisterClassW` es por proceso, asi que exactamente uno
/// gana el registro y se queda bloqueado (ese es el camino que devolveria
/// `Ok("CENSUS_IPC_TEST_X")`, porque `serve` devuelve el nombre de la clase con
/// la que creó la ventana), y el otro tiene que volver de inmediato con un
/// `Err` que menciona la clase. Si los dos volvieran, la clase no es libre y el
/// test lo dice en vez de mentir.
#[test]
fn serve_con_clase_custom_usa_ese_nombre() {
    let (tx, rx) = mpsc::channel::<Result<String, String>>();
    for _ in 0..2 {
        let tx = tx.clone();
        std::thread::spawn(move || tx.send(serve(Arc::new(FakeIndex), &[CUSTOM_CLASS])).is_ok());
    }
    drop(tx);

    // Uno de los dos no puede responder nunca: es el que registró y entró al bucle.
    let first = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("ningun serve devolvio: el registro de la clase custom no progresa");
    let Err(msg) = first else {
        panic!("serve devolvio Ok({first:?}) sin estar en el bucle, no deberia ser alcanzable");
    };
    assert!(
        msg.contains(CUSTOM_CLASS),
        "el serve perdedor deberia quejarse de la clase {CUSTOM_CLASS}, dijo: {msg}"
    );

    match rx.recv_timeout(Duration::from_secs(3)) {
        Err(RecvTimeoutError::Timeout) => {}
        Err(e @ RecvTimeoutError::Disconnected) => panic!("el ganador termino el hilo: {e}"),
        Ok(Ok(c)) => panic!(
            "los dos serve devolvieron Ok({c}): el registro de {CUSTOM_CLASS} no es exclusivo"
        ),
        Ok(Err(e)) => {
            panic!("los dos serve fallaron ({e}): la clase {CUSTOM_CLASS} no estaba libre")
        }
    }
}
