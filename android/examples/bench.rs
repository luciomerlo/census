//! Mide el costo de cada modo sobre un arbol sintetico (en esta maquina, NO en
//! un telefono): `cargo run --release --example bench -- 20000 10`
use censusindex::volumes::Mount;
use censusindex_android::delta::{rescan, Mode, Options};
use std::{fs, path::PathBuf, time::Instant};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let dirs: usize = a.get(1).and_then(|s| s.parse().ok()).unwrap_or(5000);
    let per: usize = a.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);
    let base = std::env::temp_dir().join(format!("censusindex-bench-{}", std::process::id()));
    let _ = fs::remove_dir_all(&base);
    for d in 0..dirs {
        let p = base.join(format!("g{}/d{}", d % 50, d));
        fs::create_dir_all(&p).unwrap();
        for f in 0..per {
            fs::write(p.join(format!("f{f}.dat")), b"x").unwrap();
        }
    }
    let m = [Mount {
        path: base.to_string_lossy().into(),
        volume_id: "v".into(),
        ..Default::default()
    }];
    let o = Options::default();
    let t = Instant::now();
    let (full, r) = rescan(None, &m, &Mode::Full, &o).unwrap();
    println!(
        "full           {:>7} ms  nodos={} listadas={}",
        t.elapsed().as_millis(),
        r.nodes,
        r.dirs_listed
    );
    let t = Instant::now();
    let (_, r) = rescan(Some(&full), &m, &Mode::Sweep, &o).unwrap();
    println!(
        "sweep sin cambios {:>4} ms  listadas={} statted={}",
        t.elapsed().as_millis(),
        r.dirs_listed,
        r.dirs_statted
    );
    std::thread::sleep(std::time::Duration::from_millis(20));
    let dl: PathBuf = base.join("g3/d3");
    fs::write(dl.join("nuevo.apk"), b"y").unwrap();
    let t = Instant::now();
    let (_, r) = rescan(Some(&full), &m, &Mode::Sweep, &o).unwrap();
    println!(
        "sweep 1 carpeta   {:>4} ms  listadas={} statted={}",
        t.elapsed().as_millis(),
        r.dirs_listed,
        r.dirs_statted
    );
    let t = Instant::now();
    let (_, r) = rescan(
        Some(&full),
        &m,
        &Mode::Dirty(vec![dl.to_string_lossy().into()]),
        &o,
    )
    .unwrap();
    println!(
        "dirty 1 carpeta   {:>4} ms  listadas={} statted={} subarboles={}",
        t.elapsed().as_millis(),
        r.dirs_listed,
        r.dirs_statted,
        r.subtrees_copied
    );
    let _ = fs::remove_dir_all(&base);
}
