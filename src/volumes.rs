//! Unidades: enumeración y **la misma lógica de selección que WizEverything**.
//!
//! WizEverything guarda en `%APPDATA%\WizEverything\settings.ini` una línea
//! `excluded_drives=C:,H:` (separada por comas, por nombre de unidad y sin
//! distinguir mayúsculas). CensusIndex lee esa misma lista, así que por defecto
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

    /// Código estable en ASCII para el JSON que consumen Wiz, Sort y Atlas.
    /// `label()` es para mostrar; esto es para máquinas.
    pub fn code(&self) -> &'static str {
        match self {
            DriveKind::Fixed => "fixed",
            DriveKind::Removable => "removable",
            DriveKind::Remote => "remote",
            DriveKind::Cloud => "cloud",
            DriveKind::Cdrom => "cdrom",
            DriveKind::Ram => "ram",
            DriveKind::Unknown => "unknown",
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
    /// Número de serie del volumen (`GetVolumeInformationW`). No es la letra:
    /// dos montajes que lo comparten con el mismo tamaño son virtuales (§9).
    pub serial: u64,
    /// Montaje virtual detectado por compartir serie y tamaño con otro volumen
    /// (Google Drive monta un FAT32 falso del tamaño del disco anfitrión).
    pub nube_virtual: bool,
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
        if self.nube_virtual {
            return true;
        }
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
    /// ¿Entra en el índice por defecto? La nube queda afuera: es streaming y
    /// abrirla dispara descargas. Se enumera sólo con `--include-cloud`.
    pub fn is_indexable(&self) -> bool {
        !self.fs.is_empty() && !matches!(self.base_kind, DriveKind::Cdrom) && !self.is_cloud()
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

/// Un volumen de nube montado por un cliente (Google Drive, OneDrive, rclone...).
///
/// §9: `cloud_id = hash(provider || cuenta || unidad)`. **La letra no entra**: el
/// mismo drive compartido montado en `G:` por Google Drive, en `H:` por rclone y
/// en `/mnt/rclone` por Linux es **un** volumen para Atlas.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudId {
    /// `gdrive`, `onedrive`, `dropbox`... (normalizado, igual en las 3 plataformas).
    pub provider: String,
    /// Correo de la etiqueta, o `desconocida:<letra>` si no se pudo atribuir.
    pub account: String,
    /// `my-drive` o el nombre del drive compartido.
    pub unit: String,
    /// FNV-1a 64 en hex minúsculas, 16 dígitos, sin prefijo.
    pub id: String,
    /// La cuenta no se pudo atribuir: se avisa y **no** se fusiona sola (§9.2).
    pub posible_duplicado: bool,
    pub nota: Option<String>,
}

/// Proveedor canónico de Google Drive, el mismo que usan Mac y Linux.
pub const PROVIDER_GDRIVE: &str = "gdrive";
/// Unidad por defecto: "Mi unidad" / "My Drive" se normaliza a esto.
pub const UNIDAD_MI_DRIVE: &str = "my-drive";

/// Proveedor normalizado. Acepta los alias que aparecen en etiquetas, en
/// nombres de filesystem y en `rclone`, para que el hash coincida entre PCs.
pub fn normalizar_provider(p: &str) -> String {
    let low = p.trim().to_ascii_lowercase();
    match low.as_str() {
        "gdrive" | "google" | "google-drive" | "googledrive" | "gdfuse" | "google drive"
        | "google-drive-ocamlfuse" => PROVIDER_GDRIVE.to_string(),
        "onedrive" | "one-drive" | "one drive" => "onedrive".to_string(),
        "dropbox" => "dropbox".to_string(),
        "icloud" | "icloud drive" | "icloud-drive" => "icloud".to_string(),
        "" => "unknown".to_string(),
        otro => otro.to_string(),
    }
}

/// Cuenta normalizada para el hash: minúsculas y sin el nombre del proveedor
/// pegado por algunas etiquetas (`GoogleDrive-cuenta@x`).
pub fn normalizar_account(a: &str) -> String {
    let mut a = a.trim().to_ascii_lowercase();
    for prefijo in [
        "googledrive-",
        "google drive - ",
        "onedrive-",
        "one drive - ",
        "dropbox-",
        "dropbox - ",
    ] {
        if let Some(resto) = a.strip_prefix(prefijo) {
            a = resto.trim().to_string();
            break;
        }
    }
    for sufijo in [
        " - google drive",
        " - onedrive",
        " - one drive",
        " - dropbox",
    ] {
        if let Some(resto) = a.strip_suffix(sufijo) {
            a = resto.trim().to_string();
            break;
        }
    }
    a
}

/// Unidad normalizada: `Mi unidad` y `My Drive` son la misma (`my-drive`).
pub fn normalizar_unit(u: &str) -> String {
    let u = u.trim().to_ascii_lowercase().replace('\\', "/");
    let u = u.trim_matches('/');
    let u = u
        .strip_prefix("shared drives/")
        .or_else(|| u.strip_prefix("drives compartidos/"))
        .unwrap_or(u);
    let u = u.split_whitespace().collect::<Vec<_>>().join(" ");
    match u.as_str() {
        "my drive" | "mi unidad" | "onedrive" | "dropbox" => UNIDAD_MI_DRIVE.to_string(),
        otro => otro.to_string(),
    }
}

/// Hash normativo de nube (§9.1): FNV-1a 64 sobre `provider\0cuenta\0unidad`,
/// hex minúsculas de 16 dígitos y sin prefijo. El separador NUL evita que
/// `("a", "bc")` y `("ab", "c")` den lo mismo.
///
/// Vector compartido con Mac y Linux:
/// `("gdrive", "alice@gmail.com", "my-drive") -> 28590f656b6fe252`.
pub fn cloud_hash(provider: &str, account: &str, unit: &str) -> String {
    let p = normalizar_provider(provider);
    let a = normalizar_account(account);
    let u = normalizar_unit(unit);
    let h = crate::topology::fnv1a64(&[p.as_bytes(), &[0], a.as_bytes(), &[0], u.as_bytes()]);
    format!("{h:016x}")
}

/// Alias del contrato: `cloud_id = hash(provider || account || unidad)`.
pub fn cloud_id(provider: &str, account: &str, unit: &str) -> String {
    cloud_hash(provider, account, unit)
}

/// Proveedor por palabras de la etiqueta o del filesystem.
fn proveedor_por_texto(texto: &str) -> Option<&'static str> {
    let t = texto.to_lowercase();
    if t.contains("google drive") || t.contains("googledrive") || t.contains("gdrive") {
        return Some(PROVIDER_GDRIVE);
    }
    let pista = t.contains("onedrive")
        || t.contains("one drive")
        || t.contains("dropbox")
        || t.contains("icloud")
        || t.contains("pcloud")
        || t.contains("nextcloud")
        || t.contains("sharepoint")
        || t.contains("mega");
    if !pista {
        return None;
    }
    CLOUD_LABELS
        .iter()
        .find(|k| t.contains(**k))
        .map(|k| match *k {
            "google drive" => PROVIDER_GDRIVE,
            "onedrive" => "onedrive",
            "dropbox" => "dropbox",
            "icloud" => "icloud",
            "pcloud" => "pcloud",
            "mega" => "mega",
            "box" => "box",
            "nextcloud" => "nextcloud",
            _ => "sharepoint",
        })
}

