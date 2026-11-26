//! Tipos comunes de plataforma acordados en `docs/PLATAFORMAS.md`.
//!
//! Nada de `String` para nombres: macOS admite nombres que no son UTF-8 y el
//! mismo tipo viaja a Atlas.
//!
//! Identidad, en dos niveles:
//!
//! * `FileKey = (volume_id, ino, gen)` sirve para el lookup del árbol, porque
//!   `ATTR_CMN_PARENTID` no trae el `btime` del padre.
//! * `FileId = (FileKey, btime_ns)` agrega creación para identidad y Atlas
//!   (reutilización de inodo). En Windows `gen = 0` y `btime_ns = 0`.

use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// UUID del volumen. Cero si el filesystem no lo expone.
pub type VolumeUuid = [u8; 16];

/// `volume_id` acordado: FNV-1a 64 en hex minúsculas, o
/// `dir:<hostname>:<path>` para una raíz suelta. `Arc<str>` para que clonarlo
/// por entrada sea barato.
pub type VolumeId = Arc<str>;

pub const FLAG_DIR: u16 = 0x1;
pub const FLAG_ROOT: u16 = 0x2;
pub const FLAG_MOUNT: u16 = 0x4;
pub const FLAG_SYMLINK: u16 = 0x8;
pub const FLAG_HARDLINK: u16 = 0x10;
pub const FLAG_HIDDEN: u16 = 0x20;
pub const FLAG_CLOUD: u16 = 0x40;
pub const FLAG_DATALESS: u16 = 0x80;

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct FileKey {
    pub volume_id: VolumeId,
    pub ino: u64,
    pub gen: u32,
}

