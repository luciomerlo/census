//! Volumenes de Android. Kotlin los enumera con `StorageManager` y los pasa
//! como JSON; aca se convierten en `Mount` con identidad estable.
//!
//! Identidad: el almacenamiento interno no expone numero de serie de hardware y
//! una tarjeta SD tampoco. Se usa el UUID del filesystem (`StorageVolume.uuid`,
//! el mismo que aparece en `/storage/XXXX-XXXX`):
//!
//! * interno: `fs_uuid = "emulated:<pc_id>"`, tamano 0 (no cambia al llenarse);
//! * SD/USB con UUID: `volume_id = hash(uuid || "sd" || capacidad)`;
//! * sin UUID: `sin_identidad`, no federable (regla dura de ATLAS §10).
//!
//! La ruta (`/storage/emulated/0`, `/storage/XXXX-XXXX`) es presentacion.

use census::plataforma::volume_id;
use census::volumes::Mount;
use serde_json::Value;

/// Convierte la lista de Kotlin:
/// `[{"path","uuid","label","removable","primary","total","free"}]`.
pub fn from_json(pc_id: &str, text: &str) -> Result<Vec<Mount>, String> {
    let v: Value = serde_json::from_str(text).map_err(|e| format!("volumenes: {e}"))?;
    let arr = v.as_array().ok_or("volumenes: se esperaba un arreglo")?;
    let mut out = Vec::new();
    for item in arr {
        let path = item["path"].as_str().unwrap_or("").trim_end_matches('/');
        if path.is_empty() {
            continue;
        }
        let uuid = item["uuid"].as_str().unwrap_or("");
        let primary = item["primary"].as_bool().unwrap_or(false);
        let removable = item["removable"].as_bool().unwrap_or(!primary);
        let total = item["total"].as_u64().unwrap_or(0);
        let free = item["free"].as_u64().unwrap_or(0);
        let label = item["label"].as_str().unwrap_or("").to_string();
        out.push(mount(
            pc_id, path, uuid, &label, primary, removable, total, free,
        ));
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
pub fn mount(
    pc_id: &str,
    path: &str,
    uuid: &str,
    label: &str,
    primary: bool,
    removable: bool,
    total: u64,
    free: u64,
) -> Mount {
    let (fs_uuid, fs, vol_total, status, federable) = if primary {
        (format!("emulated:{pc_id}"), "emulated", 0, "ok", true)
    } else if !uuid.is_empty() {
        (uuid.to_string(), "sd", total, "ok", true)
    } else {
        (String::new(), "sd", total, "sin_identidad", false)
    };
    let vid = if status == "sin_identidad" {
        // Estable por ruta pero marcado como no federable.
        format!("dir:{pc_id}:{path}")
    } else {
        volume_id(&fs_uuid, fs, vol_total)
    };
    Mount {
        path: path.to_string(),
        source: if primary {
            "emulated".into()
        } else {
            uuid.into()
        },
        fs: fs.into(),
        label: if label.is_empty() {
            if primary {
                "Almacenamiento interno".into()
            } else {
                path.into()
            }
        } else {
            label.into()
        },
        fs_uuid,
        volume_id: vid,
        total,
        free,
        kind: if removable {
            "removable".into()
        } else {
            "fixed".into()
        },
        removable,
        identity_status: status.into(),
        federable,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_id_ignores_free_space_and_mount_path() {
        let a = mount("pc1", "/storage/emulated/0", "", "", true, false, 128, 10);
        let b = mount("pc1", "/sdcard", "", "", true, false, 128, 99);
        assert_eq!(a.volume_id, b.volume_id);
        assert_ne!(
            a.volume_id,
            mount("pc2", "/sdcard", "", "", true, false, 128, 10).volume_id
        );
    }

    #[test]
    fn two_sd_cards_with_same_label_never_merge() {
        let a = mount(
            "pc",
            "/storage/1111-AAAA",
            "1111-AAAA",
            "SD",
            false,
            true,
            64,
            1,
        );
        let b = mount(
            "pc",
            "/storage/2222-BBBB",
            "2222-BBBB",
            "SD",
            false,
            true,
            64,
            1,
        );
        assert_ne!(a.volume_id, b.volume_id);
        let moved = mount(
            "pc",
            "/mnt/media_rw/1111-AAAA",
            "1111-AAAA",
            "SD",
            false,
            true,
            64,
            7,
        );
        assert_eq!(a.volume_id, moved.volume_id);
    }

    #[test]
    fn sd_without_uuid_is_not_federable() {
        let m = mount("pc", "/storage/x", "", "", false, true, 64, 1);
        assert!(!m.federable);
        assert_eq!(m.identity_status, "sin_identidad");
    }

    #[test]
    fn parses_kotlin_json() {
        let v = from_json(
            "pc",
            r#"[{"path":"/storage/emulated/0","primary":true,"total":100,"free":1},
                {"path":"/storage/AB12-CD34/","uuid":"AB12-CD34","removable":true,"total":50}]"#,
        )
        .unwrap();
        assert_eq!(v.len(), 2);
        assert_eq!(v[1].path, "/storage/AB12-CD34");
        assert!(v[1].removable);
    }
}
