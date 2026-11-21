//! Unidades: enumeración y **la misma lógica de selección que WizEverything**.
//!
//! WizEverything guarda en `%APPDATA%\WizEverything\settings.ini` una línea
//! `excluded_drives=C:,H:` (separada por comas, por nombre de unidad y sin
//! distinguir mayúsculas). Census lee esa misma lista, así que por defecto
//! indexa exactamente las unidades que el usuario ya eligió allá; su propia
//! configuración (`%APPDATA%\Census\settings.ini`, misma clave y formato) tiene
//! la última palabra.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriveKind {
    Fixed,
    Removable,
    Remote,
    /// Unidad virtual de un servicio en la nube (Google Drive, OneDrive, ...).
    /// No es un tipo de Windows: se deduce de la etiqueta del volumen.
    Cloud,
    Cdrom,
    Ram,
    Unknown,
}

impl DriveKind {
    pub fn label(&self) -> &'static str {
        match self {
            DriveKind::Fixed => "fijo",
            DriveKind::Removable => "extraíble",
            DriveKind::Remote => "red",
            DriveKind::Cloud => "nube",
            DriveKind::Cdrom => "cd",
            DriveKind::Ram => "ram",
            DriveKind::Unknown => "?",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Drive {
    /// Nombre corto, como lo usan las dos apps: `"C:"`.
    pub name: String,
    /// Lo que dice Windows; `kind()` agrega la detección de nube.
    pub base_kind: DriveKind,
    pub fs: String,
    pub label: String,
    pub total: u64,
    pub free: u64,
}

/// Etiquetas que delatan una unidad montada por un cliente de nube.
const CLOUD_LABELS: [&str; 9] = [
    "google drive",
    "onedrive",
    "dropbox",
    "icloud",
    "pcloud",
    "mega",
    "box",
    "nextcloud",
    "sharepoint",
];

impl Drive {
    /// WizTree separa las unidades de nube del resto; Windows no las distingue,
    /// así que la etiqueta del volumen es la única pista disponible.
    pub fn is_cloud(&self) -> bool {
        let label = self.label.to_lowercase();
        if CLOUD_LABELS.iter().any(|k| label.contains(k)) {
            return true;
        }
        // Google Drive monta la unidad con la etiqueta "cuenta@dominio - G..."
        // (Windows la corta en 32 caracteres) y un FAT32 falso del tamano del
        // disco anfitrion. El arroba en la etiqueta es la pista mas fiable.
        label.contains('@') && label.contains("driv")
            || label.contains('@') && self.fs.eq_ignore_ascii_case("FAT32")
    }

    /// Tipo ya clasificado: nube si la etiqueta lo dice, si no el de Windows.
    pub fn kind(&self) -> DriveKind {
        if self.is_cloud() {
            DriveKind::Cloud
        } else {
            self.base_kind
        }
    }

    /// Se puede indexar: tiene sistema de archivos y no es una unidad óptica
    /// sin disco. Todo lo demás entra, sea local, de red o de nube.
    pub fn is_indexable(&self) -> bool {
        !self.fs.is_empty() && !matches!(self.base_kind, DriveKind::Cdrom)
    }

    /// La MFT sólo existe en NTFS local; el resto va por recorrido.
    pub fn uses_mft(&self) -> bool {
        self.is_ntfs()
            && !self.is_cloud()
            && matches!(self.base_kind, DriveKind::Fixed | DriveKind::Removable)
    }

    pub fn letter(&self) -> char {
        self.name.chars().next().unwrap_or('C')
    }

    /// La MFT sólo existe en NTFS; el resto va por recorrido de directorios.
    pub fn is_ntfs(&self) -> bool {
        self.fs.eq_ignore_ascii_case("NTFS")
    }
}

fn settings_path_for(app: &str) -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    Some(base.join(app).join("settings.ini"))
}

pub fn wiz_settings_path() -> Option<PathBuf> {
    settings_path_for("WizEverything")
}

pub fn census_settings_path() -> Option<PathBuf> {
    settings_path_for("Census")
}

/// Lee `excluded_drives` de un `settings.ini` con el formato de WizEverything.
pub fn parse_excluded(text: &str) -> Vec<String> {
    for line in text.lines() {
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        if k.trim() == "excluded_drives" {
            return v
                .split(',')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(String::from)
                .collect();
        }
    }
    Vec::new()
}

fn read_excluded(path: Option<PathBuf>) -> Vec<String> {
    path.and_then(|p| std::fs::read_to_string(p).ok())
        .map(|t| parse_excluded(&t))
        .unwrap_or_default()
}

/// Unidades excluidas: las de Census mandan; si no hay, las de WizEverything.
pub fn excluded_drives() -> Vec<String> {
    let own = read_excluded(census_settings_path());
    if !own.is_empty() {
        return own;
    }
    read_excluded(wiz_settings_path())
}

pub fn is_excluded(name: &str, excluded: &[String]) -> bool {
    excluded.iter().any(|e| e.trim().eq_ignore_ascii_case(name))
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows_sys::Win32::Storage::FileSystem::{
        GetDiskFreeSpaceExW, GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
    };
    use windows_sys::Win32::System::WindowsProgramming::{
        DRIVE_CDROM, DRIVE_FIXED, DRIVE_RAMDISK, DRIVE_REMOTE, DRIVE_REMOVABLE,
    };

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    /// Número de serie del volumen (`GetVolumeInformationW`). No pide elevación.
    pub fn volume_serial(letter: char) -> Option<u64> {
        let root = format!("{}:\\", letter.to_ascii_uppercase());
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        let mut serial: u32 = 0;
        let ok = unsafe {
            GetVolumeInformationW(
                wide.as_ptr(),
                std::ptr::null_mut(),
                0,
                &mut serial,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                0,
            )
        };
        if ok == 0 {
            return None;
        }
        Some(serial as u64)
    }

    pub fn enumerate() -> Vec<Drive> {
        let mask = unsafe { GetLogicalDrives() };
        let mut out = Vec::new();
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root = wide(&format!("{letter}:\\"));
            let ty = unsafe { GetDriveTypeW(root.as_ptr()) };
            let kind = match ty {
                DRIVE_FIXED => DriveKind::Fixed,
                DRIVE_REMOVABLE => DriveKind::Removable,
                DRIVE_REMOTE => DriveKind::Remote,
                DRIVE_CDROM => DriveKind::Cdrom,
                DRIVE_RAMDISK => DriveKind::Ram,
                _ => DriveKind::Unknown,
            };

            let mut label_buf = [0u16; 64];
            let mut fs_buf = [0u16; 32];
            let ok = unsafe {
                GetVolumeInformationW(
                    root.as_ptr(),
                    label_buf.as_mut_ptr(),
                    label_buf.len() as u32,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    fs_buf.as_mut_ptr(),
                    fs_buf.len() as u32,
                )
            };
            let utf16 = |b: &[u16]| {
                let end = b.iter().position(|u| *u == 0).unwrap_or(b.len());
                String::from_utf16_lossy(&b[..end])
            };
            let (label, fs) = if ok != 0 {
                (utf16(&label_buf), utf16(&fs_buf))
            } else {
                (String::new(), String::new())
            };

            let mut free: u64 = 0;
            let mut total: u64 = 0;
            unsafe {
                GetDiskFreeSpaceExW(root.as_ptr(), std::ptr::null_mut(), &mut total, &mut free);
            }

            out.push(Drive {
                name: format!("{letter}:"),
                base_kind: kind,
                fs,
                label,
                total,
                free,
            });
        }
        out
    }
}