/// ¿El resto de la etiqueta es el nombre del proveedor, posiblemente truncado?
/// Windows corta la etiqueta en 32 caracteres y Google Drive deja
/// `lucio@dominio - G...`: el resto es un prefijo del nombre del proveedor.
fn es_resto_del_proveedor(resto: &str) -> bool {
    let mut r = resto.trim().to_lowercase();
    while r.ends_with('.') || r.ends_with('…') {
        r.pop();
        r = r.trim_end().to_string();
    }
    if r.is_empty() {
        return true;
    }
    let sin_espacios = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    let r = sin_espacios(&r);
    [
        "google drive",
        "googledrive",
        "gdrive",
        "onedrive",
        "dropbox",
        "icloud drive",
        "sharepoint",
    ]
    .iter()
    .any(|nombre| sin_espacios(nombre).starts_with(&r))
}

/// Saca el nombre del proveedor que algunas etiquetas anteponen a la cuenta.
fn limpiar_cuenta(cuenta: &str) -> String {
    let mut c = cuenta.trim().to_string();
    let bajo = c.to_ascii_lowercase();
    for prefijo in [
        "googledrive-",
        "google drive - ",
        "google drive (",
        "onedrive-",
        "dropbox-",
    ] {
        if bajo.starts_with(prefijo) {
            c = c[prefijo.len()..].trim().to_string();
            break;
        }
    }
    let c = c.trim().trim_end_matches(')').trim().to_string();
    if es_resto_del_proveedor(&c) {
        // La etiqueta sólo dice "Google Drive": no hay cuenta que atribuir.
        return String::new();
    }
    c
}

