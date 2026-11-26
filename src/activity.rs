//! `activity-Census.json`: qué está haciendo CensusIndex, para Wiz y Sort.
//!
//! Contrato acordado por las tres apps: un archivo por app en
//! `%LOCALAPPDATA%\WizeShared\`, cada una escribe **sólo** el suyo, con
//! `tmp` + `rename` para que nadie lea un archivo a medio escribir. El formato
//! es el mismo que usa Sort (`core/activity.rs`): `pid`, `app`, `task`,
//! `resources`, `roots`, `state`, `forced` y `heartbeat` en milisegundos.
//!
//! CensusIndex sólo anuncia el trabajo que **no** arrancó por sí solo: el replay
//! corto del journal al arrancar y los lotes de cambios que va aplicando. Las
//! tareas pesadas siguen siendo siempre `forced: true`.

use std::fs;
use std::io;
use std::path::PathBuf;

/// Nombre del archivo propio. Los otros dos son `activity-Wiz.json` y
/// `activity-Sort.json`.
const ARCHIVO: &str = "activity-Census.json";

/// Un latido más viejo que esto no se considera "corriendo" (lo aplican las
/// otras apps; acá sólo se escribe).
pub const STALE_MS: i64 = 120_000;

fn ahora_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Carpeta compartida. `WIZE_SHARED_DIR` permite apuntarla en tests.
pub fn shared_dir() -> PathBuf {
    if let Some(p) = std::env::var_os("WIZE_SHARED_DIR") {
        return PathBuf::from(p);
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join("WizeShared")
}

pub fn propio() -> PathBuf {
    shared_dir().join(ARCHIVO)
}

/// Publica el estado propio. `task` es lo que está corriendo (`usn`, `index`,
/// `idle`) y `resources` los recursos que toca (`wcat`, `usn`, `cpu`).
pub fn publicar(task: &str, resources: &[String], roots: &[String], state: &str) -> io::Result<()> {
    publicar_en(&shared_dir(), task, resources, roots, state)
}

fn publicar_en(
    dir: &std::path::Path,
    task: &str,
    resources: &[String],
    roots: &[String],
    state: &str,
) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let json = format!(
        "{{\"pid\":{},\"app\":\"Census\",\"task\":{},\"resources\":{},\"roots\":{},\"state\":{},\"forced\":false,\"heartbeat\":{}}}\n",
        std::process::id(),
        cadena(task),
        arreglo(resources),
        arreglo(roots),
        cadena(state),
        ahora_ms()
    );
    let destino = dir.join(ARCHIVO);
    let mut tmp = destino.clone().as_os_str().to_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    fs::write(&tmp, json.as_bytes())?;
    fs::rename(&tmp, &destino)
}

/// Marca que CensusIndex quedó quieto (sin journal, o al cerrar).
pub fn retirar() {
    let _ = publicar("idle", &[], &[], "idle");
}

/// Avisa que hace falta reindexar (lo consume el próximo `censusindex index`).
pub fn pedir_reindexado(motivo: &str) -> io::Result<PathBuf> {
    let dir = shared_dir();
    fs::create_dir_all(&dir)?;
    let path = dir.join("reindexar-Census.json");
    let json = format!(
        "{{\"app\":\"Census\",\"motivo\":{},\"pedido\":{}}}\n",
        cadena(motivo),
        ahora_ms()
    );
    fs::write(&path, json.as_bytes())?;
    Ok(path)
}

/// `true` si hay un pedido de reindexado pendiente (lo borra `censusindex index`).
pub fn reindexado_pendiente() -> Option<PathBuf> {
    let path = shared_dir().join("reindexar-Census.json");
    path.is_file().then_some(path)
}

pub fn limpiar_pedido() {
    let _ = fs::remove_file(shared_dir().join("reindexar-Census.json"));
}

/// Comillas y escapes mínimos: los nombres vienen de nosotros, pero igual hay
/// que escapar `\` y `"` para que el JSON no se rompa con rutas de Windows.
fn cadena(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn arreglo(items: &[String]) -> String {
    let mut out = String::from("[");
    for (i, s) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&cadena(s));
    }
    out.push(']');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir_temporal(nombre: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("censusindex-activity-{}-{}", nombre, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn escribe_json_valido_con_los_campos_del_contrato() {
        let dir = dir_temporal("campos");
        let recursos = vec!["wcat".to_string(), "usn".to_string()];
        let raices = vec!["C:".to_string()];
        publicar_en(&dir, "usn", &recursos, &raices, "running").unwrap();
        let texto = fs::read_to_string(dir.join(ARCHIVO)).unwrap();
        for campo in [
            "\"pid\":",
            "\"app\":\"Census\"",
            "\"task\":\"usn\"",
            "\"resources\":[\"wcat\",\"usn\"]",
            "\"roots\":[\"C:\"]",
            "\"state\":\"running\"",
            "\"forced\":false",
            "\"heartbeat\":",
        ] {
            assert!(texto.contains(campo), "falta {campo} en {texto}");
        }
        assert!(texto.trim_end().ends_with('}'));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn escapa_comillas_y_barras() {
        assert_eq!(cadena("a\"b"), "\"a\\\"b\"");
        assert_eq!(cadena("C:\\x"), "\"C:\\\\x\"");
        assert_eq!(arreglo(&[]), "[]");
        assert_eq!(arreglo(&["a".into(), "b".into()]), "[\"a\",\"b\"]");
    }

    #[test]
    fn el_pedido_de_reindexado_se_lee_y_se_limpia() {
        let dir = dir_temporal("pedido");
        let path = dir.join("reindexar-Census.json");
        fs::write(&path, "{}").unwrap();
        assert!(path.is_file());
        fs::remove_file(&path).unwrap();
        assert!(!path.is_file());
        let _ = fs::remove_dir_all(&dir);
    }
}
