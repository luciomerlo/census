//! Contrato de bytes del IPC de consulta.
//!
//! El layout es el que ya hablan SortEverything y WizEverything: pedido QUERY2W
//! por `WM_COPYDATA` y respuesta con cabecera de 20 bytes, tabla de items de 8
//! bytes por fila y datos (cadenas `u32 len` + UTF-16 + NUL) al final.
//!
//! Es el único lugar del proyecto donde se fijan offsets: si algo se rompe en
//! serie, se rompe acá y lo detectan los tests de `tests/protocol.rs`.

/// `dwData` del `WM_COPYDATA` de consulta.
pub const QUERY2W: usize = 18;
/// Mensaje `WM_USER + n` que responde si el índice está cargado.
pub const IS_DB_LOADED: u32 = 401;
/// Cabecera de la respuesta: `total`, `num`, `offset`, `flags`, reservado.
/// Las filas empiezan justo despues, en el byte 20.
pub const HEADER_LEN: usize = 20;
/// Tamaño de cada fila: `flags` y `offset` al prefijo de largo del nombre
/// (el primer caracter empieza 4 bytes despues de ese offset).
pub const ITEM_LEN: usize = 8;
/// Tamaño que representa "no hay tamaño" (carpetas, unidades).
pub const UNKNOWN_SIZE: u64 = u64::MAX;

/// Bits del campo `request_flags`.
pub mod req {
    pub const NAME: u32 = 0x0000_0001;
    pub const PATH: u32 = 0x0000_0002;
    pub const FULL_PATH: u32 = 0x0000_0004;
    pub const SIZE: u32 = 0x0000_0010;
    pub const DATE_MODIFIED: u32 = 0x0000_0040;
    pub const DATE_ACCESSED: u32 = 0x0000_0080;
    pub const ATTRIBUTES: u32 = 0x0000_0100;
    pub const RUN_COUNT: u32 = 0x0000_0400;
    /// Lo que piden las apps: nombre, carpeta, tamaño y fecha de modificación.
    pub const BASIC: u32 = NAME | PATH | SIZE | DATE_MODIFIED;
}

/// Bits del campo `flags` de cada item.
pub mod item {
    pub const FOLDER: u32 = 0x0001;
    pub const DRIVE: u32 = 0x0002;
    pub const IS_DIR: u32 = FOLDER | DRIVE;
}