/// Parsea la etiqueta del volumen (§9.1). Devuelve `(provider, cuenta, unidad)`
/// o `None` si no se puede atribuir la cuenta: en ese caso el llamador marca
/// `posible_duplicado` y **no** inventa un id que parezca bueno.
///
/// Formatos reales en esta máquina:
/// - `lucio@gruporiccitelli.com - Google Drive`
/// - `luciomerlo@gmail.com - Google...` (Windows corta en 32 caracteres)
/// - `Google Drive` (sin cuenta)
/// - `E:` (no es nube)
pub fn parse_cloud_label(label: &str) -> Option<(String, String, String)> {
    let l = label.trim();
    if l.is_empty() {
        return None;
    }
    let (izq, der) = match l.split_once(" - ") {
        Some((a, b)) => (a.trim(), Some(b.trim())),
        None => (l, None),
    };
    let mut proveedor = proveedor_por_texto(l);
    if proveedor.is_none() {
        // Etiqueta truncada: `cuenta@dominio - G...`.
        if let Some(der) = der {
            if izq.contains('@') && es_resto_del_proveedor(der) {
                proveedor = Some(PROVIDER_GDRIVE);
            }
        }
    }
    let proveedor = proveedor?;
    let cuenta = limpiar_cuenta(izq);
    if cuenta.is_empty() {
        return None;
    }
    let unidad = match der {
        Some(der) if !es_resto_del_proveedor(der) => der.to_string(),
        _ => UNIDAD_MI_DRIVE.to_string(),
    };
    Some((proveedor.to_string(), cuenta, unidad))
}

/// Volúmenes virtuales de nube detectados por cómo se montan, no por la letra.
///
/// Google Drive para escritorio monta un FAT32 falso del tamaño del disco
/// anfitrión y **con la misma serie de volumen en todos sus montajes**
/// (comprobado en esta máquina: `G:` y `H:` comparten serie y tamaño). Un
/// filesystem real no repite serie entre volúmenes distintos, así que dos
/// volúmenes FAT32 con la misma serie y el mismo tamaño son montajes virtuales.
/// Es sólo una detección: la identidad sigue saliendo de la etiqueta, y si no
/// alcanza queda `desconocida:<letra>` + `posible_duplicado`.
pub fn marcar_nube_por_virtualizacion(drives: &mut [Drive]) {
    const UN_GIB: u64 = 1024 * 1024 * 1024;
    let mut grupos: Vec<(u64, u64, Vec<usize>)> = Vec::new();
    for (i, d) in drives.iter().enumerate() {
        if d.serial == 0 || d.total < UN_GIB || !d.fs.eq_ignore_ascii_case("FAT32") {
            continue;
        }
        match grupos
            .iter_mut()
            .find(|(serial, total, _)| *serial == d.serial && *total == d.total)
        {
            Some((_, _, indices)) => indices.push(i),
            None => grupos.push((d.serial, d.total, vec![i])),
        }
    }
    for (_, _, indices) in grupos {
        if indices.len() >= 2 {
            for i in indices {
                if let Some(d) = drives.get_mut(i) {
                    d.nube_virtual = true;
                }
            }
        }
    }
}

