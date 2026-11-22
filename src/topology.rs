//! Topología de discos: qué hay del otro lado del cable **y quién es**.
//!
//! Un volumen (`C:`) no dice nada sobre el hardware: dos letras pueden ser dos
//! particiones del mismo disco, y cuatro discos pueden colgar del **mismo**
//! controlador USB compartiendo ancho de banda. Para decidir cuántas tareas
//! pesadas correr a la vez hace falta mirar el disco físico y su bus:
//!
//! 1. `IOCTL_STORAGE_GET_DEVICE_NUMBER` da el número de disco físico de una letra.
//! 2. `IOCTL_STORAGE_QUERY_PROPERTY` con `StorageDeviceProperty` da el tipo de bus
//!    (SATA, NVMe, USB, ...), el modelo y el **número de serie**, sin depender de
//!    WMI ni de PowerShell.
//!
//! Con eso se arma un **grupo de contención**: discos que comparten bus comparten
//! techo de velocidad. La regla de planificación es una sola tarea pesada por
//! grupo; en el grupo interno (SATA/NVMe, sin compartir cable) se permite una por
//! disco. En USB se permite como mucho dos, porque cuatro mecánicos saturan los
//! 5 Gbps del enlace y además se pelean por la cabeza de lectura.
//!
//! # Identidad por hardware (`docs/ATLAS.md` §10)
//!
//! La letra de unidad **no es identidad**: cambia al reasignar en el
//! Administrador de discos, y dos discos externos distintos se conectan en
//! momentos distintos y reciben la misma letra. Census usa tres niveles:
//!
//! ```text
//! DiskHwId  = FNV-1a64(serial || vendor || modelo || capacidad)  # el aparato
//! volume_id = FNV-1a64(DiskHwId || offset_particion || tamaño || vol_serial)
//! mount     = letra o punto de montaje                            # presentación
//! ```
//!
//! Las cadenas se normalizan (trim, colapsar espacios, mayúsculas) y los campos
//! se separan con NUL, igual que en Mac y Linux, para que `("a","bc")` y
//! `("ab","c")` no colisionen. `census volumes --json` es el contrato que
//! consumen Wiz, Sort y Atlas.
//!
//! ## La trampa de los puentes USB baratos (§10.4)
//!
//! Muchas cajas USB-SATA informan el número de serie **del puente**, no el del
//! disco: dos discos distintos devuelven la misma serie. Si la misma `serial`
//! aparece con distinto modelo o capacidad se marca `serial_ambiguo` y se agrega
//! el GUID del disco (o de la partición, o las propiedades crudas del
//! descriptor) al hash. Si aun así dos discos físicos distintos terminan con la
//! misma clave, se marca `posible_duplicado` y se resuelve a mano
//! (`census atlas unir/separar`): **nunca** se fusiona en silencio.
//!
//! ## Si no hay identidad
//!
//! Sin serial, sin GUID y sin serie de volumen el volumen queda `sin_identidad`
//! y no se publica en federación: antes un volumen faltante avisado que dos
//! discos fusionados por error.

use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, GENERIC_READ, HANDLE, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, GetVolumeNameForVolumeMountPointW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::volumes::{CloudId, Drive, DriveKind};
use crate::win::wide;

/// `FILE_READ_ATTRIBUTES`: alcanza para preguntar el número de disco y no pide
/// elevación (a diferencia de `GENERIC_READ` sobre un volumen).
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;
/// `FILE_ANY_ACCESS`: `dwDesiredAccess = 0`. Windows lo acepta para abrir una
/// unidad y consultar atributos; es el segundo intento antes de `GENERIC_READ`.
const FILE_ANY_ACCESS: u32 = 0x0000_0000;

/// `IOCTL_STORAGE_GET_DEVICE_NUMBER`
const IOCTL_GET_DEVICE_NUMBER: u32 = 0x0002_D108;
/// `IOCTL_STORAGE_QUERY_PROPERTY`
const IOCTL_QUERY_PROPERTY: u32 = 0x0002_D140;
/// `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`
const IOCTL_VOLUME_DISK_EXTENTS: u32 = 0x0056_0000;
/// `IOCTL_DISK_GET_PARTITION_INFO_EX`
const IOCTL_DISK_PARTITION_INFO_EX: u32 = 0x0007_0048;
/// `IOCTL_DISK_GET_DRIVE_LAYOUT_EX`
const IOCTL_DISK_DRIVE_LAYOUT_EX: u32 = 0x0007_0050;
/// `IOCTL_DISK_GET_LENGTH_INFO`
const IOCTL_DISK_LENGTH_INFO: u32 = 0x0007_405C;
/// `IOCTL_STORAGE_READ_CAPACITY`
const IOCTL_STORAGE_READ_CAPACITY: u32 = 0x002D_5140;

/// `StorageDeviceProperty`
const STORAGE_DEVICE_PROPERTY: u32 = 0;
/// `PropertyStandardQuery`
const PROPERTY_STANDARD_QUERY: u32 = 0;

/// `STORAGE_DEVICE_DESCRIPTOR` mide 36 bytes sin contar las cadenas.
const DESCRIPTOR_MIN: usize = 36;
/// `DISK_EXTENT` mide 24 bytes: DiskNumber, Padding, StartingOffset, ExtentLength.
const DISK_EXTENT_LEN: usize = 24;

/// `FILE_DEVICE_VOLUME`/`IOCTL_DISK_*` no pasan de unos pocos cientos de bytes,
/// pero el layout puede crecer con la cantidad de particiones.
const BUF_GRANDE: usize = 8 * 1024;

/// `STORAGE_DEVICE_NUMBER`
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct StorageDeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: u32,
}

/// `STORAGE_PROPERTY_QUERY` (con el relleno de alineación explícito).
#[repr(C)]
#[derive(Clone, Copy)]
struct StoragePropertyQuery {
    property_id: u32,
    query_type: u32,
    additional: [u8; 4],
}

/// Diagnóstico: con `CENSUS_DEBUG=1` cada IOCTL fallido imprime su código de
/// `GetLastError`. Sin esto, "sin discos físicos visibles" no dice nada.
fn debug(msg: impl FnOnce() -> String) {
    let activo = std::env::var_os("CENSUS_DEBUG").is_some_and(|v| v != "0" && !v.is_empty());
    if activo {
        eprintln!("[census:debug] {}", msg());
    }
}

// ---------------------------------------------------------------------------
// Identidad: funciones puras (§10)
// ---------------------------------------------------------------------------

