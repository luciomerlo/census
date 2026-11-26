//! Contrato de `censusindex volumes --json` (`docs/ATLAS.md` §9/§10).
//!
//! Este JSON es lo que consumen Wiz, Sort y Atlas: los nombres y los tipos son
//! parte del contrato, no un detalle de presentación. La prueba lanza el
//! binario real y valida forma, no valores de una máquina en particular.

use std::process::Command;

use serde_json::Value;

fn volumenes() -> Vec<Value> {
    let salida = Command::new(env!("CARGO_BIN_EXE_censusindex"))
        .arg("volumes")
        .arg("--json")
        .output()
        .expect("no se pudo ejecutar censusindex volumes --json");
    assert!(
        salida.status.success(),
        "censusindex volumes --json falló: {}",
        String::from_utf8_lossy(&salida.stderr)
    );
    let texto = String::from_utf8(salida.stdout).expect("el JSON no es UTF-8");
    let valor: Value = serde_json::from_str(&texto).expect("JSON inválido");
    valor
        .as_array()
        .cloned()
        .expect("se esperaba un array de volúmenes")
}

fn hex_de_16(valor: &str) -> bool {
    valor.len() == 16
        && valor
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

#[test]
fn el_json_tiene_los_campos_del_contrato() {
    let filas = volumenes();
    assert!(!filas.is_empty(), "no se enumeró ningún volumen");

    for fila in &filas {
        for campo in [
            "mount",
            "label",
            "fstype",
            "total_bytes",
            "volume_id",
            "disk_hw_id",
            "serial",
            "serial_ambiguo",
            "cloud_id",
            "kind",
            "identity_status",
            "federable",
        ] {
            assert!(fila.get(campo).is_some(), "falta el campo {campo}: {fila}");
        }

        let mount = fila["mount"].as_str().expect("mount debe ser string");
        assert_eq!(mount.len(), 2, "mount raro: {mount}");
        assert!(mount.ends_with(':'));

        for campo in ["volume_id", "disk_hw_id"] {
            let id = fila[campo].as_str().expect("el id debe ser string");
            assert!(
                id.is_empty() || hex_de_16(id),
                "{campo} debe ser FNV-1a 64 en hex minúsculas: {id}"
            );
            // §10: la letra no es identidad, ni siquiera por accidente.
            assert_ne!(id, mount, "{campo} no puede ser la letra");
        }

        assert!(fila["serial_ambiguo"].as_bool().is_some());
        assert!(fila["federable"].as_bool().is_some());
        let estado = fila["identity_status"].as_str().expect("estado");
        assert!(
            ["ok", "serial_ambiguo", "posible_duplicado", "sin_identidad"].contains(&estado),
            "estado desconocido: {estado}"
        );

        if fila["kind"] == "cloud" {
            // §9: la identidad de nube es `cloud_id`, y no usa la letra.
            let cloud = fila["cloud_id"]
                .as_str()
                .expect("una unidad de nube debe traer cloud_id");
            assert!(hex_de_16(cloud), "cloud_id raro: {cloud}");
            let esperado = fila["cloud_account"].as_str().expect("cuenta de nube");
            assert!(
                !esperado.is_empty(),
                "la cuenta no puede quedar vacía (se usa desconocida:<letra>)"
            );
            assert!(
                fila["volume_id"].as_str().is_some_and(|v| v.is_empty()),
                "la nube no lleva volume_id de hardware"
            );
        } else {
            assert!(
                fila["cloud_id"].is_null(),
                "una unidad local no puede tener cloud_id"
            );
        }
    }

    // En esta máquina (y en cualquier Windows con disco GPT) `C:` tiene que
    // resolver a alguna identidad, aunque no haya elevación.
    if let Some(c) = filas.iter().find(|f| f["mount"] == "C:") {
        assert!(
            !c["disk_hw_id"].as_str().unwrap_or("").is_empty(),
            "sin serial ni GUID no se puede quedar sin DiskHwId: {c}"
        );
        assert!(!c["volume_id"].as_str().unwrap_or("").is_empty());
    }
}
