//! Pausa del trabajo de fondo, con reanudación programada.
//!
//! El usuario puede cortar el indexado pesado y dejarlo para la noche. El
//! estado vive en disco (`pause.json`), así que cerrar la ventana o apagar la
//! máquina no lo borra: al volver, Census sigue en pausa hasta la hora pactada.
//!
//! Las consultas nunca se frenan: la pausa es para el trabajo que consume
//! disco y CPU, no para lo que el usuario está mirando.

use std::path::PathBuf;

/// Estado de la pausa.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Pause {
    pub paused: bool,
    /// Milisegundos desde la época en que se reanuda solo (misma unidad que
    /// usa Wiz, para que el contrato sea uno solo). `None` = hasta que el
    /// usuario lo pida.
    pub resume_at: Option<i64>,
}

fn dir() -> PathBuf {
    crate::snapshot::cache_dir().unwrap_or_else(|| PathBuf::from("."))
}

fn path() -> PathBuf {
    dir().join("pause.json")
}

impl Pause {
    /// Lee el estado del disco. Un archivo roto equivale a "sin pausa".
    ///
    /// Acepta el JSON acordado con Wiz y Sort y, por compatibilidad, el formato
    /// `clave=valor` que escribían las versiones anteriores de Census.
    pub fn load() -> Self {
        let Ok(texto) = std::fs::read_to_string(path()) else {
            return Self::default();
        };
        let mut p = Self::default();
        if texto.trim_start().starts_with('{') {
            for (clave, valor) in pares_json(&texto) {
                match clave.as_str() {
                    "paused" => p.paused = valor == "true",
                    "resume_at" => {
                        p.resume_at = valor.parse::<i64>().ok().filter(|v| *v > 0);
                    }
                    _ => {}
                }
            }
            return p;
        }
        for linea in texto.lines() {
            let Some((clave, valor)) = linea.split_once('=') else {
                continue;
            };
            match clave.trim() {
                "paused" => p.paused = valor.trim() == "1" || valor.trim() == "true",
                "resume_at" => {
                    p.resume_at = valor.trim().parse::<i64>().ok().filter(|v| *v > 0);
                }
                _ => {}
            }
        }
        p
    }

    /// Escribe el JSON compartido: `{"paused","resume_at","reason","updated"}`.
    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let resume = match self.resume_at {
            Some(v) => v.to_string(),
            None => String::from("null"),
        };
        let texto = format!(
            "{{\"paused\":{},\"resume_at\":{},\"reason\":\"{}\",\"updated\":{}}}\n",
            self.paused,
            resume,
            if self.paused { "manual" } else { "resume" },
            crate::time::now() * 1000
        );
        let tmp = dir().join(format!("pause.json.tmp.{}", std::process::id()));
        std::fs::write(&tmp, texto)?;
        std::fs::rename(&tmp, path())
    }

    /// ¿Hay que frenar el trabajo de fondo ahora?
    ///
    /// Si la hora de reanudación ya pasó, la pausa se levanta sola y se guarda,
    /// para que no haga falta apretar nada a la mañana.
    pub fn active(&self, now: i64) -> bool {
        if !self.paused {
            return false;
        }
        !matches!(self.resume_at, Some(at_ms) if now * 1000 >= at_ms)
    }

    /// Texto para el botón: "Pausar" o "Reanudar a las 22:00".
    pub fn label(&self, now: i64) -> String {
        if !self.active(now) {
            return String::from("Pausar");
        }
        match self.resume_at {
            Some(at_ms) => format!("Reanudar {}", crate::time::stamp_local(at_ms / 1000)),
            None => String::from("Reanudar"),
        }
    }
}

