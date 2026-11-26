//! Politica para volumenes que aparecen despues de instalar (pendrive, SD,
//! disco USB): indexar siempre, nunca, o preguntar.
//!
//! * El almacenamiento fijo/interno se indexa siempre; la politica solo manda
//!   sobre volumenes **extraibles** (y de red/nube si se pide).
//! * Hay una politica global y excepciones por `volume_id`, que ganan.
//! * Un volumen se pregunta **una sola vez** por conexion pendiente: no se
//!   repite la pregunta cada vez que el sistema avisa un cambio.
//! * Un volumen ya indexado sigue actualizandose salvo que pase a `never`
//!   (en ese caso queda congelado: se conserva su ultima copia, no se borra).
//!
//! Identidad: siempre por `volume_id`, nunca por la ruta ni la etiqueta.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Policy {
    #[default]
    Ask,
    Always,
    Never,
}

impl Policy {
    pub fn parse(s: &str) -> Option<Policy> {
        match s.trim().to_ascii_lowercase().as_str() {
            "ask" | "preguntar" => Some(Policy::Ask),
            "always" | "siempre" | "force" | "forzar" => Some(Policy::Always),
            "never" | "nunca" => Some(Policy::Never),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    Index,
    Skip,
    Ask,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Known {
    pub label: String,
    pub path: String,
    pub removable: bool,
    pub last_seen: i64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct VolPolicy {
    /// Para extraibles sin excepcion propia.
    #[serde(default)]
    pub default: Policy,
    /// Tambien aplicar la politica a red/nube (por defecto solo extraibles).
    #[serde(default)]
    pub include_network: bool,
    #[serde(default)]
    pub per_volume: BTreeMap<String, Policy>,
    #[serde(default)]
    pub known: BTreeMap<String, Known>,
    /// Ya se les pregunto y no contestaron.
    #[serde(default)]
    pub asked: BTreeSet<String>,
}

impl VolPolicy {
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        let tmp = path.with_extension("tmp");
        std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, path).map_err(|e| e.to_string())
    }

    /// Politica efectiva de un volumen (la propia gana sobre la global).
    pub fn effective(&self, volume_id: &str) -> Policy {
        self.per_volume.get(volume_id).copied().unwrap_or(self.default)
    }

    /// Que hacer con un volumen. `indexed`: ya tiene copia en el indice.
    pub fn decide(&self, volume_id: &str, removable: bool, network: bool, indexed: bool) -> Decision {
        let governed = removable || (network && self.include_network);
        if !governed {
            return Decision::Index;
        }
        match self.effective(volume_id) {
            Policy::Always => Decision::Index,
            Policy::Never => Decision::Skip,
            // Si ya lo indexaste antes y no lo prohibiste, sigue al dia.
            Policy::Ask if indexed => Decision::Index,
            Policy::Ask => Decision::Ask,
        }
    }

    /// `volume_id` `"*"` fija la politica global.
    pub fn set(&mut self, volume_id: &str, p: Policy) {
        if volume_id == "*" {
            self.default = p;
        } else {
            self.per_volume.insert(volume_id.to_string(), p);
        }
        self.asked.remove(volume_id);
    }

    /// Quita la excepcion de un volumen (vuelve a la global).
    pub fn clear(&mut self, volume_id: &str) {
        self.per_volume.remove(volume_id);
    }

    /// Los volumenes que ya no estan conectados vuelven a poder preguntarse la
    /// proxima vez que aparezcan ("una vez por conexion").
    pub fn retain_connected(&mut self, present: &[String]) {
        self.asked.retain(|id| present.contains(id));
    }

    /// Registra el volumen visto. `true` si hay que preguntar ahora (primera
    /// vez que queda pendiente).
    pub fn note_seen(&mut self, volume_id: &str, k: Known, decision: Decision) -> bool {
        self.known.insert(volume_id.to_string(), k);
        match decision {
            Decision::Ask => self.asked.insert(volume_id.to_string()),
            _ => {
                self.asked.remove(volume_id);
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn internal_is_always_indexed_regardless_of_policy() {
        let mut p = VolPolicy::default();
        p.default = Policy::Never;
        assert_eq!(p.decide("int", false, false, false), Decision::Index);
    }

    #[test]
    fn removable_follows_global_then_per_volume_override() {
        let mut p = VolPolicy::default();
        assert_eq!(p.decide("usb", true, false, false), Decision::Ask);
        p.set("*", Policy::Always);
        assert_eq!(p.decide("usb", true, false, false), Decision::Index);
        p.set("usb", Policy::Never);
        assert_eq!(p.decide("usb", true, false, false), Decision::Skip);
        assert_eq!(p.decide("otro", true, false, false), Decision::Index);
        p.clear("usb");
        assert_eq!(p.decide("usb", true, false, false), Decision::Index);
    }

    #[test]
    fn already_indexed_keeps_updating_under_ask_but_never_freezes_it() {
        let mut p = VolPolicy::default();
        assert_eq!(p.decide("sd", true, false, true), Decision::Index);
        p.set("sd", Policy::Never);
        assert_eq!(p.decide("sd", true, false, true), Decision::Skip);
    }

    #[test]
    fn network_is_ungoverned_unless_asked() {
        let mut p = VolPolicy::default();
        assert_eq!(p.decide("nas", false, true, false), Decision::Index);
        p.include_network = true;
        assert_eq!(p.decide("nas", false, true, false), Decision::Ask);
    }

    #[test]
    fn asks_only_once_until_answered() {
        let mut p = VolPolicy::default();
        let k = || Known { label: "USB".into(), path: "/x".into(), removable: true, last_seen: 1 };
        assert!(p.note_seen("usb", k(), Decision::Ask));
        assert!(!p.note_seen("usb", k(), Decision::Ask), "no se repite");
        p.set("usb", Policy::Always);
        assert!(!p.note_seen("usb", k(), Decision::Index));
        // Tras desconectar y volver a un Ask, se vuelve a preguntar.
        p.clear("usb");
        p.default = Policy::Ask;
        assert!(p.note_seen("usb", k(), Decision::Ask));
    }

    #[test]
    fn unplugging_allows_asking_again_on_the_next_connection() {
        let mut p = VolPolicy::default();
        let k = || Known { label: "USB".into(), path: "/x".into(), removable: true, last_seen: 1 };
        assert!(p.note_seen("usb", k(), Decision::Ask));
        p.retain_connected(&[]);
        assert!(p.note_seen("usb", k(), Decision::Ask));
        p.retain_connected(&["usb".to_string()]);
        assert!(!p.note_seen("usb", k(), Decision::Ask));
    }

    #[test]
    fn parses_spanish_and_english_and_round_trips() {
        assert_eq!(Policy::parse("Siempre"), Some(Policy::Always));
        assert_eq!(Policy::parse("forzar"), Some(Policy::Always));
        assert_eq!(Policy::parse("nunca"), Some(Policy::Never));
        assert_eq!(Policy::parse("preguntar"), Some(Policy::Ask));
        assert_eq!(Policy::parse("x"), None);
        let mut p = VolPolicy::default();
        p.set("a", Policy::Never);
        let f = std::env::temp_dir().join(format!("censusindex-pol-{}.json", std::process::id()));
        p.save(&f).unwrap();
        assert_eq!(VolPolicy::load(&f).per_volume["a"], Policy::Never);
        let _ = std::fs::remove_file(f);
    }
}