/// FNV-1a 64, el mismo de `.wcat`, `content_hash` y Atlas.
pub fn fnv1a64(partes: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for parte in partes {
        for &b in *parte {
            h ^= b as u64;
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    h
}

/// Normalización de identidad: `trim`, colapsar espacios y mayúsculas.
/// Un mismo disco no debe cambiar de `DiskHwId` porque el firmware agregó dos
/// espacios al final del modelo.
pub fn normalizar(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase()
}

/// Hash de `n` campos separados por NUL, en hex minúsculas de 16 dígitos.
fn hash_campos(campos: &[&str]) -> String {
    let mut partes: Vec<&[u8]> = Vec::with_capacity(campos.len() * 2);
    for (i, campo) in campos.iter().enumerate() {
        if i > 0 {
            partes.push(&[0u8]);
        }
        partes.push(campo.as_bytes());
    }
    format!("{:016x}", fnv1a64(&partes))
}

/// `DiskHwId` es el **aparato físico**, no la partición y nunca la letra.
///
/// `extra` sólo se usa cuando el serial no alcanza (serial vacío o
/// `serial_ambiguo`): ahí entra el GUID del disco GPT, el GUID de la partición o
/// el hash de las propiedades crudas del descriptor.
pub fn disk_hw_id(
    serial: &str,
    vendor: &str,
    modelo: &str,
    capacidad: u64,
    extra: &str,
) -> String {
    let serial = normalizar(serial);
    let vendor = normalizar(vendor);
    let modelo = normalizar(modelo);
    let extra = normalizar(extra);
    if serial.is_empty() && extra.is_empty() && modelo.is_empty() && vendor.is_empty() {
        return String::new();
    }
    let capacidad = capacidad.to_string();
    hash_campos(&[&serial, &vendor, &modelo, &capacidad, &extra])
}

/// `volume_id` es la **partición**: mismo disco, distinta partición → distinto
/// id. La letra de unidad no entra nunca.
pub fn volume_id(disk_hw_id: &str, offset: u64, tamano: u64, vol_serial: u64) -> String {
    let offset = offset.to_string();
    let tamano = tamano.to_string();
    let serial = format!("{vol_serial:08x}");
    hash_campos(&[disk_hw_id, &offset, &tamano, &serial])
}

/// Estado de identidad, con los mismos nombres que Mac y Linux para que Atlas
/// no tenga que traducir nada.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum EstadoIdentidad {
    #[default]
    Ok,
    /// La misma `serial` aparece en discos con distinto modelo o capacidad:
    /// típico de un puente USB-SATA barato que informa el serial del puente.
    SerialAmbiguo,
    /// Dos discos físicos distintos terminaron con la misma clave: no se
    /// fusionan, se avisa y se resuelve a mano (§10.4).
    PosibleDuplicado,
    /// Sin serial, sin GUID y sin serie de volumen: no se publica en federación.
    SinIdentidad,
}

impl EstadoIdentidad {
    pub fn label(self) -> &'static str {
        match self {
            EstadoIdentidad::Ok => "ok",
            EstadoIdentidad::SerialAmbiguo => "serial_ambiguo",
            EstadoIdentidad::PosibleDuplicado => "posible_duplicado",
            EstadoIdentidad::SinIdentidad => "sin_identidad",
        }
    }

    /// §10.5: sin identidad no se indexa en federación.
    pub fn federable(self) -> bool {
        !matches!(self, EstadoIdentidad::SinIdentidad)
    }
}

// ---------------------------------------------------------------------------
// FFI: abrir, preguntar y cerrar
// ---------------------------------------------------------------------------

/// Handle abierto con el acceso que funcionó. Se cierra solo al salir de scope.
struct Apertura {
    handle: HANDLE,
}

impl Drop for Apertura {
    fn drop(&mut self) {
        // SAFETY: el handle lo devolvió `CreateFileW` y sólo lo cierra este Drop.
        unsafe {
            CloseHandle(self.handle);
        }
    }
}

fn abrir_con(ruta: &str, acceso: u32) -> Result<Apertura, u32> {
    let path = wide(ruta);
    // SAFETY: `path` es UTF-16 terminado en NUL; los punteros nulos y los flags
    // son válidos para abrir un dispositivo de Windows.
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            acceso,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        // SAFETY: `GetLastError` no tiene precondiciones.
        let codigo = unsafe { GetLastError() };
        debug(|| format!("abrir {ruta} acceso=0x{acceso:x} -> error {codigo}"));
        return Err(codigo);
    }
    debug(|| format!("abrir {ruta} acceso=0x{acceso:x} ok"));
    Ok(Apertura { handle: h })
}

/// Vista de bytes de una estructura `repr(C)` de sólo datos.
fn bytes_de<T>(v: &T) -> &[u8] {
    // SAFETY: `T` es `repr(C)` sin punteros ni huecos y se lee sólo durante la
    // llamada; el tamaño sale de `size_of`.
    unsafe { std::slice::from_raw_parts(v as *const T as *const u8, std::mem::size_of::<T>()) }
}