/// Pares `"clave":valor` de un JSON plano, suficientes para este archivo.
fn pares_json(texto: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let bytes: Vec<char> = texto.chars().collect();
    let mut i = 0usize;
    while i < bytes.len() {
        // Buscar el inicio de una clave.
        if bytes[i] != '"' {
            i += 1;
            continue;
        }
        i += 1;
        let mut clave = String::new();
        while i < bytes.len() && bytes[i] != '"' {
            clave.push(bytes[i]);
            i += 1;
        }
        i += 1;
        while i < bytes.len() && bytes[i] != ':' {
            i += 1;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_whitespace() {
            i += 1;
        }
        let mut valor = String::new();
        if i < bytes.len() && bytes[i] == '"' {
            i += 1;
            while i < bytes.len() && bytes[i] != '"' {
                valor.push(bytes[i]);
                i += 1;
            }
            i += 1;
        } else {
            while i < bytes.len() && !matches!(bytes[i], ',' | '}') {
                if !bytes[i].is_whitespace() {
                    valor.push(bytes[i]);
                }
                i += 1;
            }
        }
        out.push((clave, valor));
    }
    out
}

/// Estado vigente, ya normalizado (levanta la pausa vencida y persiste).
pub fn current() -> Pause {
    let mut p = Pause::load();
    if p.paused && !p.active(crate::time::now()) {
        p.paused = false;
        p.resume_at = None;
        let _ = p.save();
    }
    p
}

/// ¿El trabajo de fondo está en pausa?
pub fn is_paused() -> bool {
    current().paused
}

/// Pausa hasta el instante dado (o sin fecha si es `None`).
pub fn pause_until(resume_at_ms: Option<i64>) -> std::io::Result<Pause> {
    let p = Pause {
        paused: true,
        resume_at: resume_at_ms,
    };
    p.save()?;
    Ok(p)
}

pub fn resume() -> std::io::Result<()> {
    Pause::default().save()
}

/// Próxima ocurrencia local de `hh:mm` (hoy si todavía no pasó, si no mañana).
pub fn until_local(hh: u32, mm: u32) -> Option<i64> {
    if hh > 23 || mm > 59 {
        return None;
    }
    let z = jiff::Zoned::now();
    let hora = jiff::civil::Time::new(hh as i8, mm as i8, 0, 0).ok()?;
    let mut fecha = z.date();
    // Se devuelve en milisegundos, como el resto del contrato.
    let candidato = fecha
        .to_zoned(z.time_zone().clone())
        .ok()?
        .with()
        .time(hora)
        .build()
        .ok()?;
    if candidato.timestamp().as_second() <= z.timestamp().as_second() {
        fecha = fecha.tomorrow().ok()?;
    }
    let fin = fecha
        .to_zoned(z.time_zone().clone())
        .ok()?
        .with()
        .time(hora)
        .build()
        .ok()?;
    Some(fin.timestamp().as_millisecond())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_until_the_hour_arrives() {
        let p = Pause {
            paused: true,
            resume_at: Some(1_000_000),
        };
        assert!(p.active(999));
        assert!(!p.active(1_000));
        assert!(!p.active(1_500));
        assert!(!Pause::default().active(0));
    }

    #[test]
    fn label_changes_with_state() {
        let ahora = 1_000;
        assert_eq!(Pause::default().label(ahora), "Pausar");
        let p = Pause {
            paused: true,
            resume_at: None,
        };
        assert_eq!(p.label(ahora), "Reanudar");
        let con_hora = Pause {
            paused: true,
            resume_at: Some((ahora + 3_600) * 1000),
        };
        assert!(con_hora.label(ahora).starts_with("Reanudar "));
    }

    #[test]
    fn midnight_and_bad_times() {
        assert!(until_local(24, 0).is_none());
        assert!(until_local(22, 60).is_none());
        let t = until_local(22, 0).expect("hora válida");
        assert!(t > crate::time::now() * 1000);
    }
}

#[cfg(test)]
mod formato_tests {
    use super::*;

    #[test]
    fn json_round_trip() {
        let p = Pause {
            paused: true,
            resume_at: Some(1_795_314_600_000),
        };
        let texto = format!(
            "{{\"paused\":{},\"resume_at\":{},\"reason\":\"manual\",\"updated\":1}}",
            p.paused,
            p.resume_at.unwrap()
        );
        let pares = pares_json(&texto);
        assert_eq!(pares.iter().find(|(k, _)| k == "paused").unwrap().1, "true");
        assert_eq!(
            pares.iter().find(|(k, _)| k == "resume_at").unwrap().1,
            "1795314600000"
        );
        assert_eq!(
            pares.iter().find(|(k, _)| k == "reason").unwrap().1,
            "manual"
        );
    }

    #[test]
    fn null_resume_at_means_manual() {
        let pares = pares_json("{\"paused\":false,\"resume_at\":null}");
        assert_eq!(
            pares.iter().find(|(k, _)| k == "resume_at").unwrap().1,
            "null"
        );
        assert!(pares
            .iter()
            .find(|(k, _)| k == "resume_at")
            .unwrap()
            .1
            .parse::<i64>()
            .is_err());
    }
}
