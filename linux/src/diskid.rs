//! Identidad de hardware y de volumen.
//!
//! La letra o el punto de montaje NO identifican un disco: dos discos externos
//! pueden caer en `/media/lucio/BACKUP` en momentos distintos, y `sdb1`/`sdc1`
//! cambia con el orden de enumeracion. Census usa:
//!
//! ```text
//! DiskHwId  = hash(WWN o serial || vendor || model || capacidad)
//! volume_id = hash(DiskHwId || offset_particion || fs_uuid)
//! mount     = /media/...  (solo presentacion, se remapea)
//! ```
//!
//! Los datos salen de udev (`/run/udev/data/b<major>:<minor>`) y de sysfs, sin
//! ejecutar `udevadm`. Si el disco no tiene identidad determinable no entra a
//! federacion: antes un volumen faltante que dos fusionados.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::plataforma::fnv1a64;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum IdentityStatus {
    #[default]
    Ok,
    /// Misma serial con distinto modelo/capacidad (puentes USB que mienten).
    SerialAmbiguo,
    /// Dos volumenes distintos terminaron con la misma clave.
    PosibleDuplicado,
    /// Sin serial, WWN ni UUID de filesystem.
    SinIdentidad,
}

impl IdentityStatus {
    pub fn label(self) -> &'static str {
        match self {
            IdentityStatus::Ok => "ok",
            IdentityStatus::SerialAmbiguo => "serial_ambiguo",
            IdentityStatus::PosibleDuplicado => "posible_duplicado",
            IdentityStatus::SinIdentidad => "sin_identidad",
        }
    }

    pub fn federable(self) -> bool {
        // Conservador: una serial ambigua no federa aunque se haya desambiguado.
        matches!(self, IdentityStatus::Ok)
    }
}

#[derive(Clone, Debug, Default)]
pub struct DiskIdentity {
    pub disk_hw_id: String,
    pub serial: String,
    pub vendor: String,
    pub model: String,
    pub wwn: String,
    pub capacity: u64,
    pub partition_offset: u64,
    pub partition_size: u64,
    pub status: IdentityStatus,
    pub note: Option<String>,
}

fn hash_parts(parts: &[&[u8]]) -> String {
    let mut buf = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            buf.push(0);
        }
        buf.extend_from_slice(p);
    }
    format!("{:016x}", fnv1a64(&buf))
}

/// `DiskHwId = hash(WWN/serial || vendor || model || capacidad)`.
pub fn disk_hw_id(wwn: &str, serial: &str, vendor: &str, model: &str, capacity: u64) -> String {
    let id = if !wwn.trim().is_empty() { wwn } else { serial };
    if id.trim().is_empty() {
        return String::new();
    }
    hash_parts(&[
        id.as_bytes(),
        vendor.as_bytes(),
        model.as_bytes(),
        &capacity.to_le_bytes(),
    ])
}

/// `volume_id = hash(DiskHwId || offset || fs_uuid)`. Sin punto de montaje.
pub fn volume_id(disk_hw_id: &str, partition_offset: u64, fs_uuid: &str) -> String {
    hash_parts(&[
        disk_hw_id.as_bytes(),
        &partition_offset.to_le_bytes(),
        fs_uuid.as_bytes(),
    ])
}

fn read_sys(path: impl AsRef<Path>) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn dev_name(major: u64, minor: u64) -> Option<String> {
    let link = fs::read_link(format!("/sys/dev/block/{major}:{minor}")).ok()?;
    link.file_name().map(|s| s.to_string_lossy().into_owned())
}

fn dev_major_minor(name: &str) -> Option<(u64, u64)> {
    read_sys(format!("/sys/class/block/{name}/dev")).and_then(|s| {
        let (a, b) = s.split_once(':')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    })
}

/// Sigue `slaves` (dm/LUKS/RAID) y sube de particion a disco fisico.
fn physical_name(name: &str, depth: u32) -> String {
    if depth > 8 {
        return name.to_string();
    }
    let base = PathBuf::from(format!("/sys/class/block/{name}"));
    let is_part = base.join("partition").exists();
    if is_part {
        if let Ok(target) = fs::canonicalize(&base) {
            if let Some(parent) = target.parent().and_then(|p| p.file_name()) {
                let p = parent.to_string_lossy().into_owned();
                if p != name {
                    return physical_name(&p, depth + 1);
                }
            }
        }
    }
    if let Ok(rd) = fs::read_dir(base.join("slaves")) {
        if let Some(Ok(e)) = rd.into_iter().next() {
            let slave = e.file_name().to_string_lossy().into_owned();
            return physical_name(&slave, depth + 1);
        }
    }
    name.to_string()
}

