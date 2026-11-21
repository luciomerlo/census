//! Lista de *data runs* de un atributo `$DATA` y un `Read + Seek` sobre el volumen.

#![cfg(windows)]

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

use crate::win::VolumeHandle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Run {
    /// `None` = hueco disperso (no hay LCN).
    pub lcn: Option<i64>,
    pub clusters: u64,
}

/// Parsea una runlist NTFS. Ignora lo que siga al terminador (0x00).
pub fn parse_runlist(bytes: &[u8]) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut i = 0usize;
    let mut lcn: i64 = 0;
    while i < bytes.len() {
        let header = bytes[i];
        i += 1;
        if header == 0 {
            break;
        }
        let len_size = (header & 0x0f) as usize;
        let off_size = (header >> 4) as usize;
        if len_size == 0 || i + len_size + off_size > bytes.len() {
            break;
        }
        let mut clusters: u64 = 0;
        for k in 0..len_size {
            clusters |= (bytes[i + k] as u64) << (8 * k);
        }
        i += len_size;

        if off_size == 0 {
            runs.push(Run { lcn: None, clusters });
            continue;
        }
        let mut raw: u64 = 0;
        for k in 0..off_size {
            raw |= (bytes[i + k] as u64) << (8 * k);
        }
        i += off_size;
        // Extensión de signo del delta (los LCN pueden retroceder).
        let bits = 8 * off_size as u32;
        let delta = if bits < 64 && raw & (1u64 << (bits - 1)) != 0 {
            (raw | (!0u64 << bits)) as i64
        } else {
            raw as i64
        };
        lcn += delta;
        runs.push(Run { lcn: Some(lcn), clusters });
    }
    runs
}

/// Lee la MFT (o cualquier stream no residente) como si fuera un archivo plano.
pub struct RunsReader {
    vol: Arc<VolumeHandle>,
    runs: Vec<Run>,
    /// Offset en bytes del comienzo de cada run.
    starts: Vec<u64>,
    cluster: u64,
    len: u64,
    pos: u64,
}

impl RunsReader {
    pub fn new(vol: Arc<VolumeHandle>, runs: Vec<Run>, cluster: u64, len: u64) -> Self {
        let mut starts = Vec::with_capacity(runs.len());
        let mut acc = 0u64;
        for r in &runs {
            starts.push(acc);
            acc += r.clusters * cluster;
        }
        Self { vol, runs, starts, cluster, len: len.min(acc), pos: 0 }
    }

    pub fn len_bytes(&self) -> u64 {
        self.len
    }

    fn locate(&self, pos: u64) -> Option<(usize, u64)> {
        if pos >= self.len {
            return None;
        }
        // Búsqueda binaria: la MFT puede estar partida en cientos de runs.
        let idx = match self.starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        Some((idx, pos - self.starts[idx]))
    }
}

impl Read for RunsReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || self.pos >= self.len {
            return Ok(0);
        }
        let mut written = 0usize;
        while written < buf.len() && self.pos < self.len {
            let Some((idx, within)) = self.locate(self.pos) else {
                break;
            };
            let run = self.runs[idx];
            let run_bytes = run.clusters * self.cluster;
            let avail = (run_bytes - within).min((buf.len() - written) as u64) as usize;
            match run.lcn {
                Some(lcn) => {
                    let phys = lcn as u64 * self.cluster + within;
                    self.vol.read_at(phys, &mut buf[written..written + avail])?;
                }
                None => buf[written..written + avail].fill(0),
            }
            written += avail;
            self.pos += avail as u64;
        }
        Ok(written)
    }
}

impl Seek for RunsReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let new = match from {
            SeekFrom::Start(n) => n as i64,
            SeekFrom::End(n) => self.len as i64 + n,
            SeekFrom::Current(n) => self.pos as i64 + n,
        };
        if new < 0 {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "seek negativo"));
        }
        self.pos = new as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn single_run() {
        // header 0x11: 1 byte de largo, 1 byte de offset; largo 0x28, lcn 0x0c
        let runs = parse_runlist(&[0x11, 0x28, 0x0c, 0x00]);
        assert_eq!(runs, vec![Run { lcn: Some(12), clusters: 40 }]);
    }

    #[test]
    fn multiple_runs_accumulate_lcn() {
        let runs = parse_runlist(&[0x11, 0x10, 0x20, 0x11, 0x08, 0xFE, 0x00]);
        // segundo run: delta -2 -> lcn 30
        assert_eq!(
            runs,
            vec![
                Run { lcn: Some(32), clusters: 16 },
                Run { lcn: Some(30), clusters: 8 },
            ]
        );
    }

    #[test]
    fn sparse_run_has_no_lcn() {
        let runs = parse_runlist(&[0x01, 0x05, 0x11, 0x02, 0x03, 0x00]);
        assert_eq!(
            runs,
            vec![Run { lcn: None, clusters: 5 }, Run { lcn: Some(3), clusters: 2 }]
        );
    }

    #[test]
    fn two_byte_offsets() {
        // offset de 2 bytes con signo: 0x1234
        let runs = parse_runlist(&[0x21, 0x04, 0x34, 0x12, 0x00]);
        assert_eq!(runs, vec![Run { lcn: Some(0x1234), clusters: 4 }]);
    }

    #[test]
    fn truncated_runlist_stops() {
        assert!(parse_runlist(&[0x11, 0x10]).is_empty());
        assert!(parse_runlist(&[]).is_empty());
        assert!(parse_runlist(&[0x00]).is_empty());
    }
}