#[cfg(not(windows))]
mod imp {
    use super::*;
    pub fn volume_serial(_letter: char) -> Option<u64> {
        None
    }

    pub fn enumerate() -> Vec<Drive> {
        vec![Drive {
            name: "C:".into(),
            base_kind: DriveKind::Fixed,
            fs: "NTFS".into(),
            label: String::new(),
            total: 0,
            free: 0,
        }]
    }
}

pub use imp::enumerate;
pub use imp::volume_serial;

/// Unidades que Census indexa: **todas las disponibles** (locales, de red y de
/// nube), salvo las que el usuario haya excluido en la configuración de Census.
pub fn selected() -> Vec<Drive> {
    let excluded = read_excluded(census_settings_path());
    enumerate()
        .into_iter()
        .filter(|d| d.is_indexable())
        .filter(|d| !is_excluded(&d.name, &excluded))
        .collect()
}

/// Roster completo con el estado de selección, para mostrarlo en la ventana.
pub fn roster() -> Vec<(Drive, bool)> {
    let excluded = read_excluded(census_settings_path());
    enumerate()
        .into_iter()
        .filter(|d| !d.fs.is_empty())
        .map(|d| {
            let on = d.is_indexable() && !is_excluded(&d.name, &excluded);
            (d, on)
        })
        .collect()
}

