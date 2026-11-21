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
    pub fn load() -> Self {
        let Ok(texto) = std::fs::read_to_string(path()) else {
            return Self::default();
        };
        let mut p = Self::default();
        for linea in texto.lines() {
            let Some((clave, valor)) = linea.split_once('=') else {
                continue;
            };
            match clave.trim() {
                "paused" => p.paused = valor.trim() == "1" || valor.trim() == "true",
                "resume_at" => p.resume_at = valor.trim().parse::<i64>().ok(),
                _ => {}
            }
        }
        p
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(dir())?;
        let texto = format!(
            "paused={}\nresume_at={}\n",
            if self.paused { 1 } else { 0 },
            self.resume_at.unwrap_or(0)
        );
        std::fs::write(path(), texto)
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
