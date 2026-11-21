//! Fuentes de registros: MFT (producción) y recorrido de directorios (pruebas).

#![cfg(windows)]

use std::io::{Read, Seek};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use mft::attribute::MftAttributeContent;
use mft::MftEntry;

use crate::runs::{parse_runlist, RunsReader};
use crate::win;

/// Un registro ya normalizado: lo único que el índice necesita.
#[derive(Debug, Clone, Default)]
pub struct RawRecord {
    pub frn: u64,
    pub parent_frn: u64,
    pub name: String,
    pub size: u64,
    pub mtime: i64,
    pub atime: i64,
    pub attrs: u32,
    pub is_dir: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ScanStats {
    pub records: u64,
    pub used: u64,
    pub skipped: u64,
    pub bytes_read: u64,
    pub elapsed_ms: u128,
    /// Sólo se usa para reportar el fallo de un tramo desde un hilo.
    pub error: Option<String>,
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Recorre los atributos de un registro crudo.
/// `f(tipo, no_residente, largo_del_nombre, bytes_del_atributo)`.
pub fn for_each_attr(rec: &[u8], mut f: impl FnMut(u32, u8, u8, &[u8])) {
    if rec.len() < 0x18 {
        return;
    }
    let mut off = u16_at(rec, 0x14) as usize;
    let used = (u32_at(rec, 0x18) as usize).min(rec.len());
    while off + 4 <= used {
        let ty = u32_at(rec, off);
        if ty == 0xFFFF_FFFF {
            break;
        }
        if off + 8 > used {
            break;
        }
        let len = u32_at(rec, off + 4) as usize;
        if len < 16 || off + len > used {
            break;
        }
        let non_resident = rec[off + 8];
        let name_len = rec[off + 9];
        f(ty, non_resident, name_len, &rec[off..off + len]);
        off += len;
    }
}

/// Tamaño lógico del `$DATA` sin nombre, leído del registro crudo.
fn unnamed_data_size(rec: &[u8]) -> Option<u64> {
    let mut out = None;
    for_each_attr(rec, |ty, nr, name_len, attr| {
        if ty != 0x80 || name_len != 0 {
            return;
        }
        if nr == 1 {
            if attr.len() >= 0x38 {
                let v = u64::from_le_bytes(attr[0x30..0x38].try_into().unwrap());
                out = Some(v);
            }
        } else if attr.len() >= 0x14 {
            let value_len = u32_at(attr, 0x10) as u64;
            out = Some(value_len);
        }
    });
    out
}

/// Runlist del `$MFT` (atributo `$DATA` sin nombre, no residente).
fn mft_runs(rec0: &[u8]) -> Option<Vec<crate::runs::Run>> {
    let mut out = None;
    for_each_attr(rec0, |ty, nr, name_len, attr| {
        if ty != 0x80 || name_len != 0 || nr != 1 || attr.len() < 0x24 {
            return;
        }
        let run_off = u16_at(attr, 0x20) as usize;
        if run_off < attr.len() {
            out = Some(parse_runlist(&attr[run_off..]));
        }
    });
    out
}

/// Cuántos hilos usan la lectura. `CENSUS_THREADS` manda; 1 = secuencial.
fn thread_count() -> usize {
    if let Ok(v) = std::env::var("CENSUS_THREADS") {
        if let Ok(n) = v.parse::<usize>() {
            return n.clamp(1, 32);
        }
    }
    std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).clamp(1, 8)
}

/// Parte el rango de la MFT en tramos alineados al tamaño de registro.
fn plan_chunks(len: u64, rec_size: usize, threads: usize) -> Vec<(u64, u64)> {
    let rec = rec_size as u64;
    let total = len / rec;
    if total == 0 {
        return Vec::new();
    }
    let threads = threads.max(1) as u64;
    let per = total.div_ceil(threads);
    let mut out = Vec::new();
    let mut start = 0u64;
    while start < total {
        let end = (start + per).min(total);
        out.push((start * rec, (end - start) * rec));
        start = end;
    }
    out
}

/// Carga inicial leyendo el `$MFT` en bloque, en paralelo. Requiere elevación.
///
/// El parseo no es el cuello: el disco sí. Repartir la lectura entre varios
/// hilos es lo que convierte una lectura secuencial de 1,8 GB en algo que usa
/// el ancho de banda real del NVMe.
pub fn scan_mft(letter: char, mut emit: impl FnMut(RawRecord)) -> Result<ScanStats, String> {
    let start = Instant::now();
    win::enable_raw_volume_access();

    let (cluster, rec_size, runs, total_len) = {
        let vol = Arc::new(win::open_volume(letter).map_err(|e| {
            format!("no se pudo abrir la unidad {letter}: ({e}). Hace falta ejecutar como administrador")
        })?);
        let vd = win::volume_data(&vol).map_err(|e| format!("FSCTL_GET_NTFS_VOLUME_DATA falló: {e}"))?;
        if vd.bytes_per_file_record_segment == 0 || vd.bytes_per_cluster == 0 {
            return Err("geometría NTFS inválida".into());
        }
        let cluster = vd.bytes_per_cluster as u64;
        let rec_size = vd.bytes_per_file_record_segment as usize;

        // Registro 0: de ahí sale la runlist del propio $MFT.
        let mut rec0 = vec![0u8; (cluster as usize).max(rec_size)];
        vol.read_at(vd.mft_start_lcn as u64 * cluster, &mut rec0)
            .map_err(|e| format!("no se pudo leer el registro 0 de la MFT: {e}"))?;
        let runs = mft_runs(&rec0[..rec_size]).ok_or("no se pudo parsear la runlist de $MFT")?;
        (cluster, rec_size, runs, vd.mft_valid_data_length as u64)
    };

    let threads = thread_count();
    let chunks = plan_chunks(total_len, rec_size, threads);
    let mut stats = ScanStats::default();

    if chunks.len() <= 1 {
        let mut out = Vec::new();
        scan_range(letter, &runs, cluster, 0, total_len, rec_size, 0, &mut |r| out.push(r))
            .map_err(|e| format!("lectura de la MFT falló: {e}"))?;
        for r in out {
            stats.used += 1;
            emit(r);
        }
        stats.records = total_len / rec_size as u64;
        stats.bytes_read = total_len;
        stats.elapsed_ms = start.elapsed().as_millis();
        return Ok(stats);
    }

    // Cada hilo lee su tramo y manda lotes; el hilo principal arma el índice
    // mientras los demás siguen leyendo.
    let (tx, rx) = std::sync::mpsc::channel::<(Vec<RawRecord>, ScanStats)>();
    let mut handles = Vec::with_capacity(chunks.len());
    for (offset, len) in chunks {
        let tx = tx.clone();
        let runs = runs.clone();
        handles.push(std::thread::spawn(move || {
            let mut batch: Vec<RawRecord> = Vec::with_capacity(4096);
            let mut local = ScanStats::default();
            let flush = |batch: &mut Vec<RawRecord>, local: &mut ScanStats| {
                if !batch.is_empty() {
                    let _ = tx.send((std::mem::take(batch), std::mem::take(local)));
                }
            };
            let res = scan_range(letter, &runs, cluster, offset, len, rec_size, offset, &mut |r| {
                batch.push(r);
                if batch.len() >= 4096 {
                    flush(&mut batch, &mut local);
                }
            });
            match res {
                Ok(st) => {
                    local.records += st.records;
                    local.skipped += st.skipped;
                    local.bytes_read += st.bytes_read;
                    flush(&mut batch, &mut local);
                }
                Err(e) => {
                    local.error = Some(e.to_string());
                    flush(&mut batch, &mut local);
                }
            }
        }));
    }
    drop(tx);

    let mut first_error: Option<String> = None;
    for (batch, st) in rx {
        stats.records += st.records;
        stats.skipped += st.skipped;
        stats.bytes_read += st.bytes_read;
        if let Some(e) = st.error {
            first_error.get_or_insert(e);
        }
        for r in batch {
            stats.used += 1;
            emit(r);
        }
    }
    for h in handles {
        let _ = h.join();
    }
    if let Some(e) = first_error {
        return Err(format!("lectura de la MFT falló: {e}"));
    }

    stats.elapsed_ms = start.elapsed().as_millis();
    Ok(stats)
}

/// Lee `[offset, offset + len)` de la MFT y emite los registros usables.
#[allow(clippy::too_many_arguments)]
fn scan_range(
    letter: char,
    runs: &[crate::runs::Run],
    cluster: u64,
    offset: u64,
    len: u64,
    rec_size: usize,
    first_recno: u64,
    emit: &mut dyn FnMut(RawRecord),
) -> std::io::Result<ScanStats> {
    let vol = Arc::new(win::open_volume(letter)?);
    let mut reader = RunsReader::new(vol, runs.to_vec(), cluster, offset + len);
    reader.seek(std::io::SeekFrom::Start(offset))?;

    let mut stats = ScanStats::default();
    let chunk_bytes = ((8 * 1024 * 1024) / rec_size).max(1) * rec_size;
    let mut buf = vec![0u8; chunk_bytes.min(len as usize).max(rec_size)];
    let end = offset + len;
    let mut pos = offset;
    let mut recno = first_recno / rec_size as u64;

    while pos < end {
        let want = ((end - pos) as usize).min(buf.len());
        let mut filled = 0usize;
        while filled < want {
            match reader.read(&mut buf[filled..want]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(e) => return Err(e),
            }
        }
        if filled == 0 {
            break;
        }
        stats.bytes_read += filled as u64;
        let mut off = 0usize;
        while off + rec_size <= filled {
            let rec = &buf[off..off + rec_size];
            off += rec_size;
            stats.records += 1;
            let allocated = &rec[0..4] == b"FILE" && rec[0x16] & 0x01 != 0;
            let this = recno;
            recno += 1;
            if !allocated {
                stats.skipped += 1;
                continue;
            }
            let entry = match MftEntry::from_buffer(rec.to_vec(), this) {
                Ok(e) => e,
                Err(_) => {
                    stats.skipped += 1;
                    continue;
                }
            };
            // Los registros de extensión sólo repiten atributos del base.
            if entry.header.base_reference.entry != 0 {
                stats.skipped += 1;
                continue;
            }
            if let Some(r) = to_record(&entry, unnamed_data_size(rec)) {
                stats.used += 1;
                emit(r);
            } else {
                stats.skipped += 1;
            }
        }
        pos += filled as u64;
    }
    Ok(stats)
}

fn to_record(entry: &MftEntry, data_size: Option<u64>) -> Option<RawRecord> {
    let mut best: Option<(u8, mft::FileNameAttr)> = None;
    let mut mtime = 0i64;
    let mut atime = 0i64;
    let mut attrs = 0u32;
    let mut have_std = false;

    for attr in entry.iter_attributes().flatten() {
        match attr.data {
            MftAttributeContent::AttrX30(f) => {
                // Win32AndDos(3) > Win32(1) > POSIX(0) > DOS(2): el nombre "lindo".
                let score = match f.namespace.clone() as u8 {
                    3 => 3u8,
                    1 => 2,
                    0 => 1,
                    _ => 0,
                };
                if best.as_ref().map(|(s, _)| score > *s).unwrap_or(true) {
                    best = Some((score, f));
                }
            }
            MftAttributeContent::AttrX10(si) => {
                mtime = si.modified.timestamp();
                atime = si.accessed.timestamp();
                attrs = si.file_flags.bits();
                have_std = true;
            }
            _ => {}
        }
    }

    let (_, fname) = best?;
    if fname.name.is_empty() {
        return None;
    }
    if !have_std {
        mtime = fname.modified.timestamp();
        atime = fname.accessed.timestamp();
        attrs = fname.flags.bits();
    }
    let is_dir = entry.is_dir();
    let size = if is_dir { u64::MAX } else { data_size.unwrap_or(fname.logical_size) };

    Some(RawRecord {
        frn: entry.header.record_number,
        parent_frn: fname.parent.entry,
        name: fname.name,
        size,
        mtime,
        atime,
        attrs,
        is_dir,
    })
}

/// Recorrido clásico, sólo para pruebas sin elevación y volúmenes no NTFS.
pub fn scan_dir(root: &Path, mut emit: impl FnMut(RawRecord)) -> Result<ScanStats, String> {
    use std::time::UNIX_EPOCH;
    let start = Instant::now();
    let mut stats = ScanStats::default();
    let mut next = 1_000_000u64;
    let root_frn = 5u64;
    // La raiz se llama como la unidad que contiene a `root`: "G:" y no "C:".
    let letter = root
        .components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .filter(|s| s.len() == 2 && s.ends_with(':'))
        .unwrap_or_else(|| "C:".to_string());

    // La raíz del índice siempre es la unidad; la carpeta pedida entra como
    // una cadena de antecesores, así las rutas completas salen bien.
    stats.used += 1;
    emit(RawRecord {
        frn: root_frn,
        parent_frn: root_frn,
        name: letter,
        size: u64::MAX,
        mtime: 0,
        atime: 0,
        attrs: 0x10,
        is_dir: true,
    });

    let mut deepest = root_frn;
    for comp in root.components() {
        let std::path::Component::Normal(part) = comp else {
            continue;
        };
        let frn = next;
        next += 1;
        stats.used += 1;
        emit(RawRecord {
            frn,
            parent_frn: deepest,
            name: part.to_string_lossy().to_string(),
            size: u64::MAX,
            mtime: 0,
            atime: 0,
            attrs: 0x10,
            is_dir: true,
        });
        deepest = frn;
    }

    let mut stack: Vec<(std::path::PathBuf, u64)> = vec![(root.to_path_buf(), deepest)];
    while let Some((dir, parent)) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for e in entries.flatten() {
            let md = match e.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            let is_dir = md.is_dir();
            let to_unix = |t: std::io::Result<std::time::SystemTime>| -> i64 {
                t.ok()
                    .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0)
            };
            let frn = next;
            next += 1;
            let name = e.file_name().to_string_lossy().to_string();
            stats.records += 1;
            stats.used += 1;
            emit(RawRecord {
                frn,
                parent_frn: parent,
                name: name.clone(),
                size: if is_dir { u64::MAX } else { md.len() },
                mtime: to_unix(md.modified()),
                atime: to_unix(md.accessed()),
                attrs: 0,
                is_dir,
            });
            if is_dir {
                stack.push((e.path(), frn));
            }
        }
    }
    stats.elapsed_ms = start.elapsed().as_millis();
    Ok(stats)
}
