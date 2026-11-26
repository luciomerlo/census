//! Robustez del núcleo: nada de lo que entra por disco o por teclado puede
//! hacer panic. Cubre el snapshot `C.wcat`, el parser de consultas y la
//! resolución de rutas, que son las tres puertas de entrada de datos externos.

use std::path::PathBuf;

use censusindex::{query, snapshot, Index};

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

fn temp(name: &str) -> PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("censusindex-fuzz-{}-{name}", std::process::id()));
    p
}

#[test]
fn random_bytes_never_panic_in_snapshot_load() {
    let path = temp("snapshot.bin");
    let mut rng = Rng(0x0f0f_1234_5678_9abc);
    for _ in 0..500 {
        let bytes = rng.bytes(4096);
        std::fs::write(&path, &bytes).unwrap();
        // Cualquier basura tiene que dar Err, no panic.
        assert!(
            snapshot::load(&path).is_err(),
            "aceptó {} bytes basura",
            bytes.len()
        );
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn snapshot_with_hostile_header_is_rejected() {
    let path = temp("snapshot-hostile.wcat");
    // Magic correcto y conteos absurdos: no debe intentar reservar 200 GB.
    let mut bytes = b"CENSUSWCAT".to_vec();
    bytes.extend_from_slice(&1u16.to_le_bytes()); // version
    bytes.extend_from_slice(&1u16.to_le_bytes()); // flags
    bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // node_count
    bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // name_units
    bytes.extend_from_slice(&u32::MAX.to_le_bytes()); // root_count
    bytes.extend_from_slice(&[0u8; 3]);
    bytes.extend_from_slice(&(b'C' as u32).to_le_bytes());
    bytes.extend_from_slice(&0i64.to_le_bytes());
    bytes.extend_from_slice(&[0u8; 5 * 8]);
    std::fs::write(&path, &bytes).unwrap();
    let err = snapshot::load(&path).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("absurd")
            || msg.contains("truncad")
            || msg.contains("header")
            || msg.contains("checksum"),
        "error inesperado: {msg}"
    );
    let _ = std::fs::remove_file(&path);
}

#[test]
fn a_valid_snapshot_round_trips_through_disk() {
    let ix = Index::default();
    let path = temp("snapshot-ok.wcat");
    let bytes = snapshot::save(&ix, &path).expect("guardar un índice vacío");
    assert!(bytes >= 64);
    let back = snapshot::load(&path).expect("leer lo que acabamos de escribir");
    assert_eq!(back.len(), ix.len());
    assert!(back.roots.is_empty());
    let _ = std::fs::remove_file(&path);
}

#[test]
fn random_query_strings_never_panic() {
    let ix = Index::default();
    let mut rng = Rng(0xabcd_1234_ef01_2345);
    let alfabeto: Vec<char> = "abcdefghij0123456789 :\"\\<>=-_,.*?!ñÑáé".chars().collect();
    for _ in 0..5_000 {
        let n = (rng.next() as usize) % 40;
        let s: String = (0..n)
            .map(|_| alfabeto[(rng.next() as usize) % alfabeto.len()])
            .collect();
        let q = query::parse(&s, &ix);
        // Sobre un índice vacío ninguna consulta puede devolver nada.
        assert!(ix.match_ids(&q).is_empty());
    }
}

#[test]
fn pathological_queries_are_bounded() {
    let ix = Index::default();
    for s in [
        "",
        " ",
        ":",
        "path:",
        "path:\"\"",
        "ext:,,,,,",
        "size:>",
        "size:>99999999999999999999999",
        "dm:",
        "rc:>=",
        "rc:>=9999-99-99T99:99:99",
        &"a".repeat(10_000),
        &"ext:".repeat(100),
        "\u{0}\u{1}\u{2}",
    ] {
        let _ = query::parse(s, &ix); // no debe panic
    }
}

#[test]
fn random_paths_never_panic_in_find_dir() {
    let ix = Index::default();
    let mut rng = Rng(0x1111_2222_3333_4444);
    let piezas = ["C:", "D:", "\\", "/", "Users", "..", ".", "a", "ñ", ""];
    for _ in 0..5_000 {
        let n = (rng.next() as usize) % 8;
        let mut s = String::new();
        for _ in 0..n {
            s.push_str(piezas[(rng.next() as usize) % piezas.len()]);
        }
        let _ = ix.find_dir(&s); // índice vacío: siempre None, nunca panic
        let _ = ix.find_dir(&s.chars().rev().collect::<String>());
    }
}

#[test]
fn merging_nothing_gives_an_empty_index() {
    let ix = Index::merge(Vec::new());
    assert_eq!(ix.len(), 0);
    assert!(ix.roots.is_empty());
    assert!(
        ix.name_string(0).is_empty(),
        "un índice vacío no tiene nombre de raíz"
    );
    assert!(ix.dir_path(0).is_empty());
    assert!(!ix.is_dir(0));
}