/// Orden de resultados.
pub mod sort {
    pub const NAME_ASC: u32 = 1;
    pub const NAME_DESC: u32 = 2;
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Request {
    /// HWND al que se le manda la respuesta (recortado a 32 bits en el protocolo).
    pub reply_hwnd: u32,
    /// `dwData` que debe llevar el `WM_COPYDATA` de respuesta.
    pub reply_msg: u32,
    pub offset: u32,
    pub max: u32,
    pub flags: u32,
    pub sort: u32,
    pub search: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReplyItem {
    pub name: String,
    /// Carpeta contenedora, sin el nombre. Vacía para el nodo raíz.
    pub path: String,
    /// `None` = desconocido (carpetas, unidades): se serializa como `u64::MAX`.
    pub size: Option<u64>,
    pub modified_ft: u64,
    pub accessed_ft: u64,
    pub attributes: u32,
    pub run_count: u32,
    pub is_dir: bool,
    pub is_drive: bool,
}

impl ReplyItem {
    pub fn flags(&self) -> u32 {
        if self.is_drive {
            item::DRIVE
        } else if self.is_dir {
            item::FOLDER
        } else {
            0
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reply {
    pub total: u32,
    pub offset: u32,
    pub flags: u32,
    pub items: Vec<ReplyItem>,
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    b.get(at..at + 4).map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    b.get(at..at + 8).map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

/// Pedido: 7 `u32` y después la búsqueda en UTF-16 terminada en NUL.
///
/// El tercer campo es el largo en unidades de la cadena; los clientes que
/// conocemos lo mandan en 0 y confían en el terminador.
pub fn parse_request(buf: &[u8]) -> Option<Request> {
    if buf.len() < 28 {
        return None;
    }
    let search_len = u32_at(buf, 8)? as usize;
    let mut units: Vec<u16> = Vec::new();
    if search_len > 0 && 28 + search_len * 2 <= buf.len() {
        units.reserve(search_len);
        for k in 0..search_len {
            let i = 28 + 2 * k;
            units.push(u16::from_le_bytes([buf[i], buf[i + 1]]));
        }
    } else {
        let mut i = 28;
        while i + 1 < buf.len() {
            let u = u16::from_le_bytes([buf[i], buf[i + 1]]);
            if u == 0 {
                break;
            }
            units.push(u);
            i += 2;
        }
    }
    Some(Request {
        reply_hwnd: u32_at(buf, 0)?,
        reply_msg: u32_at(buf, 4)?,
        offset: u32_at(buf, 12)?,
        max: u32_at(buf, 16)?,
        flags: u32_at(buf, 20)?,
        sort: u32_at(buf, 24)?,
        search: String::from_utf16_lossy(&units),
    })
}

/// Arma el pedido que mandan los clientes (lo usan el shim y los tests).
pub fn build_request(r: &Request) -> Vec<u8> {
    let mut out = Vec::with_capacity(28 + r.search.len() * 2 + 2);
    let units: Vec<u16> = r.search.encode_utf16().collect();
    for v in [
        r.reply_hwnd,
        r.reply_msg,
        units.len() as u32,
        r.offset,
        r.max,
        r.flags,
        r.sort,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

fn push_str(out: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().collect();
    out.extend_from_slice(&(units.len() as u32).to_le_bytes());
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
    out.extend_from_slice(&0u16.to_le_bytes());
}

/// Serializa una página de resultados. `flags` decide qué campos viajan.
pub fn build_reply(total: u32, offset: u32, flags: u32, items: &[ReplyItem]) -> Vec<u8> {
    let n = items.len();
    let data_start = HEADER_LEN + n * ITEM_LEN;
    let mut buf = vec![0u8; data_start];
    buf[0..4].copy_from_slice(&total.to_le_bytes());
    buf[4..8].copy_from_slice(&(n as u32).to_le_bytes());
    buf[8..12].copy_from_slice(&offset.to_le_bytes());
    buf[12..16].copy_from_slice(&flags.to_le_bytes());

    let mut data: Vec<u8> = Vec::with_capacity(n * 64);
    for (i, it) in items.iter().enumerate() {
        let at = HEADER_LEN + i * ITEM_LEN;
        let item_off = (data_start + data.len()) as u32;
        buf[at..at + 4].copy_from_slice(&it.flags().to_le_bytes());
        buf[at + 4..at + 8].copy_from_slice(&item_off.to_le_bytes());

        if flags & req::NAME != 0 {
            push_str(&mut data, &it.name);
        }
        if flags & (req::PATH | req::FULL_PATH) != 0 {
            push_str(&mut data, &it.path);
        }
        if flags & req::SIZE != 0 {
            data.extend_from_slice(&it.size.unwrap_or(UNKNOWN_SIZE).to_le_bytes());
        }
        if flags & req::DATE_MODIFIED != 0 {
            data.extend_from_slice(&it.modified_ft.to_le_bytes());
        }
        if flags & req::DATE_ACCESSED != 0 {
            data.extend_from_slice(&it.accessed_ft.to_le_bytes());
        }
        if flags & req::ATTRIBUTES != 0 {
            data.extend_from_slice(&it.attributes.to_le_bytes());
        }
        if flags & req::RUN_COUNT != 0 {
            data.extend_from_slice(&it.run_count.to_le_bytes());
        }
    }
    buf.extend_from_slice(&data);
    buf
}

/// Lee una respuesta. Es el parser del shim y el oráculo de los tests.
pub fn parse_reply(buf: &[u8]) -> Option<Reply> {
    if buf.len() < HEADER_LEN {
        return None;
    }
    let total = u32_at(buf, 0)?;
    let num = u32_at(buf, 4)? as usize;
    let offset = u32_at(buf, 8)?;
    let flags = u32_at(buf, 12)?;
    if buf.len() < HEADER_LEN + num * ITEM_LEN {
        return None;
    }
    let mut items = Vec::with_capacity(num);
    for i in 0..num {
        let at = HEADER_LEN + i * ITEM_LEN;
        let f = u32_at(buf, at)?;
        let mut p = u32_at(buf, at + 4)? as usize;

        let read_str = |p: &mut usize| -> Option<String> {
            let len = u32_at(buf, *p)? as usize;
            *p += 4;
            let end = *p + len * 2;
            if end + 2 > buf.len() {
                return None;
            }
            let mut units = Vec::with_capacity(len);
            for k in 0..len {
                let q = *p + 2 * k;
                units.push(u16::from_le_bytes([buf[q], buf[q + 1]]));
            }
            *p = end + 2;
            Some(String::from_utf16_lossy(&units))
        };

        let name = if flags & req::NAME != 0 { read_str(&mut p)? } else { String::new() };
        let path = if flags & (req::PATH | req::FULL_PATH) != 0 { read_str(&mut p)? } else { String::new() };
        let size = if flags & req::SIZE != 0 {
            let v = u64_at(buf, p)?;
            p += 8;
            if v == UNKNOWN_SIZE {
                None
            } else {
                Some(v)
            }
        } else {
            None
        };
        let take8 = |p: &mut usize| -> Option<u64> {
            let v = u64_at(buf, *p)?;
            *p += 8;
            Some(v)
        };
        let modified_ft = if flags & req::DATE_MODIFIED != 0 { take8(&mut p)? } else { 0 };
        let accessed_ft = if flags & req::DATE_ACCESSED != 0 { take8(&mut p)? } else { 0 };
        let attributes = if flags & req::ATTRIBUTES != 0 {
            let v = u32_at(buf, p)?;
            p += 4;
            v
        } else {
            0
        };
        let run_count = if flags & req::RUN_COUNT != 0 { u32_at(buf, p)? } else { 0 };
        items.push(ReplyItem {
            name,
            path,
            size,
            modified_ft,
            accessed_ft,
            attributes,
            run_count,
            is_dir: f & item::IS_DIR != 0,
            is_drive: f & item::DRIVE != 0,
        });
    }
    Some(Reply { total, offset, flags, items })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(name: &str, path: &str, size: Option<u64>, dir: bool) -> ReplyItem {
        ReplyItem {
            name: name.into(),
            path: path.into(),
            size,
            modified_ft: 133_000_000_000_000_000,
            is_dir: dir,
            ..Default::default()
        }
    }

    #[test]
    fn request_roundtrip() {
        let r = Request {
            reply_hwnd: 0x1234,
            reply_msg: 0x5745_5A31,
            offset: 7,
            max: 250_000,
            flags: req::BASIC,
            sort: sort::NAME_ASC,
            search: "path:\"C:\\Users\"".into(),
        };
        assert_eq!(parse_request(&build_request(&r)).unwrap(), r);
    }

    #[test]
    fn request_tolerates_zero_length_prefix() {
        // Los clientes mandan search_length = 0 y confían en el NUL final.
        let mut buf = build_request(&Request {
            search: "file: dm:today".into(),
            ..Default::default()
        });
        buf[8..12].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(parse_request(&buf).unwrap().search, "file: dm:today");
    }

    #[test]
    fn reply_roundtrip_all_flags() {
        let flags = req::BASIC | req::DATE_ACCESSED | req::ATTRIBUTES | req::RUN_COUNT;
        let items = vec![
            item("C:", "", None, true),
            item("informe.pdf", "C:\\Users\\Lucio\\Docs", Some(4_096), false),
            item("ñandú.txt", "C:\\Users\\Lucio\\Docs", Some(0), false),
        ];
        let raw = build_reply(3, 0, flags, &items);
        let back = parse_reply(&raw).unwrap();
        assert_eq!(back.total, 3);
        assert_eq!(back.items, items);
    }

    #[test]
    fn items_start_at_twenty() {
        // WizEverything lee las filas a partir del byte 20: no se puede mover.
        let items = vec![item("a", "C:", Some(1), false), item("b", "C:", Some(2), false)];
        let raw = build_reply(2, 0, req::BASIC, &items);
        assert_eq!(HEADER_LEN, 20);
        for i in 0..2 {
            let off = u32_at(&raw, 20 + i * 8 + 4).unwrap() as usize;
            assert!(off >= 20 + 2 * 8);
        }
    }

    #[test]
    fn folder_size_is_unknown() {
        let raw = build_reply(1, 0, req::SIZE, &[item("dir", "C:", None, true)]);
        let back = parse_reply(&raw).unwrap();
        assert_eq!(back.items[0].size, None);
        assert!(back.items[0].is_dir);
    }

    #[test]
    fn flags_without_name_skip_strings() {
        let raw = build_reply(1, 0, req::SIZE, &[item("ignorado", "C:", Some(9), false)]);
        let back = parse_reply(&raw).unwrap();
        assert_eq!(back.items[0].name, "");
        assert_eq!(back.items[0].size, Some(9));
    }

    #[test]
    fn truncated_input_is_rejected() {
        assert!(parse_request(&[0u8; 27]).is_none());
        assert!(parse_reply(&[0u8; 19]).is_none());
        let raw = build_reply(1, 0, req::BASIC, &[item("x", "C:", Some(1), false)]);
        assert!(parse_reply(&raw[..raw.len() - 4]).is_none());
    }

    #[test]
    fn drive_flag_wins_over_folder() {
        let mut it = item("D:", "", None, true);
        it.is_drive = true;
        let raw = build_reply(1, 0, req::NAME, &[it]);
        let back = parse_reply(&raw).unwrap();
        assert!(back.items[0].is_drive);
        assert!(back.items[0].is_dir);
    }
}
