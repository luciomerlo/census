//! Conformidad byte a byte del protocolo QUERY2W.
//!
//! Esto es el contrato con las apps que ya existen: si un test de acá se rompe,
//! WizEverything o SortEverything dejan de ver resultados. Los offsets no son
//! negociables.

use census_ipc::protocol::{self, item, req, sort, ReplyItem, Request};

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn it(name: &str, path: &str, size: Option<u64>, dir: bool) -> ReplyItem {
    ReplyItem {
        name: name.into(),
        path: path.into(),
        size,
        modified_ft: 133_000_000_000_000_000,
        accessed_ft: 132_000_000_000_000_000,
        attributes: 0x20,
        run_count: 3,
        is_dir: dir,
        is_drive: false,
    }
}

#[test]
fn header_is_20_bytes_and_rows_start_right_after() {
    let items = vec![
        it("a.txt", "C:\\x", Some(1), false),
        it("b.txt", "C:\\x", Some(2), false),
    ];
    let raw = protocol::build_reply(7, 0, req::BASIC, &items);

    assert_eq!(protocol::HEADER_LEN, 20);
    assert_eq!(u32_at(&raw, 0), 7); // total
    assert_eq!(u32_at(&raw, 4), 2); // num
    assert_eq!(u32_at(&raw, 12), req::BASIC); // flags

    // Cada fila: 8 bytes. La fila i arranca en 20 + 8*i.
    for (i, _) in (0..2).enumerate() {
        let flags = u32_at(&raw, 20 + i * 8);
        let off = u32_at(&raw, 20 + i * 8 + 4) as usize;
        assert_eq!(flags, 0, "archivo normal no lleva flags");
        assert!(off >= 20 + 2 * 8, "los datos nunca pisan la tabla");
        // En `off` empieza el largo del nombre, y el nombre 4 bytes despues.
        let declared = u32_at(&raw, off) as usize;
        let expected = items[i].name.encode_utf16().count();
        assert_eq!(declared, expected);
        let name: Vec<u16> = (0..declared)
            .map(|k| u16::from_le_bytes([raw[off + 4 + 2 * k], raw[off + 5 + 2 * k]]))
            .collect();
        assert_eq!(String::from_utf16(&name).unwrap(), items[i].name);
    }
}

#[test]
fn first_name_starts_after_table_plus_length_prefix() {
    let items = vec![
        it("uno", "C:", Some(1), false),
        it("dos", "C:", Some(2), false),
    ];
    let raw = protocol::build_reply(2, 0, req::NAME | req::PATH | req::SIZE, &items);
    // El offset de la fila apunta al prefijo de largo del nombre...
    let first = u32_at(&raw, 20 + 4) as usize;
    assert_eq!(first, 20 + 2 * 8, "20 de cabecera + 16 de tabla");
    // ...y el primer caracter UTF-16 empieza 4 bytes despues, despues de los
    // dos strings (nombre y carpeta) del item 0.
    let declared = u32_at(&raw, first) as usize;
    assert_eq!(declared, items[0].name.encode_utf16().count());
    let second = first + 4 + declared * 2 + 2;
    assert_eq!(
        u32_at(&raw, second) as usize,
        items[0].path.encode_utf16().count()
    );
    let sizes_at = second + 4 + items[0].path.encode_utf16().count() * 2 + 2;
    assert_eq!(u64_at(&raw, sizes_at), 1);
}

#[test]
fn only_requested_fields_travel() {
    let items = vec![it("solo-nombre", "C:\\x", Some(9), false)];

    let raw = protocol::build_reply(1, 0, req::NAME, &items);
    let back = protocol::parse_reply(&raw).unwrap();
    assert_eq!(back.items[0].name, "solo-nombre");
    assert_eq!(back.items[0].path, "");
    assert_eq!(back.items[0].size, None);
    assert_eq!(back.items[0].modified_ft, 0);
    // nombre: 4 + 2*len + 2
    assert_eq!(
        raw.len(),
        20 + 8 + 4 + "solo-nombre".encode_utf16().count() * 2 + 2
    );

    let raw = protocol::build_reply(1, 0, req::SIZE, &items);
    let back = protocol::parse_reply(&raw).unwrap();
    assert_eq!(back.items[0].size, Some(9));
    assert_eq!(back.items[0].name, "");
    assert_eq!(raw.len(), 20 + 8 + 8);

    let raw = protocol::build_reply(1, 0, req::DATE_MODIFIED, &items);
    let back = protocol::parse_reply(&raw).unwrap();
    assert_eq!(back.items[0].modified_ft, items[0].modified_ft);
    assert_eq!(back.items[0].accessed_ft, 0);
}