impl Default for FileKey {
    fn default() -> Self {
        FileKey {
            volume_id: Arc::from(""),
            ino: 0,
            gen: 0,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
pub struct FileId {
    pub key: FileKey,
    pub btime_ns: i64,
}

impl FileId {
    pub fn new(volume_id: VolumeId, ino: u64, gen: u32, btime_ns: i64) -> Self {
        FileId {
            key: FileKey {
                volume_id,
                ino,
                gen,
            },
            btime_ns,
        }
    }

    pub fn volume_id(&self) -> &str {
        &self.key.volume_id
    }

    pub fn ino(&self) -> u64 {
        self.key.ino
    }

    pub fn gen(&self) -> u32 {
        self.key.gen
    }

    /// Clave de árbol: no incluye `btime`.
    pub fn key(&self) -> FileKey {
        self.key.clone()
    }

    pub fn is_zero(&self) -> bool {
        self.key.ino == 0 && self.key.volume_id.is_empty()
    }

    pub fn hex(&self) -> String {
        format!(
            "{}:{:x}:{:x}",
            self.key.volume_id, self.key.ino, self.key.gen
        )
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.hex())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    #[default]
    Other,
    File,
    Dir,
    Symlink,
}

impl Kind {
    pub fn is_dir(self) -> bool {
        self == Kind::Dir
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::File => "archivo",
            Kind::Dir => "carpeta",
            Kind::Symlink => "enlace",
            Kind::Other => "otro",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Encoding {
    #[default]
    Utf8,
    Utf16Le,
    /// Hay bytes que no son UTF-8 válido. No se pierden: se guardan tal cual y
    /// sólo se normalizan (con reemplazo avisado) al publicar a `C.wcat`.
    Other,
}

/// Una entrada emitida por una fuente, en lotes.
#[derive(Clone, Debug)]
pub struct Entrada {
    pub id: FileId,
    pub parent_id: FileId,
    pub dev_id: u64,
    pub kind: Kind,
    pub size: u64,
    pub alloc: u64,
    pub mtime_ns: i64,
    pub btime_ns: i64,
    pub nlink: u32,
    pub flags: u32,
    pub name: Vec<u8>,
    pub encoding: Encoding,
}

impl Entrada {
    pub fn normalized_flags(&self) -> u16 {
        let mut f = 0u16;
        match self.kind {
            Kind::Dir => f |= FLAG_DIR,
            Kind::Symlink => f |= FLAG_SYMLINK,
            _ => {}
        }
        if self.nlink > 1 {
            f |= FLAG_HARDLINK;
        }
        if self.name.first() == Some(&b'.') {
            f |= FLAG_HIDDEN;
        }
        f
    }
}

/// Motivo de un cambio, neutro para las tres plataformas.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum CambioKind {
    #[default]
    Modify,
    Create,
    Remove,
    Rename,
    Metadata,
    MustRescan,
}

/// Cambio en caliente. En macOS FSEvents da ruta, flags y `eventId`, no inodo:
/// `id` y `parent` quedan en `None` y se resuelve por `path`.
#[derive(Clone, Debug)]
pub struct Cambio {
    pub id: Option<FileId>,
    pub parent: Option<FileId>,
    pub path: Vec<u8>,
    pub nombre: Option<Vec<u8>>,
    pub kind: Kind,
    pub motivo: CambioKind,
    pub marca: Marca,
}

/// Marca de versión del índice. Opaca: sólo la interpreta su plataforma.
///
/// - macOS: `m1:<vol_uuid>:<last_event_id>` o `m1:g<gen>:e<event_id>`.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Marca(pub String);

impl Marca {
    pub fn mac(volume_uuid: VolumeUuid, event_id: u64) -> Self {
        Marca(format!("m1:{}:{}", uuid_hex(&volume_uuid), event_id))
    }

    pub fn mac_generation(gen: u64, event_id: u64) -> Self {
        Marca(format!("m1:g{gen}:e{event_id}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn platform(&self) -> &str {
        self.0.split(':').next().unwrap_or("")
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Generación declarada por la marca, si la plataforma la expone.
    pub fn generation(&self) -> Option<u64> {
        let mut it = self.0.split(':');
        if it.next()? != "m1" {
            return None;
        }
        let next = it.next()?;
        next.strip_prefix('g')?.parse().ok()
    }

    /// Compara dos marcas de la misma plataforma. `None` si no son
    /// comparables (distinta plataforma o formato).
    pub fn cmp_same(&self, other: &Marca) -> Option<std::cmp::Ordering> {
        if self.platform() != other.platform() || self.platform().is_empty() {
            return None;
        }
        match self.platform() {
            "m1" => {
                let a = self.event_id()?;
                let b = other.event_id()?;
                Some(a.cmp(&b))
            }
            _ => Some(self.0.cmp(&other.0)),
        }
    }

    fn event_id(&self) -> Option<u64> {
        let last = self.0.rsplit(':').next()?;
        last.strip_prefix('e').unwrap_or(last).parse().ok()
    }
}

impl fmt::Display for Marca {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stats {
    pub records: u64,
    pub files: u64,
    pub dirs: u64,
    pub skipped: u64,
    pub name_bytes: u64,
    pub build_ms: u64,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdentityState {
    #[default]
    Ok,
    /// Hay montaje pero no se pudo determinar disco/partición: se indexa en
    /// local, no se publica en federación.
    SinIdentidad,
    /// Nube con `cloud_id` atribuido.
    Cloud,
    /// Nube sin cuenta/unidad atribuible: aviso, nunca fusión silenciosa.
    PosibleDuplicado,
}

impl IdentityState {
    /// Etiqueta estable para la CLI y el JSON. Es el mismo texto que produce
    /// `serde` (`rename_all = "snake_case"`): no se cambia después, lo consumen
    /// Sort, Wiz y Atlas.
    pub fn label(self) -> &'static str {
        match self {
            IdentityState::Ok => "ok",
            IdentityState::SinIdentidad => "sin_identidad",
            IdentityState::Cloud => "cloud",
            IdentityState::PosibleDuplicado => "posible_duplicado",
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Volume {
    /// `hash(DiskHwId || offset_particion || fs_uuid)`. La letra o el punto
    /// de montaje **no** entran en la clave.
    pub id: VolumeId,
    /// Identidad del aparato físico: serial/vendor/model/capacidad o, si
    /// falta el serial, el `DiskUUID` del disco entero.
    #[serde(default)]
    pub disk_hw_id: Option<String>,
    #[serde(default)]
    pub disk_guid: Option<String>,
    #[serde(default)]
    pub serial: Option<String>,
    /// Modelo del aparato tal como lo informa el sistema (`MediaName` de
    /// `diskutil`). Es presentación: no entra en `DiskHwId` aparte de la
    /// cadena `vendor_model` que ya lo usa.
    #[serde(default)]
    pub model: Option<String>,
    #[serde(default)]
    pub partition_offset: u64,
    #[serde(default)]
    pub identity_state: IdentityState,
    /// Identidad de nube `hash(provider || account || unidad)`, si se pudo
    /// atribuir. No usa la ruta ni la letra: el mismo drive montado por otra
    /// app o en otra PC da el mismo `cloud_id`.
    #[serde(default)]
    pub cloud_id: Option<String>,
    /// Montaje de nube sin identidad atribuible: se avisa, no se fusiona.
    #[serde(default)]
    pub possible_duplicate: bool,
    pub uuid: VolumeUuid,
    pub name: String,
    /// Punto de montaje: **sólo presentación**. Se refresca en cada escaneo.
    pub mount: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub local: bool,
    pub network: bool,
    pub cloud: bool,
    pub removable: bool,
    pub dev: u64,
    pub method: String,
    pub roots: Vec<String>,
    pub nodes: u64,
}

impl Volume {
    pub fn is_indexable(&self) -> bool {
        !self.network && !self.cloud
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Provenance {
    pub pc_id: String,
    pub host: String,
    pub platform: String,
    pub arch: String,
    pub env: String,
    pub app_version: String,
}

/// FNV-1a 64, el mismo de `C.wcat`, para `volume_id` y `content_hash`.
pub fn fnv1a64(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for part in parts {
        for &b in *part {
            h ^= b as u64;
            h = h.wrapping_mul(0x100000001b3);
        }
    }
    h
}

/// `DiskHwId`: identidad del aparato físico. El serial manda; si falta, el
/// `DiskUUID` del disco entero; si tampoco hay, vendor/model + capacidad.
/// Nunca usa `disk2` ni el punto de montaje.
pub fn disk_hw_id(
    serial: Option<&str>,
    vendor_model: &str,
    capacity: u64,
    disk_guid: Option<&str>,
) -> Option<String> {
    let serial = serial.unwrap_or("").trim().to_ascii_lowercase();
    let guid = disk_guid.unwrap_or("").trim().to_ascii_lowercase();
    let vm = vendor_model.trim().to_ascii_lowercase();
    if serial.is_empty() && guid.is_empty() && vm.is_empty() {
        return None;
    }
    let cap = capacity.to_string();
    let h = fnv1a64(&[
        serial.as_bytes(),
        &[0],
        vm.as_bytes(),
        &[0],
        cap.as_bytes(),
        &[0],
        guid.as_bytes(),
    ]);
    // Contrato §10 y `tests/identity.rs`: FNV-1a 64 en hex minúsculas de 16
    // caracteres, **sin prefijo**. El prefijo `disk:` anterior rompía el
    // contrato que consumen Wiz, Sort y Atlas.
    Some(format!("{h:016x}"))
}

/// `volume_id` = hash(DiskHwId || offset_particion || fs_uuid). El mismo disco
/// en `/Volumes/Backup` o en `/Volumes/Backup 1` da el mismo id.
pub fn volume_identity(disk_hw: &str, partition_offset: u64, fs_uuid: &str) -> VolumeId {
    let off = partition_offset.to_string();
    let h = fnv1a64(&[
        disk_hw.as_bytes(),
        &[0],
        off.as_bytes(),
        &[0],
        fs_uuid.as_bytes(),
    ]);
    // Mismo contrato que `DiskHwId`: 16 hex minúsculas, sin prefijo.
    Arc::from(format!("{h:016x}").as_str())
}

/// Raíz suelta que no es un volumen montado: `dir:<hostname>:<path>`, la
/// forma que fija `docs/ATLAS.md` §10.5. No se hashea porque es la única
/// identidad posible y el usuario la tiene que poder leer.
pub fn dir_volume_id(host: &str, path: &str) -> VolumeId {
    Arc::from(format!("dir:{host}:{path}").as_str())
}

pub fn uuid_hex(u: &VolumeUuid) -> String {
    let mut s = String::with_capacity(32);
    for b in u {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

pub fn parse_uuid_hex(s: &str) -> Option<VolumeUuid> {
    let s: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if s.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

/// UUID de volumen a partir de la forma con guiones de `diskutil`.
pub fn parse_uuid_dashed(s: &str) -> Option<VolumeUuid> {
    parse_uuid_hex(s)
}

/// Letra sintética estable para un volumen: se deriva del `volume_id`
/// ordenado, nunca del orden de montaje. Windows usa `A:`..`Z:`; macOS
/// exporta `A:`.. para que el hub y `C.wcat` tengan una clave estable.
pub fn stable_letter(ids_sorted: &[VolumeId], id: &str) -> char {
    let pos = ids_sorted.iter().position(|v| &**v == id).unwrap_or(0);
    // Empezamos en `A`; si hay más de 26 volúmenes seguimos con minúsculas.
    const LETRAS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    LETRAS[pos % LETRAS.len()] as char
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_key_ignores_birth_time() {
        let vol: VolumeId = Arc::from("0123456789abcdef");
        let a = FileId::new(vol.clone(), 7, 0, 100);
        let b = FileId::new(vol, 7, 0, 999);
        assert_ne!(a, b);
        assert_eq!(a.key(), b.key());
    }

    #[test]
    fn marca_mac_orders_by_event_id() {
        let a = Marca::mac([2; 16], 10);
        let b = Marca::mac([2; 16], 11);
        assert_eq!(a.cmp_same(&b), Some(std::cmp::Ordering::Less));
        assert_eq!(a.cmp_same(&Marca("l1:9:9".into())), None);
        assert_eq!(Marca::mac_generation(4, 99).generation(), Some(4));
    }

    #[test]
    fn uuid_roundtrip() {
        let u = [0xde, 0xad, 0xbe, 0xef, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        assert_eq!(parse_uuid_hex(&uuid_hex(&u)), Some(u));
        assert_eq!(
            parse_uuid_hex("DEADBEEF-0001-0203-0405-060708090A0B"),
            Some(u)
        );
    }

    #[test]
    fn volume_id_is_stable_and_not_the_letter() {
        let hw = disk_hw_id(Some("SN-1"), "Disk", 1000, None).unwrap();
        let a = volume_identity(&hw, 0, "fs-1");
        let b = volume_identity(&hw, 0, "fs-1");
        assert_eq!(a, b);
        // Contrato §10 / `tests/identity.rs`: 16 hex minúsculas, sin prefijo.
        assert_eq!(a.len(), 16, "volume_id fuera de contrato: {a}");
        assert!(a
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert_ne!(a, volume_identity(&hw, 0, "fs-2"));
    }

    #[test]
    fn volume_identity_does_not_use_mount() {
        let hw = disk_hw_id(Some("SERIAL1"), "Seagate Backup", 4_000_000, None).unwrap();
        let a = volume_identity(&hw, 0, "uuid-a");
        let b = volume_identity(&hw, 0, "uuid-a");
        assert_eq!(a, b);
        // Otro disco con el mismo nombre montado en la misma ruta: distinto id.
        let hw2 = disk_hw_id(Some("SERIAL2"), "Seagate Backup", 4_000_000, None).unwrap();
        assert_ne!(a, volume_identity(&hw2, 0, "uuid-a"));
        // Otra partición del mismo disco: distinto offset.
        assert_ne!(a, volume_identity(&hw, 1_000_000, "uuid-a"));
    }

    #[test]
    fn disk_hw_id_prefers_serial_but_accepts_guid() {
        let by_serial = disk_hw_id(Some("ABC"), "NVMe", 1000, None).unwrap();
        let by_guid = disk_hw_id(None, "NVMe", 1000, Some("GUID-1")).unwrap();
        assert_ne!(by_serial, by_guid);
        assert_eq!(by_serial.len(), 16, "DiskHwId fuera de contrato: {by_serial}");
        assert!(by_serial
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        assert!(disk_hw_id(None, "", 0, None).is_none());
    }

    #[test]
    fn stable_letters_do_not_depend_on_mount_order() {
        let ids: Vec<VolumeId> = ["aaa", "bbb", "ccc"]
            .iter()
            .map(|s| Arc::from(*s))
            .collect();
        assert_eq!(stable_letter(&ids, "aaa"), 'A');
        assert_eq!(stable_letter(&ids, "ccc"), 'C');
    }
}
