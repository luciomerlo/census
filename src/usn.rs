//! USN Journal: el aviso de cambios de NTFS.
//!
//! El índice es una foto; esto es lo que la mantiene al día sin releer la MFT.
//! NTFS escribe en `$UsnJrnl` un registro por cada creación, borrado, renombre
//! o modificación, con el FRN del archivo, el FRN del padre, el nombre y el
//! motivo. Se lee como un archivo: desde un USN hasta el actual.
//!
// `as_chunks` (lo que sugiere el lint) todavía es inestable en Rust estable.
#![allow(clippy::chunks_exact_to_as_chunks)]
//!
//! Todo el FFI es puntual; el parseo de los registros está aislado en
//! [`parse_records`] para poder probarlo con bytes sintéticos, sin tocar el
//! disco ni pedir elevación.

use std::io;

use windows_sys::Win32::System::IO::DeviceIoControl;

use crate::win::VolumeHandle;

/// `FSCTL_QUERY_USN_JOURNAL`: estado actual del diario.
pub const FSCTL_QUERY_USN_JOURNAL: u32 = 0x0009_00F4;
/// `FSCTL_READ_USN_JOURNAL`: leer registros desde un USN.
pub const FSCTL_READ_USN_JOURNAL: u32 = 0x0009_00BB;

/// El FRN de NTFS es `secuencia (16 bits) << 48 | número de registro (48)`.
/// El índice guarda sólo el número de registro, así que se enmascara acá: es
/// el único punto donde se traducen las dos representaciones.
pub const FRN_MASK: u64 = 0x0000_FFFF_FFFF_FFFF;

/// `USN_REASON_FILE_CREATE`
pub const REASON_CREATE: u32 = 0x0000_0100;
/// `USN_REASON_FILE_DELETE`
pub const REASON_DELETE: u32 = 0x0000_0200;
/// `USN_REASON_RENAME_OLD_NAME`
pub const REASON_RENAME_OLD: u32 = 0x0000_1000;
/// `USN_REASON_RENAME_NEW_NAME`
pub const REASON_RENAME_NEW: u32 = 0x0000_2000;
/// `USN_REASON_BASIC_INFO_CHANGE` (atributos y fechas)
pub const REASON_BASIC_INFO: u32 = 0x0000_8000;
/// `USN_REASON_CLOSE`
pub const REASON_CLOSE: u32 = 0x8000_0000;

/// `USN_JOURNAL_DATA_V0`, 56 bytes.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct UsnJournalData {
    usn_journal_id: u64,
    first_usn: i64,
    next_usn: i64,
    lowest_valid_usn: i64,
    max_usn: i64,
    maximum_size: u64,
    allocation_delta: u64,
}

/// `READ_USN_JOURNAL_DATA_V0`, 48 bytes contando el padding final.
#[repr(C)]
#[derive(Default, Clone, Copy)]
struct ReadUsnJournalData {
    start_usn: i64,
    reason_mask: u32,
    return_only_on_close: u32,
    timeout: u64,
    bytes_to_wait_for: u64,
    usn_journal_id: u64,
    min_major_version: u16,
    max_major_version: u16,
    _padding: u32,
}

/// Estado del diario en un momento dado.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Journal {
    /// Identificador del diario. Si cambia, los cambios viejos se perdieron.
    pub id: u64,
    /// USN más viejo que todavía se puede leer.
    pub first_usn: i64,
    /// USN del próximo registro que se va a escribir.
    pub next_usn: i64,
}

/// Un cambio leído del diario, ya decodificado.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cambio {
    pub frn: u64,
    pub padre: u64,
    pub nombre: Vec<u16>,
    pub motivo: u32,
    pub attrs: u32,
}

impl Cambio {
    pub fn es_creacion(&self) -> bool {
        self.motivo & REASON_CREATE != 0
    }
    pub fn es_borrado(&self) -> bool {
        self.motivo & REASON_DELETE != 0
    }
    pub fn es_renombre(&self) -> bool {
        self.motivo & (REASON_RENAME_OLD | REASON_RENAME_NEW) != 0
    }
    pub fn es_carpeta(&self) -> bool {
        const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;
        self.attrs & FILE_ATTRIBUTE_DIRECTORY != 0
    }
}

