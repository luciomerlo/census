//! Actualización en caliente: aplicar el USN Journal sin reindexar (C2).
//!
//! Un hilo pregunta cada pocos segundos si el diario de NTFS tiene registros
//! nuevos y los aplica al [`Overlay`] del índice. Con eso, un archivo creado
//! aparece en la búsqueda en un par de segundos y uno borrado desaparece, sin
//! volver a leer la MFT.
//!
//! Si el diario se reinició o se dio vuelta (los cambios viejos ya no están),
//! no hay forma de recuperarlos: se avisa con [`Latido::Reindexar`] y el
//! llamador decide reindexar el volumen.

use std::io;
use std::sync::Arc;
use std::time::Duration;

use crate::index::Index;
use crate::usn;
use crate::win::open_volume;

/// Cada cuánto se pregunta si hay cambios.
pub const CADA: Duration = Duration::from_secs(2);

/// Cuántos bytes se piden por lectura (unos 8 MB de registros).
const BLOQUE: usize = 8 * 1024 * 1024;

/// Qué pasó en una vuelta del vigilante.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Latido {
    /// No hubo cambios.
    Nada,
    /// Se aplicaron `n` cambios.
    Aplicados(usize),
    /// Los cambios se perdieron: hay que reindexar para no mentir.
    Reindexar,
    /// El overlay creció demasiado (`n` cambios): conviene reindexar y vaciarlo.
    Compactar(u64),
    /// No hay permisos para abrir la unidad (CensusIndex no está elevado).
    SinPermisos,
}

/// Mantiene el handle al volumen y el estado del diario.
pub struct Vigilante {
    letter: char,
    handle: Option<crate::win::VolumeHandle>,
    ultimo_error: Option<String>,
}

impl Vigilante {
    pub fn nuevo(letter: char) -> Self {
        Vigilante {
            letter,
            handle: None,
            ultimo_error: None,
        }
    }

    fn handle(&mut self) -> io::Result<&crate::win::VolumeHandle> {
        if self.handle.is_none() {
            match open_volume(self.letter) {
                Ok(h) => self.handle = Some(h),
                Err(e) => {
                    self.ultimo_error = Some(format!("{e}"));
                    return Err(e);
                }
            }
        }
        match self.handle.as_ref() {
            Some(h) => Ok(h),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                "sin handle al volumen",
            )),
        }
    }

    pub fn ultimo_error(&self) -> Option<&str> {
        self.ultimo_error.as_deref()
    }

    /// Una vuelta: mira el diario y aplica lo que haya.
    pub fn latir(&mut self, ix: &Index) -> Latido {
        let h = match self.handle() {
            Ok(h) => h,
            Err(_) => return Latido::SinPermisos,
        };
        let diario = match usn::query(h) {
            Ok(d) => d,
            // Sin diario (unidad de red, FAT) esto simplemente no aplica.
            Err(_) => return Latido::Nada,
        };
        let (id, desde) = {
            let ov = ix.overlay();
            match ov.watermark {
                Some((id, usn)) => (id, usn),
                // Sin marcador: no sabemos qué nos perdimos, así que arrancamos
                // desde el final. Hay que dejarlo guardado o el vigilante
                // volvería a saltarse todo en cada vuelta.
                None => {
                    drop(ov);
                    let mut ov = ix.overlay_mut();
                    ov.watermark = Some((diario.id, diario.next_usn));
                    return Latido::Nada;
                }
            }
        };
        // El diario cambió de identidad o se dio vuelta: los cambios volaron.
        if id != diario.id || desde < diario.first_usn {
            let mut ov = ix.overlay_mut();
            ov.watermark = Some((diario.id, diario.next_usn));
            return Latido::Reindexar;
        }
        if desde >= diario.next_usn {
            return Latido::Nada;
        }
        let mut desde = desde;
        let mut total = 0usize;
        loop {
            let (cambios, siguiente) = match usn::read(h, desde, diario.id, BLOQUE) {
                Ok(v) => v,
                Err(_) => return Latido::Nada,
            };
            if siguiente <= desde && cambios.is_empty() {
                break;
            }
            if !cambios.is_empty() {
                let mut ov = ix.overlay_mut();
                total += ov.aplicar(ix, &cambios);
                ov.watermark = Some((diario.id, siguiente));
            } else {
                let mut ov = ix.overlay_mut();
                ov.watermark = Some((diario.id, siguiente));
            }
            desde = siguiente;
            if desde >= diario.next_usn || total > 100_000 {
                break;
            }
        }
        if total == 0 {
            Latido::Nada
        } else {
            // Se anuncia en activity-Census.json para que Wiz sepa que estamos
            // tocando el índice, sin bloquear: son lotes de milisegundos.
            let recursos = vec!["wcat".to_string(), "usn".to_string()];
            let raices = vec![format!("{}:", self.letter)];
            let _ = crate::activity::publicar("usn", &recursos, &raices, "running");
            let (aplicados, compactar) = {
                let ov = ix.overlay();
                (ov.aplicados, ov.compactar_si_hace_falta())
            };
            if compactar {
                let _ = crate::activity::pedir_reindexado("overlay grande");
                return Latido::Compactar(aplicados);
            }
            Latido::Aplicados(total)
        }
    }
}

/// Arranca el hilo que mantiene el índice al día. Devuelve el handle para
/// poder cortarlo (por ejemplo al cerrar la ventana).
pub fn arrancar(ix: Arc<Index>, letter: char) -> Option<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("usn".into())
        .spawn(move || {
            let mut vigilante = Vigilante::nuevo(letter);
            loop {
                match vigilante.latir(&ix) {
                    Latido::Reindexar => {
                        eprintln!(
                            "aviso: el diario de cambios de {letter}: se reinició; \
                             hace falta reindexar para no mostrar datos viejos"
                        );
                        // Se sigue vigilando: si el usuario reindexa, el
                        // marcador se reemplaza y esto vuelve a la normalidad.
                        std::thread::sleep(CADA * 30);
                        continue;
                    }
                    Latido::Compactar(n) => {
                        eprintln!(
                            "aviso: el overlay de cambios paso {n}; conviene reindexar                              el volumen (pedido anotado en WizeShared)"
                        );
                        std::thread::sleep(CADA * 30);
                        continue;
                    }
                    Latido::SinPermisos => {
                        // Sin elevación no hay journal: se corta el hilo en vez
                        // de gastar una vuelta cada dos segundos.
                        crate::activity::retirar();
                        return;
                    }
                    _ => {}
                }
                std::thread::sleep(CADA);
            }
        })
        .ok()
}
