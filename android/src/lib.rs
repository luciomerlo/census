//! CensusIndex para Android.
//!
//! La app (Kotlin) decide **cuando** mirar y **donde**; este nucleo decide
//! **como** actualizar el indice con el menor trabajo posible:
//!
//! * [`delta::rescan`] reconstruye el indice reutilizando lo que no cambio:
//!   - `Mode::Sweep`: barrido podado por `mtime` de carpeta (un `stat` por
//!     carpeta, `getdents`+`statx` solo donde cambio);
//!   - `Mode::Dirty`: relista solo las carpetas que la app declara sucias
//!     (por `MediaStore`, `inotify` o `ContentObserver`) y copia el resto.
//! * [`engine::Engine`] guarda el estado (indice + snapshot `.cns`).
//! * [`jni`] expone el motor a Kotlin (solo en Android).

pub mod delta;
pub mod engine;
#[cfg(target_os = "android")]
pub mod jni;
pub mod volumes;