/// Identidad de nube de una unidad, o `None` si no parece nube.
pub fn nube_de(d: &Drive) -> Option<CloudId> {
    if !d.is_cloud() {
        return None;
    }
    if let Some((provider, account, unit)) = parse_cloud_label(&d.label) {
        let id = cloud_id(&provider, &account, &unit);
        return Some(CloudId {
            provider,
            account,
            unit,
            id,
            posible_duplicado: false,
            nota: None,
        });
    }
    let provider = proveedor_por_texto(&d.label)
        .unwrap_or(PROVIDER_GDRIVE)
        .to_string();
    let account = format!("desconocida:{}", d.letter());
    let unit = UNIDAD_MI_DRIVE.to_string();
    let id = cloud_id(&provider, &account, &unit);
    Some(CloudId {
        provider,
        account,
        unit,
        id: id.clone(),
        posible_duplicado: true,
        nota: Some(
            "la etiqueta no alcanza para atribuir la cuenta: se marca posible duplicado y no se fusiona sola (§9.2)"
                .into(),
        ),
    })
}

fn settings_path_for(app: &str) -> Option<PathBuf> {
    let base = std::env::var_os("APPDATA").map(PathBuf::from)?;
    Some(base.join(app).join("settings.ini"))
}

pub fn wiz_settings_path() -> Option<PathBuf> {
    settings_path_for("WizEverything")
}

pub fn censusindex_settings_path() -> Option<PathBuf> {
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

/// Unidades excluidas: las de CensusIndex mandan; si no hay, las de WizEverything.
pub fn excluded_drives() -> Vec<String> {
    let own = read_excluded(censusindex_settings_path());
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
            let mut serial: u32 = 0;
            let ok = unsafe {
                GetVolumeInformationW(
                    root.as_ptr(),
                    label_buf.as_mut_ptr(),
                    label_buf.len() as u32,
                    &mut serial,
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
                serial: serial as u64,
                nube_virtual: false,
            });
        }
        marcar_nube_por_virtualizacion(&mut out);
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
            serial: 0,
            nube_virtual: false,
        }]
    }
}

pub use imp::enumerate;
pub use imp::volume_serial;

/// Unidades que CensusIndex indexa: **todas las disponibles** (locales, de red y de
/// nube), salvo las que el usuario haya excluido en la configuración de CensusIndex.
pub fn selected() -> Vec<Drive> {
    let excluded = read_excluded(censusindex_settings_path());
    enumerate()
        .into_iter()
        .filter(|d| d.is_indexable())
        .filter(|d| !is_excluded(&d.name, &excluded))
        .collect()
}

/// Roster completo con el estado de selección, para mostrarlo en la ventana.
pub fn roster() -> Vec<(Drive, bool)> {
    let excluded = read_excluded(censusindex_settings_path());
    enumerate()
        .into_iter()
        .filter(|d| !d.fs.is_empty())
        .map(|d| {
            let on = d.is_indexable() && !is_excluded(&d.name, &excluded);
            (d, on)
        })
        .collect()
}

