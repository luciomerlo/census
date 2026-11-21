//! Robustez de los parsers del IPC: bytes hostiles no deben hacer panic.
//!
//! No es un fuzzer real (no hace coverage), pero corre decenas de miles de
//! entradas aleatorias y algunos casos límite construidos a mano, que es donde
//! se esconden los `slice index out of range` de los parsers binarios.

use census_ipc::protocol::{self, build_reply, parse_reply, parse_request, ReplyItem};

/// xorshift64: determinista y suficiente para esto.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn bytes(&mut self, max: usize) -> Vec<u8> {
        let n = (self.next() as usize) % (max + 1);
        (0..n).map(|_| self.next() as u8).collect()
    }
}

#[test]
fn random_bytes_never_panic_in_the_parsers() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for _ in 0..20_000 {
        let buf = rng.bytes(512);
        let _ = parse_request(&buf);
        let _ = parse_reply(&buf);
    }
}

#[test]
fn hostile_headers_are_rejected_not_trusted() {
    // Cabecera sola, sin una sola fila detrás, con cantidades absurdas.
    let cases = [
        (u32::MAX, u32::MAX, u32::MAX, u32::MAX),
        (0, 1, 0, 0),
        (1_000_000, 0, 5, 0),
        (0, 1_000_000, 0, u32::MAX),
        (7, 1, 0, 0),
    ];
    for (total, num, offset, flags) in cases {
        let mut buf = Vec::new();
        buf.extend_from_slice(&total.to_le_bytes());
        buf.extend_from_slice(&num.to_le_bytes());
        buf.extend_from_slice(&offset.to_le_bytes());
        buf.extend_from_slice(&flags.to_le_bytes());
        // Un parser correcto devuelve None o algo vacío, nunca panic.
        match parse_reply(&buf) {
            None => {}
            Some(r) => assert!(r.items.is_empty(), "no puede inventar filas sin datos"),
        }
    }
}

#[test]
fn truncated_reply_with_valid_header_does_not_panic() {
    let items = vec![
        ReplyItem {
            name: "uno.txt".into(),
            path: "C:\\".into(),
            size: Some(10),
            modified_ft: 1,
            accessed_ft: 2,
            attributes: 0x20,
            run_count: 0,
            is_dir: false,
            is_drive: false,
            identity: None,
        },
        ReplyItem {
            name: "dos".into(),
            path: "C:\\tmp".into(),
            size: None,
            modified_ft: 3,
            accessed_ft: 4,
            attributes: 0x10,
            run_count: 0,
            is_dir: true,
            is_drive: false,
            identity: None,
        },
    ];
    let full = build_reply(2, 0, protocol::req::BASIC, &items);
    // Cada truncado posible tiene que ser seguro.
    for cut in 0..full.len() {
        let _ = parse_reply(&full[..cut]);
    }
    let r = parse_reply(&full).expect("la respuesta completa se parsea");
    assert_eq!(r.total, 2);
    assert_eq!(r.items.len(), 2);
    assert_eq!(r.items[0].name, "uno.txt");
}

#[test]
fn random_requests_never_panic() {
    // El pedido es más chico: cabecera fija + búsqueda UTF-16.
    let mut rng = Rng(0xdead_beef_0bad_f00d);
    for _ in 0..5_000 {
        let buf = rng.bytes(256);
        if let Some(req) = parse_request(&buf) {
            // Si lo aceptó, los campos tienen que ser coherentes.
            assert!(req.search.len() <= 1024);
        }
    }
}