/// Estado del diario del volumen. Requiere elevación.
pub fn query(h: &VolumeHandle) -> io::Result<Journal> {
    let mut out = UsnJournalData::default();
    let mut returned: u32 = 0;
    let size = std::mem::size_of::<UsnJournalData>() as u32;
    let ok = unsafe {
        DeviceIoControl(
            h.raw(),
            FSCTL_QUERY_USN_JOURNAL,
            std::ptr::null(),
            0,
            &mut out as *mut UsnJournalData as *mut std::ffi::c_void,
            size,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    if returned < size {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "respuesta corta del journal",
        ));
    }
    Ok(Journal {
        id: out.usn_journal_id,
        first_usn: out.first_usn,
        next_usn: out.next_usn,
    })
}

/// Lee hasta `max_bytes` de registros a partir de `desde`.
///
/// Devuelve los cambios y el USN desde el que seguir la próxima vez. Si no
/// hubo nada nuevo, la lista viene vacía y el USN es el mismo.
pub fn read(
    h: &VolumeHandle,
    desde: i64,
    journal_id: u64,
    max_bytes: usize,
) -> io::Result<(Vec<Cambio>, i64)> {
    let entrada = ReadUsnJournalData {
        start_usn: desde,
        reason_mask: u32::MAX,
        return_only_on_close: 0,
        timeout: 0,
        bytes_to_wait_for: 0,
        usn_journal_id: journal_id,
        min_major_version: 2,
        max_major_version: 2,
        _padding: 0,
    };
    let mut buffer = vec![0u8; max_bytes.max(64 * 1024)];
    let mut returned: u32 = 0;
    let ok = unsafe {
        DeviceIoControl(
            h.raw(),
            FSCTL_READ_USN_JOURNAL,
            &entrada as *const ReadUsnJournalData as *const std::ffi::c_void,
            std::mem::size_of::<ReadUsnJournalData>() as u32,
            buffer.as_mut_ptr() as *mut std::ffi::c_void,
            buffer.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    let returned = returned as usize;
    // Los primeros 8 bytes de la respuesta son el USN del próximo registro.
    if returned < 8 {
        return Ok((Vec::new(), desde));
    }
    let siguiente = i64::from_le_bytes(buffer[0..8].try_into().unwrap());
    let cambios = parse_records(&buffer[8..returned]);
    Ok((cambios, siguiente))
}

/// Parsea registros `USN_RECORD_V2` consecutivos. Ignora lo que no entienda.
///
/// `USN_RECORD_V2`, 60 bytes de cabecera:
/// `RecordLength` u32, `MajorVersion` u16, `MinorVersion` u16, `FileReferenceNumber`
/// u64, `ParentFileReferenceNumber` u64, `Usn` i64, `TimeStamp` i64, `Reason` u32,
/// `SourceInfo` u32, `SecurityId` u32, `FileAttributes` u32, `FileNameLength` u16,
/// `FileNameOffset` u16, y el nombre UTF-16.
pub fn parse_records(bytes: &[u8]) -> Vec<Cambio> {
    let mut out = Vec::new();
    let mut off = 0usize;
    while off + 60 <= bytes.len() {
        let largo = u32::from_le_bytes(bytes[off..off + 4].try_into().unwrap()) as usize;
        if largo < 60 || off + largo > bytes.len() {
            break;
        }
        let version = u16::from_le_bytes(bytes[off + 4..off + 6].try_into().unwrap());
        let salto = (largo + 7) & !7;
        if version != 2 {
            off += salto;
            continue;
        }
        let frn = u64::from_le_bytes(bytes[off + 8..off + 16].try_into().unwrap()) & FRN_MASK;
        let padre = u64::from_le_bytes(bytes[off + 16..off + 24].try_into().unwrap()) & FRN_MASK;
        let motivo = u32::from_le_bytes(bytes[off + 40..off + 44].try_into().unwrap());
        let attrs = u32::from_le_bytes(bytes[off + 52..off + 56].try_into().unwrap());
        let nombre_len = u16::from_le_bytes(bytes[off + 56..off + 58].try_into().unwrap()) as usize;
        let nombre_off = u16::from_le_bytes(bytes[off + 58..off + 60].try_into().unwrap()) as usize;
        if nombre_len.is_multiple_of(2) && off + nombre_off + nombre_len <= bytes.len() {
            let nombre: Vec<u16> = bytes[off + nombre_off..off + nombre_off + nombre_len]
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            out.push(Cambio {
                frn,
                padre,
                nombre,
                motivo,
                attrs,
            });
        }
        off += salto;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Arma un `USN_RECORD_V2` con nombre y largo reales, alineado a 8.
    fn registro(frn: u64, padre: u64, motivo: u32, attrs: u32, nombre: &[u16]) -> Vec<u8> {
        let nombre_len = nombre.len() * 2;
        let total = (60 + nombre_len + 7) & !7;
        let mut b = vec![0u8; total];
        b[0..4].copy_from_slice(&(total as u32).to_le_bytes());
        b[4..6].copy_from_slice(&2u16.to_le_bytes());
        b[8..16].copy_from_slice(&frn.to_le_bytes());
        b[16..24].copy_from_slice(&padre.to_le_bytes());
        b[24..32].copy_from_slice(&frn.to_le_bytes()); // USN, no se usa
        b[40..44].copy_from_slice(&motivo.to_le_bytes());
        b[52..56].copy_from_slice(&attrs.to_le_bytes());
        b[56..58].copy_from_slice(&(nombre_len as u16).to_le_bytes());
        b[58..60].copy_from_slice(&60u16.to_le_bytes());
        for (i, u) in nombre.iter().enumerate() {
            b[60 + i * 2..62 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        b
    }

    fn utf16(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn parsea_varios_registros_y_sus_motivos() {
        let mut bytes = Vec::new();
        bytes.extend(registro(100, 5, REASON_CREATE, 0x20, &utf16("informe.txt")));
        bytes.extend(registro(101, 5, REASON_DELETE, 0x20, &utf16("viejo.txt")));
        bytes.extend(registro(
            102,
            6,
            REASON_RENAME_NEW,
            0x20,
            &utf16("nuevo.txt"),
        ));
        let cambios = parse_records(&bytes);
        assert_eq!(cambios.len(), 3);
        assert_eq!(cambios[0].frn, 100);
        assert_eq!(cambios[0].padre, 5);
        assert!(cambios[0].es_creacion());
        assert!(!cambios[0].es_borrado());
        assert!(!cambios[0].es_renombre());
        assert!(cambios[1].es_borrado());
        assert!(cambios[2].es_renombre());
        assert_eq!(cambios[0].nombre, utf16("informe.txt"));
    }

    #[test]
    fn ignora_cola_truncada_y_largo_absurdo() {
        let mut bytes = registro(1, 0, REASON_CREATE, 0x20, &utf16("a.txt"));
        bytes.extend_from_slice(&[0u8; 7]);
        assert_eq!(parse_records(&bytes).len(), 1);

        let mut roto = registro(2, 0, REASON_CREATE, 0x20, &utf16("b.txt"));
        roto[0..4].copy_from_slice(&9999u32.to_le_bytes());
        assert!(parse_records(&roto).is_empty());
    }

    #[test]
    fn version_distinta_se_salta_pero_no_corta() {
        let mut bytes = registro(1, 0, REASON_CREATE, 0x20, &utf16("a"));
        bytes[4..6].copy_from_slice(&3u16.to_le_bytes());
        bytes.extend(registro(2, 0, REASON_CREATE, 0x20, &utf16("b")));
        let cambios = parse_records(&bytes);
        assert_eq!(cambios.len(), 1);
        assert_eq!(cambios[0].frn, 2);
    }

    #[test]
    fn enmascara_la_secuencia_del_frn() {
        // FRN real: secuencia 21 en los 16 bits altos, registro 0x14F6 abajo.
        let frn_con_secuencia = (21u64 << 48) | 0x14F6;
        let b = registro(
            frn_con_secuencia,
            (21u64 << 48) | 5,
            REASON_CREATE,
            0x20,
            &utf16("x"),
        );
        let c = parse_records(&b);
        assert_eq!(c[0].frn, 0x14F6);
        assert_eq!(c[0].padre, 5);
    }

    #[test]
    fn reconoce_carpetas() {
        let b = registro(7, 1, REASON_CREATE, 0x10, &utf16("carpeta"));
        let c = parse_records(&b);
        assert!(c[0].es_carpeta());
    }

    #[test]
    fn vario_motivo_en_un_solo_registro() {
        let b = registro(9, 1, REASON_CREATE | REASON_CLOSE, 0x20, &utf16("x"));
        let c = parse_records(&b);
        assert!(c[0].es_creacion());
    }
}