/// Guarda la selección de unidades en la configuración de CensusIndex.
pub fn save_excluded(names: &[String]) -> Result<(), String> {
    let Some(path) = censusindex_settings_path() else {
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

/// Lee una clave de `%APPDATA%\Census\settings.ini`.
pub fn get_setting(key: &str) -> Option<String> {
    let texto = std::fs::read_to_string(censusindex_settings_path()?).ok()?;
    for line in texto.lines() {
        let (k, v) = line.split_once('=')?;
        if k.trim() == key {
            return Some(v.trim().to_string());
        }
    }
    None
}

/// Escribe (o reemplaza) una clave, conservando el resto del archivo.
pub fn set_setting(key: &str, value: &str) -> std::io::Result<()> {
    let path = censusindex_settings_path()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no hay APPDATA"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let previo = std::fs::read_to_string(&path).unwrap_or_default();
    let mut lineas: Vec<String> = previo
        .lines()
        .filter(|l| {
            l.split_once('=')
                .map(|(k, _)| k.trim() != key)
                .unwrap_or(true)
        })
        .map(String::from)
        .collect();
    lineas.push(format!("{key}={value}"));
    std::fs::write(
        &path,
        lineas.join(
            "
",
        ) + "
",
    )
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
            serial: 0,
            nube_virtual: false,
        };
        assert!(d("C:", "NTFS", DriveKind::Fixed, "").is_ntfs());
        assert!(!d("E:", "exFAT", DriveKind::Removable, "").is_ntfs());
        // La nube se deduce de la etiqueta y **no** entra al índice por defecto.
        assert!(d("G:", "NTFS", DriveKind::Fixed, "Google Drive").is_cloud());
        assert_eq!(
            d("G:", "NTFS", DriveKind::Fixed, "Google Drive").kind(),
            DriveKind::Cloud
        );
        assert!(!d("G:", "NTFS", DriveKind::Fixed, "Google Drive").uses_mft());
        assert!(
            !d("G:", "NTFS", DriveKind::Fixed, "Google Drive").is_indexable(),
            "la nube es streaming: no se indexa sola"
        );
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

    // -----------------------------------------------------------------
    // Nube (§9): etiquetas reales de esta máquina
    // -----------------------------------------------------------------

    #[test]
    fn parsea_la_etiqueta_completa_de_google_drive() {
        // Etiqueta real de `G:` en esta máquina.
        let (provider, account, unit) =
            parse_cloud_label("lucio@gruporiccitelli.com - Google Drive").expect("debe parsear");
        assert_eq!(provider, PROVIDER_GDRIVE);
        assert_eq!(account, "lucio@gruporiccitelli.com");
        assert_eq!(unit, UNIDAD_MI_DRIVE);

        // Etiqueta real de `H:`: Windows la corta en 32 caracteres.
        let (provider, account, unit) =
            parse_cloud_label("luciomerlo@gmail.com - Google...").expect("debe parsear");
        assert_eq!(provider, PROVIDER_GDRIVE);
        assert_eq!(account, "luciomerlo@gmail.com");
        assert_eq!(unit, UNIDAD_MI_DRIVE);

        // La etiqueta de `G:` tal como la ve CensusIndex hoy (`- G...`).
        let (_, account, unit) =
            parse_cloud_label("lucio@gruporiccitelli.com - G...").expect("debe parsear");
        assert_eq!(account, "lucio@gruporiccitelli.com");
        assert_eq!(unit, UNIDAD_MI_DRIVE);
    }

    #[test]
    fn etiqueta_sin_cuenta_no_inventa_identidad() {
        // "Google Drive" a secas no dice qué cuenta es.
        assert_eq!(parse_cloud_label("Google Drive"), None);
        assert_eq!(parse_cloud_label("google drive"), None);
        assert_eq!(parse_cloud_label(""), None);
        // Una letra sola no es una etiqueta de nube.
        assert_eq!(parse_cloud_label("E:"), None);
        assert_eq!(parse_cloud_label("Disco local"), None);
    }

    #[test]
    fn cloud_id_usa_el_vector_compartido_con_mac_y_linux() {
        // Vector normativo del contrato entre plataformas: FNV-1a 64 sobre
        // `provider\0cuenta\0unidad`, hex minúsculas de 16 dígitos.
        assert_eq!(
            cloud_id("gdrive", "alice@gmail.com", "my-drive"),
            "28590f656b6fe252"
        );
        // Los alias del proveedor y de la unidad no cambian el id.
        assert_eq!(
            cloud_id("google-drive", "Alice@Gmail.com", "Mi unidad"),
            cloud_id("gdrive", "alice@gmail.com", "my-drive")
        );
        // La letra de unidad no entra: el mismo drive en G: o H: es el mismo.
        let g = nube_de(&drive("G:", "lucio@gruporiccitelli.com - G...")).expect("G: es nube");
        let h = nube_de(&drive("H:", "lucio@gruporiccitelli.com - Google Drive"))
            .expect("H: es nube");
        assert_eq!(g.id, h.id, "la letra no puede cambiar el cloud_id");
        assert_eq!(h.id, "2680377977155f73");
    }

    #[test]
    fn nube_sin_cuenta_queda_desconocida_y_marcada() {
        let g = nube_de(&drive("G:", "Google Drive")).expect("es nube por la etiqueta");
        assert_eq!(g.provider, PROVIDER_GDRIVE);
        assert_eq!(g.account, "desconocida:G");
        assert!(g.posible_duplicado);
        assert!(g.nota.is_some());
        // Nunca se inventa un id que parezca bueno: difiere del atribuido y de
        // otra letra desconocida, así no se fusionan dos montajes sin cuenta.
        let h = nube_de(&drive("H:", "Google Drive")).expect("es nube");
        assert_ne!(g.id, h.id);
        assert_ne!(g.id, cloud_id("gdrive", "alice@gmail.com", "my-drive"));
    }

    #[test]
    fn una_unidad_normal_no_es_nube() {
        assert!(nube_de(&drive("C:", "")).is_none());
        assert!(nube_de(&drive("E:", "Backup Seagate")).is_none());
        assert!(nube_de(&drive("D:", "Google Drive")).is_some());
        // OneDrive también se detecta, con su propio provider.
        let od = nube_de(&drive("I:", "lucio@empresa.com - OneDrive")).expect("es nube");
        assert_eq!(od.provider, "onedrive");
        assert_eq!(od.account, "lucio@empresa.com");
    }

    #[test]
    fn drive_virtual_por_compartir_serie_y_tamano() {
        // Comprobado en esta máquina: G: y H: (Google Drive) comparten la serie
        // 428019990 y el tamaño del disco anfitrión. Es un FAT32 virtual.
        let mut drives = vec![
            drive_fat32("C:", "", 912_056_151, 254_854_295_552),
            drive_fat32("G:", "lucio@gruporiccitelli.com - G...", 428_019_990, 254_854_295_552),
            drive_fat32("H:", "luciomerlo@gmail.com - Google...", 428_019_990, 254_854_295_552),
            drive_fat32("E:", "USB DISK", 123_456_789, 254_854_295_552),
        ];
        marcar_nube_por_virtualizacion(&mut drives);
        assert!(!drives[0].nube_virtual, "C: no comparte serie con nadie");
        assert!(drives[1].nube_virtual, "G: y H: comparten serie y tamaño");
        assert!(drives[2].nube_virtual);
        assert!(!drives[3].nube_virtual, "una serie única no es virtual");
        assert_eq!(drives[1].kind(), DriveKind::Cloud);
        assert!(!drives[1].is_indexable());
    }

    fn drive(name: &str, label: &str) -> Drive {
        Drive {
            name: name.into(),
            base_kind: DriveKind::Fixed,
            fs: "FAT32".into(),
            label: label.into(),
            total: 254_854_295_552,
            free: 0,
            serial: 428_019_990,
            nube_virtual: false,
        }
    }

    fn drive_fat32(name: &str, label: &str, serial: u64, total: u64) -> Drive {
        Drive {
            name: name.into(),
            base_kind: DriveKind::Fixed,
            fs: "FAT32".into(),
            label: label.into(),
            total,
            free: 0,
            serial,
            nube_virtual: false,
        }
    }
}