/// Parsea las lineas `E:CLAVE=valor` de `/run/udev/data/bX:Y`.
pub fn parse_udev(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let Some(rest) = line.strip_prefix("E:") else {
            continue;
        };
        let Some((k, v)) = rest.split_once('=') else {
            continue;
        };
        out.insert(k.trim().to_string(), unescape(v));
    }
    out
}

fn unescape(v: &str) -> String {
    let b = v.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 3 < b.len() && b[i + 1] == b'x' {
            if let Ok(byte) = u8::from_str_radix(&v[i + 2..i + 4], 16) {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        if b[i] == b'\\' && i + 1 < b.len() {
            out.push(b[i + 1]);
            i += 2;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn udev_props(major: u64, minor: u64) -> HashMap<String, String> {
    let path = format!("/run/udev/data/b{major}:{minor}");
    fs::read_to_string(path)
        .map(|t| parse_udev(&t))
        .unwrap_or_default()
}

fn prop<'a>(p: &'a HashMap<String, String>, keys: &[&str]) -> Option<&'a str> {
    keys.iter()
        .find_map(|k| p.get(*k).map(|s| s.as_str()))
        .filter(|s| !s.trim().is_empty())
}

/// Identifica el disco fisico y la particion del dispositivo `major:minor`.
/// `fs_uuid` se usa solo para decidir si hay identidad suficiente.
pub fn identify(major: u64, minor: u64, fs_uuid: &str) -> DiskIdentity {
    let name = match dev_name(major, minor) {
        Some(n) => n,
        None => {
            let status = if fs_uuid.is_empty() {
                IdentityStatus::SinIdentidad
            } else {
                IdentityStatus::Ok
            };
            return DiskIdentity {
                status,
                ..Default::default()
            };
        }
    };
    let phys = physical_name(&name, 0);
    let (pmaj, pmin) = dev_major_minor(&phys).unwrap_or((major, minor));
    let props = udev_props(pmaj, pmin);

    let wwn = prop(&props, &["ID_WWN_WITH_EXTENSION", "ID_WWN"])
        .map(|s| s.to_string())
        .or_else(|| read_sys(format!("/sys/class/block/{phys}/device/wwid")))
        .or_else(|| read_sys(format!("/sys/class/block/{phys}/wwid")))
        .unwrap_or_default();
    let serial = prop(&props, &["ID_SERIAL_SHORT", "ID_SERIAL"])
        .map(|s| s.to_string())
        .or_else(|| read_sys(format!("/sys/class/block/{phys}/device/serial")))
        .unwrap_or_default();
    let vendor = prop(&props, &["ID_VENDOR", "ID_VENDOR_FROM_DATABASE"])
        .map(|s| s.to_string())
        .or_else(|| read_sys(format!("/sys/class/block/{phys}/device/vendor")))
        .unwrap_or_default();
    let model = prop(&props, &["ID_MODEL", "ID_MODEL_FROM_DATABASE"])
        .map(|s| s.to_string())
        .or_else(|| read_sys(format!("/sys/class/block/{phys}/device/model")))
        .unwrap_or_default();

    let sector = 512u64;
    let capacity = read_sys(format!("/sys/class/block/{phys}/size"))
        .and_then(|s| s.parse::<u64>().ok())
        .map(|s| s.saturating_mul(sector))
        .unwrap_or(0);
    let (partition_offset, partition_size) = if name != phys {
        let start = read_sys(format!("/sys/class/block/{name}/start"))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            .saturating_mul(sector);
        let size = read_sys(format!("/sys/class/block/{name}/size"))
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0)
            .saturating_mul(sector);
        (start, size)
    } else {
        (0, capacity)
    };

    let hw = disk_hw_id(&wwn, &serial, &vendor, &model, capacity);
    let status = if hw.is_empty() && fs_uuid.is_empty() {
        IdentityStatus::SinIdentidad
    } else {
        IdentityStatus::Ok
    };
    let note = if hw.is_empty() {
        Some("sin serial/WWN de hardware; identidad solo por filesystem".into())
    } else {
        None
    };
    DiskIdentity {
        disk_hw_id: hw,
        serial,
        vendor,
        model,
        wwn,
        capacity,
        partition_offset,
        partition_size,
        status,
        note,
    }
}

/// Marca `serial_ambiguo` cuando la misma serial aparece con distinto
/// modelo/capacidad. El llamador agrupa todos los montajes antes.
pub fn mark_ambiguous(items: &mut [DiskIdentity]) {
    let mut seen: HashMap<String, (String, u64)> = HashMap::new();
    let mut ambiguous: Vec<String> = Vec::new();
    for id in items.iter() {
        let serial = if !id.wwn.is_empty() {
            id.wwn.clone()
        } else {
            id.serial.clone()
        };
        if serial.is_empty() {
            continue;
        }
        let pair = (id.model.clone(), id.capacity);
        match seen.get(&serial) {
            Some(prev) if *prev != pair => ambiguous.push(serial),
            None => {
                seen.insert(serial, pair);
            }
            _ => {}
        }
    }
    for id in items.iter_mut() {
        let serial = if !id.wwn.is_empty() {
            id.wwn.clone()
        } else {
            id.serial.clone()
        };
        if ambiguous.contains(&serial) {
            id.status = IdentityStatus::SerialAmbiguo;
            id.note = Some(
                "misma serial con distinto modelo/capacidad (posible puente USB); \
                 se agrega capacidad y particion al hash"
                    .into(),
            );
        }
    }
}

/// Recalcula `disk_hw_id` para una serial ambigua incluyendo modelo,
/// capacidad y offset de particion (evita mezclar dos discos del mismo puente).
pub fn disambiguate(id: &mut DiskIdentity, partition_offset: u64) {
    if id.status != IdentityStatus::SerialAmbiguo {
        return;
    }
    let base = if !id.wwn.is_empty() {
        id.wwn.clone()
    } else {
        id.serial.clone()
    };
    id.disk_hw_id = hash_parts(&[
        base.as_bytes(),
        id.model.as_bytes(),
        &id.capacity.to_le_bytes(),
        &partition_offset.to_le_bytes(),
    ]);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn volume_id_does_not_depend_on_mount() {
        let hw = disk_hw_id("", "SER123", "Seagate", "Backup+", 1_000_000);
        let a = volume_id(&hw, 1_048_576, "uuid-1");
        let b = volume_id(&hw, 1_048_576, "uuid-1");
        assert_eq!(a, b);
        assert_ne!(a, volume_id(&hw, 2_097_152, "uuid-1"));
        assert_ne!(a, volume_id(&hw, 1_048_576, "uuid-2"));
        assert_ne!(a, volume_id(&hw, 0, "uuid-1"));
    }

    #[test]
    fn different_serial_different_volume_even_with_same_label() {
        let a = disk_hw_id("", "SER-A", "WD", "Elements", 2_000);
        let b = disk_hw_id("", "SER-B", "WD", "Elements", 2_000);
        assert_ne!(a, b);
        assert_ne!(volume_id(&a, 0, "uuid"), volume_id(&b, 0, "uuid"));
    }

    #[test]
    fn parses_udev_and_unescapes() {
        let text =
            "E:ID_SERIAL_SHORT=ABC123\nE:ID_MODEL=My\\x20Disk\nE:ID_WWN_WITH_EXTENSION=0x5000\n";
        let p = parse_udev(text);
        assert_eq!(p.get("ID_SERIAL_SHORT").unwrap(), "ABC123");
        assert_eq!(p.get("ID_MODEL").unwrap(), "My Disk");
        assert_eq!(p.get("ID_WWN_WITH_EXTENSION").unwrap(), "0x5000");
    }

    #[test]
    fn ambiguous_serial_is_flagged_and_disambiguated() {
        let a = DiskIdentity {
            serial: "BRIDGE-1".into(),
            model: "Disk A".into(),
            capacity: 1000,
            ..Default::default()
        };
        let b = DiskIdentity {
            serial: "BRIDGE-1".into(),
            model: "Disk B".into(),
            capacity: 2000,
            ..Default::default()
        };
        let mut items = vec![a, b];
        mark_ambiguous(&mut items);
        assert_eq!(items[0].status, IdentityStatus::SerialAmbiguo);
        assert_eq!(items[1].status, IdentityStatus::SerialAmbiguo);
        disambiguate(&mut items[0], 0);
        disambiguate(&mut items[1], 0);
        assert_ne!(items[0].disk_hw_id, items[1].disk_hw_id);
    }

    #[test]
    fn no_identity_means_not_federable() {
        let st = IdentityStatus::SinIdentidad;
        assert!(!st.federable());
        assert!(IdentityStatus::Ok.federable());
    }
}