/// Guarda la selección de unidades en la configuración de Census.
pub fn save_excluded(names: &[String]) -> Result<(), String> {
    let Some(path) = census_settings_path() else {
        return Err("no hay APPDATA".into());
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    let mut kept: Vec<String> = previous
        .lines()
        .filter(|l| !l.trim_start().starts_with("excluded_drives"))
        .map(|l| l.to_string())
        .collect();
    kept.push(format!("excluded_drives={}", names.join(",")));
    std::fs::write(&path, kept.join("\n") + "\n").map_err(|e| e.to_string())
}

pub fn describe(d: &Drive) -> String {
    let label = if d.label.is_empty() {
        String::new()
    } else {
        format!(" {}", d.label)
    };
    format!("{} ({}{})", d.name, d.fs.to_lowercase(), label)
}

pub fn exists(path: &Path) -> bool {
    path.exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn excluded_list_parses_like_wizeverything() {
        // Formato real: "excluded_drives= , H: ," (con espacios y vacíos).
        let text = "lang=es\ntheme=dark\nexcluded_drives= , H: ,\nclose_to_tray=1\n";
        assert_eq!(parse_excluded(text), vec!["H:".to_string()]);
        assert!(parse_excluded("lang=es\n").is_empty());
        assert!(parse_excluded("").is_empty());
    }

    #[test]
    fn exclusion_is_case_insensitive() {
        let ex = vec!["h:".to_string(), "Z:".into()];
        assert!(is_excluded("H:", &ex));
        assert!(is_excluded("z:", &ex));
        assert!(!is_excluded("C:", &ex));
    }

    #[test]
    fn only_ntfs_local_non_excluded() {
        let d = |name: &str, fs: &str, kind: DriveKind, label: &str| Drive {
            name: name.into(),
            base_kind: kind,
            fs: fs.into(),
            label: label.into(),
            total: 0,
            free: 0,
        };
        assert!(d("C:", "NTFS", DriveKind::Fixed, "").is_ntfs());
        assert!(!d("E:", "exFAT", DriveKind::Removable, "").is_ntfs());
        // La nube se deduce de la etiqueta y se indexa recorriendo carpetas.
        assert!(d("G:", "NTFS", DriveKind::Fixed, "Google Drive").is_cloud());
        assert_eq!(
            d("G:", "NTFS", DriveKind::Fixed, "Google Drive").kind(),
            DriveKind::Cloud
        );
        assert!(!d("G:", "NTFS", DriveKind::Fixed, "Google Drive").uses_mft());
        assert!(d("G:", "NTFS", DriveKind::Fixed, "Google Drive").is_indexable());
        // Red: entra en el índice, sin MFT.
        assert!(d("Z:", "NTFS", DriveKind::Remote, "").is_indexable());
        assert!(!d("Z:", "NTFS", DriveKind::Remote, "").uses_mft());
        // Unidad óptica vacía: afuera.
        assert!(!d("D:", "", DriveKind::Cdrom, "").is_indexable());
    }

    #[test]
    fn enumerate_sees_the_system_drive() {
        let drives = enumerate();
        assert!(!drives.is_empty(), "no se enumeró ninguna unidad");
        assert!(drives.iter().any(|d| d.name == "C:"), "falta C:");
        for d in &drives {
            assert_eq!(d.name.len(), 2, "nombre raro: {}", d.name);
            assert!(d.name.ends_with(':'));
        }
    }
}