#[test]
fn unknown_size_is_a_roundtrip() {
    let folder = it("carpeta", "C:\\", None, true);
    let raw = protocol::build_reply(1, 0, req::SIZE, std::slice::from_ref(&folder));
    assert_eq!(u64_at(&raw, 20 + 8), protocol::UNKNOWN_SIZE);
    assert_eq!(protocol::parse_reply(&raw).unwrap().items[0].size, None);

    let file = it("archivo", "C:\\", Some(0), false);
    let raw = protocol::build_reply(1, 0, req::SIZE, std::slice::from_ref(&file));
    assert_eq!(protocol::parse_reply(&raw).unwrap().items[0].size, Some(0));
}

#[test]
fn unicode_names_roundtrip() {
    let names = [
        "ñandú.txt",
        "中文文件.doc",
        "🚀 lanzamiento.pdf",
        "",
        &"x".repeat(300),
    ];
    for n in names {
        let items = vec![it(n, "C:\\carpeta", Some(1), false)];
        let raw = protocol::build_reply(1, 0, req::BASIC, &items);
        let back = protocol::parse_reply(&raw).unwrap();
        assert_eq!(back.items[0].name, n, "nombre {n:?}");
    }
}

#[test]
fn item_flags_mark_folders_and_drives() {
    let mut folder = it("carpeta", "C:\\", None, true);
    let mut drive = it("C:", "", None, true);
    drive.is_drive = true;
    let raw = protocol::build_reply(2, 0, req::NAME, &[folder.clone(), drive.clone()]);
    assert_eq!(u32_at(&raw, 20), item::FOLDER);
    assert_eq!(u32_at(&raw, 28), item::DRIVE);

    let back = protocol::parse_reply(&raw).unwrap();
    assert!(back.items[0].is_dir && !back.items[0].is_drive);
    assert!(back.items[1].is_drive);

    folder.is_dir = false;
    folder.is_drive = true;
    assert_eq!(folder.flags(), item::DRIVE, "una unidad gana sobre carpeta");
}

#[test]
fn truncated_replies_return_none_instead_of_panicking() {
    let items = vec![it("informe.pdf", "C:\\Users", Some(4096), false)];
    let raw = protocol::build_reply(1, 0, req::BASIC, &items);

    assert!(protocol::parse_reply(&[0u8; 19]).is_none());
    assert!(
        protocol::parse_reply(&raw[..8]).is_none(),
        "corta antes de la tabla"
    );
    assert!(
        protocol::parse_reply(&raw[..24]).is_none(),
        "corta dentro de la tabla"
    );
    assert!(
        protocol::parse_reply(&raw[..raw.len() - 2]).is_none(),
        "sin el NUL final"
    );
    assert!(
        protocol::parse_reply(&raw[..raw.len() - 4]).is_none(),
        "sin el largo de la cadena"
    );
    assert!(protocol::parse_reply(&[]).is_none());
}

#[test]
fn requests_accept_the_three_shapes_clients_send() {
    // 1) search_len correcto
    let r = Request {
        reply_hwnd: 0x1234,
        reply_msg: 0x5745_5A31,
        offset: 0,
        max: 250_000,
        flags: req::BASIC,
        sort: sort::NAME_ASC,
        search: "path:\"C:\\Users\\Lucio\" file:".into(),
    };
    assert_eq!(
        protocol::parse_request(&protocol::build_request(&r)).unwrap(),
        r
    );

    // 2) search_len en 0 (lo que mandan los clientes reales)
    let mut buf = protocol::build_request(&Request {
        search: "file: dm:today".into(),
        ..Default::default()
    });
    buf[8..12].copy_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        protocol::parse_request(&buf).unwrap().search,
        "file: dm:today"
    );

    // 3) search_len mentiroso (mas grande que el buffer): gana el terminador
    let mut buf = protocol::build_request(&Request {
        search: "ext:log".into(),
        ..Default::default()
    });
    buf[8..12].copy_from_slice(&9999u32.to_le_bytes());
    assert_eq!(protocol::parse_request(&buf).unwrap().search, "ext:log");
}

#[test]
fn malformed_requests_return_none() {
    assert!(protocol::parse_request(&[]).is_none());
    assert!(protocol::parse_request(&[0u8; 27]).is_none());
    // 28 bytes de cabecera y un byte impar sin terminador: no hay cadena.
    let mut odd = vec![0u8; 29];
    odd[8..12].copy_from_slice(&0u32.to_le_bytes());
    let r = protocol::parse_request(&odd).unwrap();
    assert_eq!(r.search, "");
}

