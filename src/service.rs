//! Servicio: índice + caché de resultados, listo para el IPC.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use census_ipc::protocol::ReplyItem;
use census_ipc::server::Searcher;

use crate::index::{Index, F_DIR, F_ROOT};
use crate::query::{self, Sort};
use crate::time::unix_to_filetime;

const CACHE_ENTRIES: usize = 4;

pub struct Service {
    pub index: Arc<Index>,
    /// Últimas búsquedas ya ordenadas: paginar no vuelve a filtrar ni a ordenar.
    cache: Mutex<Vec<(u64, Arc<Vec<u32>>)>>,
}

fn key_of(raw: &str, sort: Sort, generation: u64) -> u64 {
    let mut h = DefaultHasher::new();
    raw.hash(&mut h);
    (sort as u8).hash(&mut h);
    generation.hash(&mut h);
    h.finish()
}

impl Service {
    pub fn new(index: Arc<Index>) -> Self {
        Self {
            index,
            cache: Mutex::new(Vec::with_capacity(CACHE_ENTRIES)),
        }
    }

    /// Devuelve (total, página de ids ya ordenada).
    pub fn page(&self, raw: &str, sort: Sort, offset: usize, max: usize) -> (usize, Vec<u32>) {
        let key = key_of(raw, sort, self.index.built_at as u64);
        let cached = {
            let cache = self.cache.lock().unwrap();
            cache.iter().find(|(k, _)| *k == key).map(|(_, v)| Arc::clone(v))
        };
        let ids = match cached {
            Some(v) => v,
            None => {
                let q = query::parse(raw, &self.index);
                let mut ids = self.index.match_ids(&q);
                self.index.sort_ids(&mut ids, sort);
                let arc = Arc::new(ids);
                let mut cache = self.cache.lock().unwrap();
                if cache.len() >= CACHE_ENTRIES {
                    cache.remove(0);
                }
                cache.push((key, Arc::clone(&arc)));
                arc
            }
        };
        let total = ids.len();
        let end = if max == 0 { total } else { (offset + max).min(total) };
        let start = offset.min(total);
        (total, ids[start..end].to_vec())
    }
}

impl Searcher for Service {
    fn is_ready(&self) -> bool {
        !self.index.is_empty()
    }

    fn search(&self, search: &str, sort: u32, offset: u32, max: u32, _flags: u32) -> (u32, Vec<ReplyItem>) {
        let sort = Sort::from_proto(sort);
        let (total, ids) = self.page(search, sort, offset as usize, max as usize);
        let items = ids
            .iter()
            .map(|&id| {
                let n = &self.index.nodes[id as usize];
                ReplyItem {
                    name: self.index.name_string(id),
                    path: self.index.dir_path(id),
                    size: if n.size == u64::MAX { None } else { Some(n.size) },
                    modified_ft: unix_to_filetime(n.mtime),
                    accessed_ft: unix_to_filetime(n.atime),
                    attributes: n.attrs,
                    run_count: 0,
                    is_dir: n.flags & F_DIR != 0,
                    is_drive: n.flags & F_ROOT != 0,
                }
            })
            .collect();
        (total as u32, items)
    }
}