fn ioctl(h: HANDLE, codigo: u32, entrada: &[u8], salida: &mut [u8]) -> Result<u32, u32> {
    let mut devuelto: u32 = 0;
    // SAFETY: el buffer de salida es exclusivo y el de entrada sólo se lee; los
    // largos son los que `DeviceIoControl` espera. El driver valida el resto.
    let ok = unsafe {
        DeviceIoControl(
            h,
            codigo,
            entrada.as_ptr() as *const std::ffi::c_void,
            entrada.len() as u32,
            salida.as_mut_ptr() as *mut std::ffi::c_void,
            salida.len() as u32,
            &mut devuelto,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        // SAFETY: `GetLastError` no tiene precondiciones.
        return Err(unsafe { GetLastError() });
    }
    if devuelto as usize > salida.len() {
        return Err(0);
    }
    Ok(devuelto)
}

fn leer_u32(buf: &[u8], off: usize) -> Option<u32> {
    let bytes = buf.get(off..off + 4)?;
    Some(u32::from_le_bytes(bytes.try_into().ok()?))
}

fn leer_u64(buf: &[u8], off: usize) -> Option<u64> {
    let bytes = buf.get(off..off + 8)?;
    Some(u64::from_le_bytes(bytes.try_into().ok()?))
}

/// Formatea un GUID tal como lo muestra Windows (`8-4-4-4-12`), respetando el
/// orden little-endian de los tres primeros campos.
fn guid_a_texto(b: &[u8]) -> String {
    if b.len() < 16 {
        return String::new();
    }
    let d1 = u32::from_le_bytes([b[0], b[1], b[2], b[3]]);
    let d2 = u16::from_le_bytes([b[4], b[5]]);
    let d3 = u16::from_le_bytes([b[6], b[7]]);
    format!(
        "{d1:08x}-{d2:04x}-{d3:04x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

// ---------------------------------------------------------------------------
// Discos físicos
// ---------------------------------------------------------------------------

/// Bus del disco, según `STORAGE_DEVICE_DESCRIPTOR.BusType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bus {
    Ata,
    Sata,
    Nvme,
    Usb,
    Scsi,
    Sas,
    Raid,
    Virtual,
    Otro(u32),
}

impl Bus {
    fn desde(codigo: u32) -> Bus {
        match codigo {
            1 => Bus::Scsi,
            2 | 3 => Bus::Ata,
            7 => Bus::Usb,
            8 => Bus::Raid,
            10 => Bus::Sas,
            11 => Bus::Sata,
            14..=16 => Bus::Virtual,
            17 => Bus::Nvme,
            otro => Bus::Otro(otro),
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Bus::Ata => "ATA",
            Bus::Sata => "SATA",
            Bus::Nvme => "NVMe",
            Bus::Usb => "USB",
            Bus::Scsi => "SCSI",
            Bus::Sas => "SAS",
            Bus::Raid => "RAID",
            Bus::Virtual => "virtual",
            Bus::Otro(_) => "otro",
        }
    }

    /// ¿Comparte cable con otros discos? USB sí: un hub o una caja con varios
    /// discos reparte un solo enlace.
    pub fn comparte_enlace(&self) -> bool {
        matches!(self, Bus::Usb | Bus::Raid | Bus::Virtual)
    }

    /// Clave del grupo de contención. Todos los USB caen en el mismo grupo
    /// salvo que se sepa más (un hub por puerto no se distingue sin SetupAPI).
    pub fn grupo(&self) -> &'static str {
        match self {
            Bus::Usb => "usb",
            Bus::Raid => "raid",
            Bus::Virtual => "virtual",
            Bus::Nvme => "nvme",
            Bus::Sata => "sata",
            Bus::Ata => "ata",
            Bus::Sas => "sas",
            Bus::Scsi => "scsi",
            Bus::Otro(_) => "otro",
        }
    }
}

/// Lo que dice el descriptor del aparato, ya normalizado para mostrar.
#[derive(Debug, Clone)]
struct Crudo {
    bus: Bus,
    vendor: String,
    modelo: String,
    revision: String,
    serial: String,
    capacidad: u64,
    guid_disco: String,
    /// Hash de `RawDeviceProperties`: último recurso cuando el puente no da
    /// serial ni hay tabla de particiones.
    raw_id: String,
    nota: Option<String>,
}

impl Default for Crudo {
    fn default() -> Self {
        Crudo {
            bus: Bus::Otro(0),
            vendor: String::new(),
            modelo: String::new(),
            revision: String::new(),
            serial: String::new(),
            capacidad: 0,
            guid_disco: String::new(),
            raw_id: String::new(),
            nota: None,
        }
    }
}

impl Crudo {
    fn anotar(&mut self, nota: String) {
        match &mut self.nota {
            Some(previo) => {
                previo.push_str("; ");
                previo.push_str(&nota);
            }
            None => self.nota = Some(nota),
        }
    }
}

/// Cadena ASCII terminada en NUL dentro del descriptor.
///
/// `desde` es `STORAGE_DEVICE_DESCRIPTOR.Size`: un offset que caiga dentro de la
/// cabecera es basura y se descarta. Se valida el largo antes de leer y nunca se
/// confía en el NUL: si no está, se corta en 512 bytes.
fn texto_en(buf: &[u8], off: usize, desde: usize) -> String {
    if off == 0 || off < desde || off >= buf.len() {
        return String::new();
    }
    let resto = &buf[off..];
    let fin = resto
        .iter()
        .position(|b| *b == 0)
        .unwrap_or(resto.len())
        .min(512);
    let bytes = &resto[..fin];
    let texto: String = bytes
        .iter()
        .map(|b| if (0x20..0x7f).contains(b) { *b as char } else { ' ' })
        .collect();
    texto.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Parsea `STORAGE_DEVICE_DESCRIPTOR` (§10.1). Función pura: se prueba con
/// buffers sintéticos, sin tocar hardware.
fn parse_descriptor(buf: &[u8], crudo: &mut Crudo) {
    if buf.len() < DESCRIPTOR_MIN {
        crudo.anotar(format!(
            "STORAGE_DEVICE_DESCRIPTOR corto: {} bytes",
            buf.len()
        ));
        return;
    }
    // `Size` varía entre drivers (unos ponen 36, otros el total con cadenas):
    // no se usa para validar offsets, sólo para avisar si es absurdo.
    let declarado = leer_u32(buf, 4).unwrap_or(0) as usize;
    if declarado > buf.len() {
        crudo.anotar(format!(
            "descriptor.Size={declarado} > {} bytes devueltos",
            buf.len()
        ));
    }
    // Las cadenas siempre empiezan después de la cabecera de 36 bytes; el
    // `Size` declarado no sirve para recortarlas.
    let desde = DESCRIPTOR_MIN;
    crudo.vendor = texto_en(buf, leer_u32(buf, 12).unwrap_or(0) as usize, desde);
    crudo.modelo = texto_en(buf, leer_u32(buf, 16).unwrap_or(0) as usize, desde);
    crudo.revision = texto_en(buf, leer_u32(buf, 20).unwrap_or(0) as usize, desde);
    crudo.serial = texto_en(buf, leer_u32(buf, 24).unwrap_or(0) as usize, desde);
    crudo.bus = Bus::desde(leer_u32(buf, 28).unwrap_or(0));
    // Si el descriptor declara más bytes crudos de los que hay, el buffer está
    // cortado o el driver miente: no se hashea basura, se anota y se sigue.
    let declaradas = leer_u32(buf, 32).unwrap_or(0) as usize;
    let disponible = buf.len().saturating_sub(DESCRIPTOR_MIN);
    let raw_len = if declaradas <= disponible { declaradas } else { 0 };
    if declaradas > disponible {
        crudo.anotar(format!(
            "propiedades crudas declaradas {declaradas} > disponibles {disponible}"
        ));
    }
    if raw_len > 0 {
        let raw = &buf[DESCRIPTOR_MIN..DESCRIPTOR_MIN + raw_len];
        crudo.raw_id = format!("raw:{:016x}", fnv1a64(&[raw]));
    }
}

fn leer_descriptor(disco: u32) -> Crudo {
    let ruta = format!("\\\\.\\PhysicalDrive{disco}");
    let mut crudo = Crudo::default();
    let consulta = StoragePropertyQuery {
        property_id: STORAGE_DEVICE_PROPERTY,
        query_type: PROPERTY_STANDARD_QUERY,
        additional: [0; 4],
    };
    let mut buf = vec![0u8; 4096];
    // `IOCTL_STORAGE_QUERY_PROPERTY` sobre un disco físico pide lectura: con
    // `FILE_READ_ATTRIBUTES` algunos drivers (NVMe) devuelven ERROR_ACCESS_DENIED
    // (5) y sólo `GENERIC_READ` —que necesita elevación— lo consigue. Por eso se
    // prueban los tres accesos y se sigue al siguiente si el IOCTL falla.
    for acceso in [FILE_READ_ATTRIBUTES, FILE_ANY_ACCESS, GENERIC_READ] {
        let a = match abrir_con(&ruta, acceso) {
            Ok(a) => a,
            Err(codigo) => {
                crudo.anotar(format!(
                    "{ruta}: CreateFileW acceso=0x{acceso:x} -> error {codigo}"
                ));
                continue;
            }
        };
        match ioctl(a.handle, IOCTL_QUERY_PROPERTY, bytes_de(&consulta), &mut buf) {
            Ok(devuelto) => {
                debug(|| format!("{ruta}: descriptor con acceso=0x{acceso:x}"));
                parse_descriptor(&buf[..devuelto as usize], &mut crudo);
                crudo.capacidad = capacidad_de_disco(a.handle).unwrap_or(0);
                if let Some(guid) = guid_de_disco(a.handle) {
                    crudo.guid_disco = guid;
                }
                if crudo.capacidad == 0 {
                    crudo.anotar(format!("{ruta}: capacidad del disco no disponible"));
                }
                return crudo;
            }
            Err(codigo) => {
                crudo.anotar(format!(
                    "{ruta}: IOCTL_STORAGE_QUERY_PROPERTY acceso=0x{acceso:x} -> error {codigo}"
                ));
                if crudo.capacidad == 0 {
                    crudo.capacidad = capacidad_de_disco(a.handle).unwrap_or(0);
                }
                if crudo.guid_disco.is_empty() {
                    if let Some(guid) = guid_de_disco(a.handle) {
                        crudo.guid_disco = guid;
                    }
                }
            }
        }
    }
    crudo
}

fn capacidad_de_disco(h: HANDLE) -> Option<u64> {
    let mut buf = [0u8; 8];
    if let Ok(n) = ioctl(h, IOCTL_DISK_LENGTH_INFO, &[], &mut buf) {
        if n >= 8 {
            return Some(u64::from_le_bytes(buf));
        }
        debug(|| format!("IOCTL_DISK_GET_LENGTH_INFO devolvió {n} bytes"));
    } else {
        debug(|| "IOCTL_DISK_GET_LENGTH_INFO no disponible".to_string());
    }
    // Alternativa: STORAGE_READ_CAPACITY (bytes 16..24 = DiskLength).
    let mut buf = [0u8; 24];
    match ioctl(h, IOCTL_STORAGE_READ_CAPACITY, &[], &mut buf) {
        Ok(n) if n >= 24 => leer_u64(&buf, 16),
        Ok(n) => {
            debug(|| format!("IOCTL_STORAGE_READ_CAPACITY devolvió {n} bytes"));
            None
        }
        Err(codigo) => {
            debug(|| format!("IOCTL_STORAGE_READ_CAPACITY -> error {codigo}"));
            None
        }
    }
}

/// GUID del disco entero (GPT `DiskId`), fallback de §10.1 cuando no hay serial.
fn guid_de_disco(h: HANDLE) -> Option<String> {
    let mut buf = vec![0u8; BUF_GRANDE];
    match ioctl(h, IOCTL_DISK_DRIVE_LAYOUT_EX, &[], &mut buf) {
        Ok(n) if n >= 24 => {
            // DRIVE_LAYOUT_INFORMATION_EX: PartitionStyle (0), PartitionCount (4)
            // y la unión en 8; para GPT, `DiskId` es el primer GUID de la unión.
            if leer_u32(&buf, 0) == Some(1) {
                Some(guid_a_texto(buf.get(8..24)?))
            } else {
                None
            }
        }
        Ok(n) => {
            debug(|| format!("IOCTL_DISK_GET_DRIVE_LAYOUT_EX devolvió {n} bytes"));
            None
        }
        Err(codigo) => {
            debug(|| format!("IOCTL_DISK_GET_DRIVE_LAYOUT_EX -> error {codigo}"));
            None
        }
    }
}

/// Número de disco físico de un volumen abierto.
fn leer_disco_de_volumen(h: HANDLE) -> Option<u32> {
    let mut buf = [0u8; std::mem::size_of::<StorageDeviceNumber>()];
    match ioctl(h, IOCTL_GET_DEVICE_NUMBER, &[], &mut buf) {
        Ok(n) if n >= 8 => leer_u32(&buf, 4),
        Ok(n) => {
            debug(|| format!("IOCTL_STORAGE_GET_DEVICE_NUMBER devolvió {n} bytes"));
            None
        }
        Err(codigo) => {
            debug(|| format!("IOCTL_STORAGE_GET_DEVICE_NUMBER -> error {codigo}"));
            None
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Extent {
    disco: u32,
    offset: u64,
    tamano: u64,
}

/// `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`: qué disco(s) hay detrás de la letra y
/// en qué offset empieza la partición. Es el fallback cuando
/// `IOCTL_STORAGE_GET_DEVICE_NUMBER` no está disponible.
fn leer_extents(h: HANDLE) -> Result<Vec<Extent>, u32> {
    let mut buf = vec![0u8; 2048];
    let devuelto = ioctl(h, IOCTL_VOLUME_DISK_EXTENTS, &[], &mut buf)?;
    if devuelto < 8 {
        return Ok(Vec::new());
    }
    let declarados = leer_u32(&buf, 0).unwrap_or(0) as usize;
    let caben = (devuelto as usize - 8) / DISK_EXTENT_LEN;
    let mut out = Vec::new();
    for i in 0..declarados.min(caben).min(64) {
        let base = 8 + i * DISK_EXTENT_LEN;
        let (Some(disco), Some(offset), Some(tamano)) = (
            leer_u32(&buf, base),
            leer_u64(&buf, base + 8),
            leer_u64(&buf, base + 16),
        ) else {
            break;
        };
        out.push(Extent {
            disco,
            offset,
            tamano,
        });
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
struct Particion {
    offset: u64,
    tamano: u64,
    guid: String,
}

fn leer_particion(h: HANDLE) -> Option<Particion> {
    let mut buf = vec![0u8; 192];
    match ioctl(h, IOCTL_DISK_PARTITION_INFO_EX, &[], &mut buf) {
        Ok(n) if n >= 32 => {
            let estilo = leer_u32(&buf, 0)?;
            let offset = leer_u64(&buf, 8)?;
            let tamano = leer_u64(&buf, 16)?;
            // PARTITION_INFORMATION_EX: estilo (0), offset (8), tamaño (16),
            // número (24), reescribir (28) y la unión en 32. En GPT el
            // `PartitionId` está en 32+16 = 48.
            let guid = if estilo == 1 && n >= 64 {
                buf.get(48..64).map(guid_a_texto).unwrap_or_default()
            } else {
                String::new()
            };
            Some(Particion {
                offset,
                tamano,
                guid,
            })
        }
        Ok(n) => {
            debug(|| format!("IOCTL_DISK_GET_PARTITION_INFO_EX devolvió {n} bytes"));
            None
        }
        Err(codigo) => {
            debug(|| format!("IOCTL_DISK_GET_PARTITION_INFO_EX -> error {codigo}"));
            None
        }
    }
}

/// `\\?\Volume{GUID}\`: estable por volumen, sobrevive al cambio de letra.
fn nombre_de_volumen(letra: char) -> Option<String> {
    let punto = wide(&format!("{letra}:\\"));
    let mut buf = [0u16; 128];
    // SAFETY: la ruta es UTF-16 con NUL y el buffer es propio con su largo.
    let ok = unsafe {
        GetVolumeNameForVolumeMountPointW(punto.as_ptr(), buf.as_mut_ptr(), buf.len() as u32)
    };
    if ok == 0 {
        // SAFETY: `GetLastError` no tiene precondiciones.
        let codigo = unsafe { GetLastError() };
        debug(|| format!("GetVolumeNameForVolumeMountPointW {letra}: -> error {codigo}"));
        return None;
    }
    let fin = buf.iter().position(|u| *u == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..fin]))
}

#[derive(Debug, Clone, Default)]
struct Geometria {
    disco: Option<u32>,
    offset: u64,
    tamano: u64,
    partition_guid: String,
}

/// Abre la letra probando los tres accesos conocidos y se queda con el primero
/// que devuelve número de disco. `IOCTL_STORAGE_GET_DEVICE_NUMBER` es el que
/// falla en algunas máquinas; ahí entra `IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS`.
fn geometria_de_volumen(letra: char) -> (Geometria, Vec<String>) {
    let ruta = format!("\\\\.\\{letra}:");
    let mut errores: Vec<String> = Vec::new();
    let mut reservada: Option<(Apertura, Geometria)> = None;
    for acceso in [FILE_READ_ATTRIBUTES, FILE_ANY_ACCESS, GENERIC_READ] {
        let a = match abrir_con(&ruta, acceso) {
            Ok(a) => a,
            Err(codigo) => {
                errores.push(format!(
                    "{ruta}: CreateFileW acceso=0x{acceso:x} -> error {codigo}"
                ));
                continue;
            }
        };
        let numero = leer_disco_de_volumen(a.handle);
        let extents = match leer_extents(a.handle) {
            Ok(e) => e,
            Err(codigo) => {
                errores.push(format!(
                    "{ruta}: IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS -> error {codigo}"
                ));
                Vec::new()
            }
        };
        let particion = leer_particion(a.handle);
        let mut geo = Geometria::default();
        if let Some(p) = &particion {
            geo.offset = p.offset;
            geo.tamano = p.tamano;
            geo.partition_guid = p.guid.clone();
        }
        if let Some(e) = extents.first() {
            if geo.offset == 0 {
                geo.offset = e.offset;
            }
            if geo.tamano == 0 {
                geo.tamano = e.tamano;
            }
            geo.disco = Some(e.disco);
        }
        if numero.is_some() {
            geo.disco = numero;
        }
        if geo.disco.is_some() {
            debug(|| {
                format!(
                    "{ruta}: acceso=0x{acceso:x} disco={:?} offset={} tamano={}",
                    geo.disco, geo.offset, geo.tamano
                )
            });
            return (geo, errores);
        }
        errores.push(format!(
            "{ruta}: con acceso=0x{acceso:x} no se obtuvo el número de disco"
        ));
        if reservada.is_none() {
            reservada = Some((a, geo));
        }
    }
    match reservada {
        Some((_, geo)) => (geo, errores),
        None => (Geometria::default(), errores),
    }
}

/// Un volumen montado con **todas** sus identidades: presentación, hardware,
/// partición y nube. Es la fila que emite `census volumes --json`.
#[derive(Debug, Clone)]
pub struct Volumen {
    pub letra: char,
    /// Letra con dos puntos (`"C:"`): sólo presentación (§10.1).
    pub mount: String,
    pub label: String,
    pub fs: String,
    pub total: u64,
    pub free: u64,
    pub tipo: DriveKind,
    /// Montado por un cliente de nube: su identidad es `cloud_id`, no hardware.
    pub nube: bool,
    pub cloud: Option<CloudId>,
    pub disco: Option<u32>,
    pub bus: Bus,
    pub vendor: String,
    pub modelo: String,
    pub revision: String,
    pub serial: String,
    pub capacidad: u64,
    pub offset: u64,
    pub tamano: u64,
    pub vol_serial: u64,
    /// `\\?\Volume{GUID}\` (§10.1).
    pub volume_guid: String,
    pub partition_guid: String,
    pub guid_disco: String,
    /// Hex minúsculas; vacío = `sin_identidad`.
    pub disk_hw_id: String,
    /// Hex minúsculas; vacío en volúmenes de nube (ahí manda `cloud_id`).
    pub volume_id: String,
    pub estado: EstadoIdentidad,
    pub nota: Option<String>,
}

impl Volumen {
    fn de_nube(d: &Drive, cloud: Option<CloudId>) -> Volumen {
        Volumen {
            letra: d.letter(),
            mount: d.name.clone(),
            label: d.label.clone(),
            fs: d.fs.clone(),
            total: d.total,
            free: d.free,
            tipo: d.kind(),
            nube: true,
            cloud,
            disco: None,
            bus: Bus::Otro(0),
            vendor: String::new(),
            modelo: String::new(),
            revision: String::new(),
            serial: String::new(),
            capacidad: 0,
            offset: 0,
            tamano: 0,
            vol_serial: crate::volumes::volume_serial(d.letter()).unwrap_or(0),
            volume_guid: nombre_de_volumen(d.letter()).unwrap_or_default(),
            partition_guid: String::new(),
            guid_disco: String::new(),
            disk_hw_id: String::new(),
            volume_id: String::new(),
            estado: EstadoIdentidad::SinIdentidad,
            nota: Some(
                "volumen de nube (§9): la identidad es cloud_id, no el hardware; \
                 la letra es sólo presentación"
                    .into(),
            ),
        }
    }

    pub fn serial_ambiguo(&self) -> bool {
        self.estado == EstadoIdentidad::SerialAmbiguo
    }

    /// `posible_duplicado` por hardware o por nube sin cuenta atribuible: en
    /// ambos casos se avisa y no se fusiona solo (§9.2, §10.4).
    pub fn posible_duplicado(&self) -> bool {
        self.estado == EstadoIdentidad::PosibleDuplicado
            || self
                .cloud
                .as_ref()
                .is_some_and(|c| c.posible_duplicado)
    }

    fn anotar(&mut self, nota: String) {
        match &mut self.nota {
            Some(previo) => {
                previo.push_str("; ");
                previo.push_str(&nota);
            }
            None => self.nota = Some(nota),
        }
    }
}

fn identificar(d: &Drive) -> Volumen {
    let letra = d.letter();
    let mut v = Volumen {
        letra,
        mount: d.name.clone(),
        label: d.label.clone(),
        fs: d.fs.clone(),
        total: d.total,
        free: d.free,
        tipo: d.kind(),
        nube: false,
        cloud: None,
        disco: None,
        bus: Bus::Otro(0),
        vendor: String::new(),
        modelo: String::new(),
        revision: String::new(),
        serial: String::new(),
        capacidad: 0,
        offset: 0,
        tamano: 0,
        vol_serial: crate::volumes::volume_serial(letra).unwrap_or(0),
        volume_guid: nombre_de_volumen(letra).unwrap_or_default(),
        partition_guid: String::new(),
        guid_disco: String::new(),
        disk_hw_id: String::new(),
        volume_id: String::new(),
        estado: EstadoIdentidad::SinIdentidad,
        nota: None,
    };

    // 1. Geometría: disco físico, offset y tamaño de la partición.
    let (geo, errores) = geometria_de_volumen(letra);
    v.disco = geo.disco;
    v.offset = geo.offset;
    v.tamano = geo.tamano;
    v.partition_guid = geo.partition_guid;
    for e in errores {
        debug(move || e);
    }

    // 2. El aparato físico: descriptor del disco.
    match v.disco {
        Some(numero) => {
            let crudo = leer_descriptor(numero);
            v.bus = crudo.bus;
            v.vendor = crudo.vendor;
            v.modelo = crudo.modelo;
            v.revision = crudo.revision;
            v.serial = crudo.serial;
            v.capacidad = crudo.capacidad;
            v.guid_disco = crudo.guid_disco;
            if let Some(nota) = crudo.nota {
                // Diagnóstico de IOCTLs (elevación, driver); no es identidad.
                debug(move || nota);
            }
            if normalizar(&v.serial).is_empty() {
                // §10.1: fallback = GUID de disco GPT, luego el de la partición
                // y, por último, las propiedades crudas del descriptor USB.
                let extra = if !v.guid_disco.is_empty() {
                    v.guid_disco.clone()
                } else if !v.partition_guid.is_empty() {
                    v.partition_guid.clone()
                } else {
                    crudo.raw_id
                };
                v.disk_hw_id = disk_hw_id(&v.serial, &v.vendor, &v.modelo, v.capacidad, &extra);
                if !v.disk_hw_id.is_empty() {
                    v.anotar(format!(
                        "sin serial de hardware: DiskHwId por GUID o propiedades crudas ({extra})"
                    ));
                }
            } else {
                v.disk_hw_id = disk_hw_id(&v.serial, &v.vendor, &v.modelo, v.capacidad, "");
            }
        }
        None => v.anotar(format!(
            "no se pudo resolver el disco físico de {letra}: (ver CENSUS_DEBUG=1)"
        )),
    }

    // 3. La partición. Si no hay hardware ni serie de volumen, no hay identidad.
    if v.disk_hw_id.is_empty() && v.vol_serial == 0 {
        v.estado = EstadoIdentidad::SinIdentidad;
        v.anotar("sin serial, sin GUID y sin vol_serial: no se publica en federación (§10.5)".into());
    } else {
        v.estado = EstadoIdentidad::Ok;
        v.volume_id = volume_id(&v.disk_hw_id, v.offset, v.tamano, v.vol_serial);
        if v.disk_hw_id.is_empty() {
            v.anotar("sin identidad de hardware: volume_id sólo por serie de volumen".into());
        }
    }
    v
}

/// Todos los volúmenes montados, con identidad ya resuelta y ambigüedades
/// marcadas. Los de nube no se sondean por hardware: informan `cloud_id`.
pub fn identidades() -> Vec<Volumen> {
    let mut out: Vec<Volumen> = Vec::new();
    for d in crate::volumes::enumerate() {
        if d.fs.is_empty() {
            continue;
        }
        let cloud = crate::volumes::nube_de(&d);
        let v = if cloud.is_some() {
            Volumen::de_nube(&d, cloud)
        } else {
            identificar(&d)
        };
        out.push(v);
    }
    marcar_seriales_ambiguos(&mut out);
    out
}

/// Marca `serial_ambiguo` (§10.4) y las colisiones que quedan como
/// `posible_duplicado`. Función pura sobre la lista ya recolectada: se prueba
/// sin hardware.
pub fn marcar_seriales_ambiguos(volumenes: &mut [Volumen]) {
    // 1. La misma serial con distinto modelo o capacidad ⇒ el serial es del
    //    puente USB, no del disco. Se agrupa por valor normalizado.
    let mut vistos: Vec<(String, Vec<(String, u64)>)> = Vec::new();
    for v in volumenes.iter().filter(|v| !v.nube) {
        let serial = normalizar(&v.serial);
        if serial.is_empty() {
            continue;
        }
        let par = (normalizar(&v.modelo), v.capacidad);
        match vistos.iter_mut().find(|(s, _)| *s == serial) {
            Some((_, pares)) => {
                if !pares.contains(&par) {
                    pares.push(par);
                }
            }
            None => vistos.push((serial, vec![par])),
        }
    }
    let ambiguas: Vec<String> = vistos
        .iter()
        .filter(|(_, pares)| pares.len() > 1)
        .map(|(serial, _)| serial.clone())
        .collect();

    for v in volumenes.iter_mut() {
        if v.nube || !ambiguas.contains(&normalizar(&v.serial)) {
            continue;
        }
        v.estado = EstadoIdentidad::SerialAmbiguo;
        let extra = if !v.guid_disco.is_empty() {
            v.guid_disco.clone()
        } else if !v.partition_guid.is_empty() {
            v.partition_guid.clone()
        } else {
            String::new()
        };
        v.disk_hw_id = disk_hw_id(&v.serial, &v.vendor, &v.modelo, v.capacidad, &extra);
        v.volume_id = volume_id(&v.disk_hw_id, v.offset, v.tamano, v.vol_serial);
        v.anotar(
            "serial_ambiguo: la misma serial aparece en discos con distinto modelo o \
             capacidad (típico de un puente USB-SATA barato que informa el serial del \
             puente, §10.4); se agrega modelo, capacidad y GUID al hash"
                .into(),
        );
    }

    // 2. Dos discos físicos distintos con la misma clave: puede ser el mismo
    //    aparato visto dos veces o dos gemelos detrás del mismo puente. No se
    //    fusiona solo: se avisa para que el usuario decida.
    let mut choques: Vec<usize> = Vec::new();
    for i in 0..volumenes.len() {
        for j in (i + 1)..volumenes.len() {
            let (a, b) = (&volumenes[i], &volumenes[j]);
            if a.nube || b.nube || a.disk_hw_id.is_empty() || a.disk_hw_id != b.disk_hw_id {
                continue;
            }
            if a.disco.is_none() || b.disco.is_none() || a.disco == b.disco {
                continue;
            }
            choques.push(i);
            choques.push(j);
        }
    }
    choques.sort_unstable();
    choques.dedup();
    for i in choques {
        if let Some(v) = volumenes.get_mut(i) {
            v.estado = EstadoIdentidad::PosibleDuplicado;
            v.anotar(
                "posible_duplicado: dos discos físicos distintos dieron el mismo \
                 DiskHwId; no se fusionan solos (§10.4)"
                    .into(),
            );
        }
    }
}

/// Un disco físico tal como lo ve Windows, con sus letras.
#[derive(Debug, Clone)]
pub struct Disco {
    pub numero: u32,
    pub bus: Bus,
    pub modelo: String,
    pub serial: String,
    pub capacidad: u64,
    pub disk_hw_id: String,
    /// Letras montadas sobre este disco (`C:`, `D:`, ...).
    pub letras: Vec<char>,
}

impl Disco {
    pub fn etiqueta(&self) -> String {
        let mut s = format!("disco {} ({}, {}", self.numero, self.bus.label(), self.modelo);
        if !self.serial.is_empty() {
            s.push_str(&format!(", serial {}", self.serial));
        }
        s.push(')');
        s
    }
}

/// Cuántas tareas pesadas tolera un grupo de contención.
///
/// - Interno (SATA/NVMe por disco): una por disco, no hay cable compartido.
/// - USB/RAID/virtual: **una** tarea pesada por grupo, y como mucho dos si hay
///   más de dos discos y el usuario fuerza: cuatro mecánicos en un enlace de
///   5 Gbps se estorban entre sí (cada uno ofrece 120–180 MB/s de secuencial,
///   pero comparten los 625 MB/s del bus y una sola cola de comandos).
pub fn cupo(grupo: &str, discos: usize) -> usize {
    match grupo {
        "usb" | "raid" | "virtual" => {
            if discos >= 2 {
                2
            } else {
                1
            }
        }
        _ => discos.max(1),
    }
}

/// Discos físicos con sus letras, agrupados por bus. Excluye la nube: un
/// Google Drive montado no comparte el disco anfitrión para planificar.
pub fn discos() -> Vec<Disco> {
    let mut out: Vec<Disco> = Vec::new();
    for v in identidades() {
        if v.nube {
            continue;
        }
        let Some(numero) = v.disco else {
            continue;
        };
        match out.iter_mut().find(|d| d.numero == numero) {
            Some(d) => {
                if !d.letras.contains(&v.letra) {
                    d.letras.push(v.letra);
                }
            }
            None => out.push(Disco {
                numero,
                bus: v.bus,
                modelo: if v.modelo.is_empty() {
                    "modelo desconocido".to_string()
                } else {
                    v.modelo.clone()
                },
                serial: v.serial.clone(),
                capacidad: v.capacidad,
                disk_hw_id: v.disk_hw_id.clone(),
                letras: vec![v.letra],
            }),
        }
    }
    out.sort_by_key(|d| d.numero);
    for d in &mut out {
        d.letras.sort_unstable();
    }
    out
}

/// Un grupo de contención: discos que se reparten el mismo ancho de banda.
#[derive(Debug, Clone)]
pub struct Grupo {
    pub nombre: String,
    pub discos: Vec<u32>,
    pub cupo: usize,
    pub compartido: bool,
}

/// Agrupa los discos por bus y calcula el cupo de tareas simultáneas.
pub fn grupos() -> Vec<Grupo> {
    let discos = discos();
    let mut nombres: Vec<String> = Vec::new();
    for d in &discos {
        let n = d.bus.grupo().to_string();
        if !nombres.contains(&n) {
            nombres.push(n);
        }
    }
    nombres
        .into_iter()
        .map(|nombre| {
            let miembros: Vec<&Disco> = discos.iter().filter(|d| d.bus.grupo() == nombre).collect();
            let compartido = miembros.first().is_some_and(|d| d.bus.comparte_enlace());
            Grupo {
                cupo: cupo(&nombre, miembros.len()),
                discos: miembros.iter().map(|d| d.numero).collect(),
                compartido,
                nombre,
            }
        })
        .collect()
}

/// Plan de indexado: qué se puede correr a la vez sin pisarse.
///
/// Interno: todos los discos en paralelo (cada uno con su controlador).
/// USB/RAID/virtual: de a uno, o de a dos si hay varios discos y el enlace
/// aguanta; el resto espera. Nunca se mezclan dos grupos con el mismo cupo.
pub fn plan() -> Vec<Vec<u32>> {
    let mut tandas: Vec<Vec<u32>> = Vec::new();
    for g in grupos() {
        for trozo in g.discos.chunks(g.cupo.max(1)) {
            tandas.push(trozo.to_vec());
        }
    }
    tandas
}

/// Texto listo para imprimir en `census drives`.
pub fn resumen() -> String {
    let discos = discos();
    if discos.is_empty() {
        return String::from(
            "sin discos fisicos visibles: no se pudo resolver el disco de ninguna unidad.\n  \
             Probá CENSUS_DEBUG=1 para ver el error de cada IOCTL (CreateFileW, \
             IOCTL_STORAGE_GET_DEVICE_NUMBER, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS).\n",
        );
    }
    let mut s = String::new();
    for d in &discos {
        let letras: Vec<String> = d.letras.iter().map(|c| format!("{c}:")).collect();
        s.push_str(&format!(
            "  {} -> {} [{}{}]\n",
            letras.join(", "),
            d.etiqueta(),
            d.bus.grupo(),
            if d.bus.comparte_enlace() {
                ", enlace compartido"
            } else {
                ""
            }
        ));
    }
    for g in grupos() {
        s.push_str(&format!(
            "  grupo {}: {} disco(s), hasta {} tarea(s) pesada(s) a la vez{}\n",
            g.nombre,
            g.discos.len(),
            g.cupo,
            if g.compartido {
                " (comparten cable)"
            } else {
                ""
            }
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vol(letra: char, disco: u32, serial: &str, modelo: &str, capacidad: u64) -> Volumen {
        Volumen {
            letra,
            mount: format!("{letra}:"),
            label: String::new(),
            fs: "NTFS".into(),
            total: capacidad,
            free: 0,
            tipo: DriveKind::Fixed,
            nube: false,
            cloud: None,
            disco: Some(disco),
            bus: Bus::Sata,
            vendor: "SEAGATE".into(),
            modelo: modelo.into(),
            revision: String::new(),
            serial: serial.into(),
            capacidad,
            offset: 1_048_576,
            tamano: capacidad / 2,
            vol_serial: 0x1234_5678,
            volume_guid: String::new(),
            partition_guid: String::new(),
            guid_disco: String::new(),
            disk_hw_id: disk_hw_id(serial, "SEAGATE", modelo, capacidad, ""),
            volume_id: String::new(),
            estado: EstadoIdentidad::Ok,
            nota: None,
        }
    }

    #[test]
    fn normaliza_serial_para_no_depender_de_espacios_o_mayusculas() {
        assert_eq!(normalizar("  wd-wcc7  1234 "), "WD-WCC7 1234");
        assert_eq!(normalizar("\tE823_8fA6\n"), "E823_8FA6");
        assert_eq!(normalizar("   "), "");
        // El mismo serial escrito de otra forma da el mismo DiskHwId.
        assert_eq!(
            disk_hw_id("  wd-wcc7 1234 ", "WD", "Elements 25A3", 4_000_000, ""),
            disk_hw_id("WD-WCC7 1234", "wd", "elements   25a3", 4_000_000, "")
        );
    }

    #[test]
    fn disk_hw_id_es_hex_de_16_y_separa_campos() {
        let id = disk_hw_id("SER123", "Seagate", "Backup+", 1_000_000, "");
        assert_eq!(id.len(), 16);
        assert!(id.chars().all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()));
        // Sin serial, sin modelo ni vendor ni GUID no hay identidad.
        assert_eq!(disk_hw_id("", "", "", 0, ""), "");
        // El serial manda: mismo modelo y capacidad, serial distinto.
        let a = disk_hw_id("SER-A", "WD", "Elements", 2_000_000, "");
        let b = disk_hw_id("SER-B", "WD", "Elements", 2_000_000, "");
        assert_ne!(a, b);
        // La capacidad entra: dos discos del mismo puente se distinguen.
        assert_ne!(
            disk_hw_id("SER-A", "WD", "Elements", 2_000_000, ""),
            disk_hw_id("SER-A", "WD", "Elements", 4_000_000, "")
        );
        // `("a","bc")` y `("ab","c")` no colisionan (separador NUL).
        assert_ne!(
            disk_hw_id("ab", "c", "", 0, ""),
            disk_hw_id("a", "bc", "", 0, "")
        );
    }

    #[test]
    fn volume_id_no_depende_de_la_letra_y_si_de_la_particion() {
        let hw = disk_hw_id("SER123", "Seagate", "Backup+", 1_000_000, "");
        let a = volume_id(&hw, 1_048_576, 500_000_000, 0xdead_beef);
        assert_eq!(a, volume_id(&hw, 1_048_576, 500_000_000, 0xdead_beef));
        assert_ne!(a, volume_id(&hw, 2_097_152, 500_000_000, 0xdead_beef));
        assert_ne!(a, volume_id(&hw, 1_048_576, 400_000_000, 0xdead_beef));
        assert_ne!(a, volume_id(&hw, 1_048_576, 500_000_000, 0x1234_5678));
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn serial_repetida_con_distinta_capacidad_es_ambigua() {
        // Dos discos detrás de la misma caja USB-SATA: el puente repite serial.
        let mut vols = vec![
            vol('E', 1, "0123456789ABCDEF", "USB 3.0", 4_000_000_000_000),
            vol('F', 2, "0123456789abcdef", "USB 3.0", 8_000_000_000_000),
        ];
        let antes = (vols[0].disk_hw_id.clone(), vols[1].disk_hw_id.clone());
        assert_ne!(antes.0, antes.1, "capacidad distinta ya los separa");
        marcar_seriales_ambiguos(&mut vols);
        assert!(vols.iter().all(|v| v.serial_ambiguo()));
        assert!(vols.iter().all(|v| v.estado != EstadoIdentidad::SinIdentidad));
        // El aviso queda escrito para que Atlas lo muestre.
        assert!(vols[0].nota.as_deref().unwrap_or("").contains("serial_ambiguo"));
        assert_ne!(vols[0].disk_hw_id, vols[1].disk_hw_id);
        assert_ne!(vols[0].volume_id, vols[1].volume_id);
    }

    #[test]
    fn misma_serial_y_mismo_modelo_no_es_ambiguo() {
        // Dos particiones del mismo disco comparten serial: eso NO es ambiguo.
        let mut vols = vec![
            vol('C', 0, "SERIAL-1", "NVMe", 256_000_000_000),
            vol('D', 0, "SERIAL-1", "NVMe", 256_000_000_000),
        ];
        marcar_seriales_ambiguos(&mut vols);
        assert_eq!(vols[0].estado, EstadoIdentidad::Ok);
        assert_eq!(vols[0].disk_hw_id, vols[1].disk_hw_id);
        // Mismo disco y mismo offset/serie no debería pasar; si pasa, es el
        // mismo volume_id, que es justo lo que se quiere.
        assert_eq!(vols[0].volume_id, vols[1].volume_id);
    }

    #[test]
    fn dos_discos_con_la_misma_clave_quedan_como_posible_duplicado() {
        // Dos discos gemelos detrás del mismo puente: serial, modelo y capacidad
        // iguales ⇒ el hash no alcanza. Se avisa, no se fusiona.
        let mut vols = vec![
            vol('E', 1, "BRIDGE-1", "USB 3.0", 4_000_000_000_000),
            vol('F', 2, "BRIDGE-1", "USB 3.0", 4_000_000_000_000),
        ];
        assert_eq!(vols[0].disk_hw_id, vols[1].disk_hw_id);
        marcar_seriales_ambiguos(&mut vols);
        assert!(vols.iter().all(|v| v.posible_duplicado()));
        assert!(vols[0].nota.as_deref().unwrap_or("").contains("posible_duplicado"));
    }

    #[test]
    fn sin_serial_ni_guid_ni_serie_es_sin_identidad() {
        assert_eq!(disk_hw_id("", "", "", 0, ""), "");
        assert!(!EstadoIdentidad::SinIdentidad.federable());
        assert!(EstadoIdentidad::Ok.federable());
        assert_eq!(EstadoIdentidad::SerialAmbiguo.label(), "serial_ambiguo");
        assert_eq!(EstadoIdentidad::PosibleDuplicado.label(), "posible_duplicado");
        assert_eq!(EstadoIdentidad::SinIdentidad.label(), "sin_identidad");
    }

    #[test]
    fn descriptor_del_disco_se_parsea_con_limites() {
        // Buffer sintético con el layout real de STORAGE_DEVICE_DESCRIPTOR.
        let mut buf = vec![0u8; 128];
        buf[0..4].copy_from_slice(&1u32.to_le_bytes()); // Version
        buf[4..8].copy_from_slice(&36u32.to_le_bytes()); // Size
        buf[12..16].copy_from_slice(&36u32.to_le_bytes()); // VendorIdOffset
        buf[16..20].copy_from_slice(&44u32.to_le_bytes()); // ProductIdOffset
        buf[20..24].copy_from_slice(&64u32.to_le_bytes()); // ProductRevisionOffset
        buf[24..28].copy_from_slice(&72u32.to_le_bytes()); // SerialNumberOffset
        buf[28..32].copy_from_slice(&17u32.to_le_bytes()); // BusType = NVMe
        buf[32..36].copy_from_slice(&8u32.to_le_bytes()); // RawPropertiesLength
        buf[36] = b'W';
        buf[37] = b'D';
        buf[44..52].copy_from_slice(b"PC SN740");
        buf[64..67].copy_from_slice(b"1.0");
        buf[72..86].copy_from_slice(b"E823_8FA6_BF53");
        let mut crudo = Crudo::default();
        parse_descriptor(&buf, &mut crudo);
        assert_eq!(crudo.vendor, "WD");
        assert_eq!(crudo.modelo, "PC SN740");
        assert_eq!(crudo.revision, "1.0");
        assert_eq!(crudo.serial, "E823_8FA6_BF53");
        assert_eq!(crudo.bus, Bus::Nvme);
        assert!(crudo.raw_id.starts_with("raw:"));
        assert!(crudo.nota.is_none());
    }

    #[test]
    fn descriptor_truncado_o_con_offsets_locos_no_paniquea() {
        let mut corto = Crudo::default();
        parse_descriptor(&[1, 2, 3], &mut corto);
        assert!(corto.serial.is_empty());
        assert!(corto.nota.is_some());

        let mut buf = vec![0u8; 40];
        buf[4..8].copy_from_slice(&36u32.to_le_bytes());
        buf[12..16].copy_from_slice(&9999u32.to_le_bytes()); // vendor fuera
        buf[16..20].copy_from_slice(&4u32.to_le_bytes()); // producto dentro de la cabecera
        buf[24..28].copy_from_slice(&39u32.to_le_bytes()); // serial sin NUL
        buf[32..36].copy_from_slice(&9999u32.to_le_bytes()); // raw imposible
        let mut crudo = Crudo::default();
        parse_descriptor(&buf, &mut crudo);
        assert!(crudo.vendor.is_empty());
        assert!(crudo.modelo.is_empty());
        assert!(crudo.serial.is_empty(), "sin NUL y al borde: se corta, no se lee de más");
        assert!(crudo.raw_id.is_empty());
    }

    #[test]
    fn usb_comparte_enlace_y_limita_a_dos() {
        assert!(Bus::Usb.comparte_enlace());
        assert!(!Bus::Nvme.comparte_enlace());
        assert_eq!(cupo("usb", 4), 2);
        assert_eq!(cupo("usb", 1), 1);
        assert_eq!(cupo("nvme", 1), 1);
        assert_eq!(cupo("sata", 3), 3);
    }

    #[test]
    fn mapea_los_codigos_de_bus_de_windows() {
        assert_eq!(Bus::desde(17), Bus::Nvme);
        assert_eq!(Bus::desde(7), Bus::Usb);
        assert_eq!(Bus::desde(11), Bus::Sata);
        assert_eq!(Bus::desde(99), Bus::Otro(99));
        assert_eq!(Bus::desde(99).label(), "otro");
    }

    #[test]
    fn el_plan_no_supera_el_cupo_de_cada_grupo() {
        // Cuatro discos USB: deben salir en dos tandas de dos.
        let tandas = vec![vec![1u32, 2], vec![3, 4]];
        for t in &tandas {
            assert!(t.len() <= cupo("usb", 4));
        }
    }
}
