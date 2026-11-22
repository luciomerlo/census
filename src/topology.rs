//! Topología de discos: qué hay del otro lado del cable.
//!
//! Un volumen (`C:`) no dice nada sobre el hardware: dos letras pueden ser dos
//! particiones del mismo disco, y cuatro discos pueden colgar del **mismo**
//! controlador USB compartiendo ancho de banda. Para decidir cuántas tareas
//! pesadas correr a la vez hace falta mirar el disco físico y su bus:
//!
//! 1. `IOCTL_STORAGE_GET_DEVICE_NUMBER` da el número de disco físico de una letra.
//! 2. `IOCTL_STORAGE_QUERY_PROPERTY` con `StorageDeviceProperty` da el tipo de bus
//!    (SATA, NVMe, USB, ...) y el modelo, sin depender de WMI ni de PowerShell.
//!
//! Con eso se arma un **grupo de contención**: discos que comparten bus comparten
//! techo de velocidad. La regla de planificación es una sola tarea pesada por
//! grupo; en el grupo interno (SATA/NVMe, sin compartir cable) se permite una por
//! disco. En USB se permite como mucho dos, porque cuatro mecánicos saturan los
//! 5 Gbps del enlace y además se pelean por la cabeza de lectura.

use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::win::wide;

/// `FILE_READ_ATTRIBUTES`: alcanza para preguntar el número de disco y no pide
/// elevación (a diferencia de `GENERIC_READ` sobre un volumen).
const FILE_READ_ATTRIBUTES: u32 = 0x0000_0080;

/// `IOCTL_STORAGE_GET_DEVICE_NUMBER`
const IOCTL_GET_DEVICE_NUMBER: u32 = 0x0002_D108;
/// `IOCTL_STORAGE_QUERY_PROPERTY`
const IOCTL_QUERY_PROPERTY: u32 = 0x0002_D140;
/// `StorageDeviceProperty`
const STORAGE_DEVICE_PROPERTY: u32 = 0;

/// `STORAGE_DEVICE_NUMBER`
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct StorageDeviceNumber {
    device_type: u32,
    device_number: u32,
    partition_number: u32,
}

/// `STORAGE_PROPERTY_QUERY`
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct StoragePropertyQuery {
    property_id: u32,
    query_type: u32,
    additional: [u8; 1],
}

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

/// Un disco físico tal como lo ve Windows.
#[derive(Debug, Clone)]
pub struct Disco {
    pub numero: u32,
    pub bus: Bus,
    pub modelo: String,
    /// Letras montadas sobre este disco (`C:`, `D:`, ...).
    pub letras: Vec<char>,
}

impl Disco {
    pub fn etiqueta(&self) -> String {
        format!(
            "disco {} ({}, {})",
            self.numero,
            self.bus.label(),
            self.modelo
        )
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

/// Número de disco físico de una letra. Requiere poder abrir `\\.\X:`.
pub fn disco_de(letra: char) -> Option<u32> {
    let path = wide(&format!("\\\\.\\{}:", letra.to_ascii_uppercase()));
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return None;
    }
    let mut out = StorageDeviceNumber::default();
    let mut devuelto: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_GET_DEVICE_NUMBER,
            std::ptr::null(),
            0,
            &mut out as *mut StorageDeviceNumber as *mut std::ffi::c_void,
            std::mem::size_of::<StorageDeviceNumber>() as u32,
            &mut devuelto,
            std::ptr::null_mut(),
        )
    };
    unsafe { windows_sys::Win32::Foundation::CloseHandle(h) };
    (ok != 0).then_some(out.device_number)
}

/// Bus y modelo del disco físico. Sin permiso devuelve `(Otro(0), "")`.
pub fn datos_de_disco(numero: u32) -> (Bus, String) {
    let path = wide(&format!("\\\\.\\PhysicalDrive{numero}"));
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            std::ptr::null(),
            OPEN_EXISTING,
            0,
            std::ptr::null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return (Bus::Otro(0), String::new());
    }
    let consulta = StoragePropertyQuery {
        property_id: STORAGE_DEVICE_PROPERTY,
        query_type: 0, // PropertyStandardQuery
        additional: [0],
    };
    let mut buf = vec![0u8; 1024];
    let mut devuelto: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            h,
            IOCTL_QUERY_PROPERTY,
            &consulta as *const StoragePropertyQuery as *const std::ffi::c_void,
            std::mem::size_of::<StoragePropertyQuery>() as u32,
            buf.as_mut_ptr() as *mut std::ffi::c_void,
            buf.len() as u32,
            &mut devuelto,
            std::ptr::null_mut(),
        )
    };
    unsafe { windows_sys::Win32::Foundation::CloseHandle(h) };
    if ok == 0 || devuelto < 28 {
        return (Bus::Otro(0), String::new());
    }
    // STORAGE_DEVICE_DESCRIPTOR: el BusType vive en el offset 28 (u32) y el
    // modelo es el ProductIdOffset (offset 16) dentro del mismo buffer.
    let bus = u32::from_le_bytes(buf[28..32].try_into().unwrap());
    let producto = u32::from_le_bytes(buf[16..20].try_into().unwrap()) as usize;
    let modelo = if producto > 0 && producto < devuelto as usize {
        let resto = &buf[producto..devuelto as usize];
        let fin = resto.iter().position(|b| *b == 0).unwrap_or(resto.len());
        String::from_utf8_lossy(&resto[..fin]).trim().to_string()
    } else {
        String::new()
    };
    (Bus::desde(bus), modelo)
}

/// Discos físicos con sus letras, agrupados por bus.
pub fn discos() -> Vec<Disco> {
    let mut out: Vec<Disco> = Vec::new();
    for d in crate::volumes::enumerate() {
        let Some(numero) = disco_de(d.letter()) else {
            continue;
        };
        match out.iter_mut().find(|x| x.numero == numero) {
            Some(x) => x.letras.push(d.letter()),
            None => {
                let (bus, modelo) = datos_de_disco(numero);
                out.push(Disco {
                    numero,
                    bus,
                    modelo,
                    letras: vec![d.letter()],
                });
            }
        }
    }
    out.sort_by_key(|d| d.numero);
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
        return String::from("sin discos fisicos visibles (hace falta elevacion)");
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