#[test]
fn limits_do_not_panic() {
    let large = Request {
        reply_hwnd: u32::MAX,
        reply_msg: u32::MAX,
        offset: u32::MAX,
        max: u32::MAX,
        flags: 0xFFFF_FFFF,
        sort: u32::MAX,
        search: "a".repeat(5000),
    };
    let back = protocol::parse_request(&protocol::build_request(&large)).unwrap();
    assert_eq!(back.search.len(), 5000);
    assert_eq!(back.max, u32::MAX);

    let raw = protocol::build_reply(
        u32::MAX,
        u32::MAX,
        req::BASIC,
        &[it("x", "C:", Some(1), false)],
    );
    let reply = protocol::parse_reply(&raw).unwrap();
    assert_eq!(reply.total, u32::MAX);
    assert_eq!(reply.offset, u32::MAX);

    // Flags desconocidos: se serializan como esten, pero no rompen el parser.
    let raw = protocol::build_reply(1, 0, 0x8000_0000, &[it("x", "C:", Some(1), false)]);
    assert!(protocol::parse_reply(&raw).is_some());
}

#[test]
fn full_page_roundtrip() {
    let items: Vec<ReplyItem> = (0..500)
        .map(|i| {
            let name = format!("archivo-{i:04}-ñ.txt");
            let path = format!("C:\\carpeta\\sub{i}");
            it(&name, &path, Some(i as u64 * 1024), i % 7 == 0)
        })
        .collect();
    let flags = req::BASIC | req::DATE_ACCESSED | req::ATTRIBUTES | req::RUN_COUNT;
    let raw = protocol::build_reply(items.len() as u32, 1234, flags, &items);
    let back = protocol::parse_reply(&raw).unwrap();
    assert_eq!(back.total, 500);
    assert_eq!(back.offset, 1234);
    assert_eq!(back.flags, flags);
    assert_eq!(back.items, items);
}

#[test]
fn serialization_is_deterministic() {
    let items: Vec<ReplyItem> = (0..50)
        .map(|i| it(&format!("n{i}"), "C:\\x", Some(i), false))
        .collect();
    let a = protocol::build_reply(50, 0, req::BASIC, &items);
    let b = protocol::build_reply(50, 0, req::BASIC, &items);
    assert_eq!(a, b);

    let r = Request {
        search: "path:\"C:\\a b\"".into(),
        ..Default::default()
    };
    assert_eq!(protocol::build_request(&r), protocol::build_request(&r));
}

#[test]
fn empty_page_is_a_valid_reply() {
    let raw = protocol::build_reply(0, 0, req::BASIC, &[]);
    assert_eq!(raw.len(), 20);
    let back = protocol::parse_reply(&raw).unwrap();
    assert_eq!(back.total, 0);
    assert!(back.items.is_empty());
}

// ---------------------------------------------------------------- capacidades

#[test]
fn reply_flags_only_announce_present_fields() {
    use census_ipc::protocol::{caps, req};
    // Pedir run_count no hace que venga: no lo tenemos.
    let asked = req::BASIC | req::RUN_COUNT;
    let present = caps::present(asked);
    assert_eq!(present, req::BASIC);
    assert!(!caps::has(present, req::RUN_COUNT));
    // Lo que sí tenemos se conserva.
    assert!(caps::has(present, req::NAME));
    assert!(caps::has(present, req::SIZE));
    // Y si piden de más (accessed, attributes), también viaja.
    let full = caps::present(req::BASIC | req::DATE_ACCESSED | req::ATTRIBUTES | req::RUN_COUNT);
    assert!(caps::has(full, req::DATE_ACCESSED));
    assert!(caps::has(full, req::ATTRIBUTES));
    assert!(!caps::has(full, req::RUN_COUNT));
}

#[test]
fn capabilities_flag_partial_metadata() {
    use census_ipc::protocol::caps;
    assert!(
        caps::has(caps::DEFAULT, caps::PARTIAL_METADATA),
        "Census debe avisar que los metadatos son parciales"
    );
    assert!(caps::has(
        caps::DEFAULT,
        census_ipc::protocol::req::DATE_ACCESSED
    ));
    assert!(!caps::has(
        caps::DEFAULT,
        census_ipc::protocol::req::RUN_COUNT
    ));
    // Máscara de mensaje estable: WM_USER + 402.
    assert_eq!(census_ipc::protocol::CAPS, 402);
}

#[test]
fn messages_a_medium_client_needs_are_filtered() {
    // Sin estos tres filtros, un cliente sin elevación no puede hablar con
    // `census serve` elevado (UIPI devuelve error 5). Ver docs/WIZE-INTEGRATION.md.
    use census_ipc::protocol::{CAPS, IS_DB_LOADED};
    let allowed = census_ipc::server::ALLOWED_FROM_LOWER_INTEGRITY;
    assert_eq!(allowed.len(), 3);
    assert!(allowed.contains(&census_ipc::protocol::WM_COPYDATA));
    assert!(allowed.contains(&(0x0400 + IS_DB_LOADED)));
    assert!(allowed.contains(&(0x0400 + CAPS)));
}
