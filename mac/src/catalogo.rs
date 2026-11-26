//! Catálogo de snapshots `.cnx`: muchos volúmenes, una sola consulta.
//!
//! Cada `.cnx` es de un volumen (§10: la letra no es identidad). El catálogo
//! carga todos los que estén presentes, reconcilia contra los montajes que hay
//! ahora (online / offline y remapeo del prefijo) y consulta en paralelo.
//!
//! Reglas de Atlas que se respetan acá:
//!
//! * Un volumen que no está montado queda `offline` y **no se consulta** salvo
//!   que el usuario lo pida (`include_offline`).
//! * El resultado no depende del orden en que terminen los hilos: se ordena por
//!   la clave pedida y se desempata por `(volume_id, path)`, que es único.
//! * Un `.cnx` corrupto o ilegible se ignora con aviso; los demás siguen
//!   respondiendo. Nunca se mezclan dos volúmenes para "arreglar" un archivo.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::query::{self, Sort};
use crate::snapshot::{Snapshot, EXT};
use crate::types::{Kind, Volume};

/// Aviso no fatal: un archivo que no se pudo usar y por qué.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub file: String,
    pub reason: String,
}

/// Un volumen cargado del catálogo.
#[derive(Clone, Debug)]
pub struct LoadedVolume {
    pub path: PathBuf,
    pub volume_id: String,
    pub mount: String,
    /// Montado ahora mismo. Los offline existen pero no se leen por defecto.
    pub online: bool,
    pub snapshot: Snapshot,
}

impl LoadedVolume {
    pub fn nodes(&self) -> u64 {
        self.snapshot.index.nodes.len() as u64
    }

    pub fn marca(&self) -> String {
        self.snapshot.index.marca.as_str().to_string()
    }
}

/// Una fila de `census snapshot list --json`. Los nombres son contrato:
/// `Sort`/Atlas los consumen y no se cambian después.
#[derive(Clone, Debug, Serialize)]
pub struct VolumeInfo {
    pub volume_id: String,
    pub mount: String,
    pub online: bool,
    pub nodes: u64,
    pub marca: String,
    pub content_hash: String,
    pub built_at: i64,
    pub generation: u64,
    pub pc_id: String,
    pub host: String,
    pub platform: String,
    pub arch: String,
    pub app_version: String,
    pub path: String,
}

/// Una fila de resultado. Lleva procedencia (`volume_id`, `marca`) para que la
/// ventana pueda decir de dónde salió y qué tan fresca es.
#[derive(Clone, Debug, Serialize)]
pub struct Hit {
    pub volume_id: String,
    pub path: String,
    pub name: String,
    /// Nombre plegado; sólo se usa para ordenar, no viaja al JSON.
    #[serde(skip)]
    pub folded: Vec<u8>,
    pub size: u64,
    pub mtime_ns: i64,
    pub kind: Kind,
    pub online: bool,
    pub marca: String,
}

/// Catálogo de snapshots de esta PC.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub volumes: Vec<LoadedVolume>,
    pub notices: Vec<Notice>,
}

impl Catalog {
    /// Carga todos los `.cnx` del directorio. Un archivo roto no aborta el
    /// resto: se anota en `notices`.
    pub fn load(dir: &Path) -> Catalog {
        let mut cat = Catalog::default();
        let mut rutas: Vec<PathBuf> = match std::fs::read_dir(dir) {
            Ok(rd) => rd
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().is_some_and(|e| e == EXT))
                .collect(),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => {
                cat.notices.push(Notice {
                    file: dir.display().to_string(),
                    reason: format!("no se pudo leer el directorio de snapshots: {e}"),
                });
                Vec::new()
            }
        };
        // Orden de carga determinista, para que los avisos y el desempate no
        // dependan del orden del sistema de archivos.
        rutas.sort();
        for path in rutas {
            match std::fs::read(&path) {
                Ok(bytes) => match Snapshot::load(&bytes) {
                    Ok(snap) => cat.push(path, snap),
                    Err(e) => cat.notices.push(Notice {
                        file: path.display().to_string(),
                        reason: e.to_string(),
                    }),
                },
                Err(e) => cat.notices.push(Notice {
                    file: path.display().to_string(),
                    reason: format!("no se pudo leer el archivo: {e}"),
                }),
            }
        }
        cat.dedup();
        cat
    }

    fn push(&mut self, path: PathBuf, snapshot: Snapshot) {
        self.volumes.push(LoadedVolume {
            path,
            volume_id: snapshot.volume.id.to_string(),
            mount: snapshot.volume.mount.clone(),
            online: true,
            snapshot,
        });
    }

    /// Si el mismo `volume_id` aparece dos veces (copias a mano, snapshots
    /// viejos), gana la `Marca` más nueva y el otro queda avisado.
    fn dedup(&mut self) {
        let mut keep: Vec<LoadedVolume> = Vec::with_capacity(self.volumes.len());
        let mut avisos: Vec<Notice> = Vec::new();
        for v in std::mem::take(&mut self.volumes) {
            let Some(i) = keep.iter().position(|k| k.volume_id == v.volume_id) else {
                keep.push(v);
                continue;
            };
            let prev = &keep[i];
            let orden = prev
                .snapshot
                .index
                .marca
                .cmp_same(&v.snapshot.index.marca)
                .unwrap_or_else(|| {
                    prev.snapshot
                        .index
                        .built_at
                        .cmp(&v.snapshot.index.built_at)
                });
            // Empate: gana el primero, que es el de nombre de archivo menor
            // (las rutas se cargan ordenadas, así el resultado es estable).
            let (gana, pierde) = if orden == Ordering::Less {
                (v.path.clone(), keep[i].path.clone())
            } else {
                (keep[i].path.clone(), v.path.clone())
            };
            avisos.push(Notice {
                file: pierde.display().to_string(),
                reason: format!(
                    "duplicado del volumen {}; se usa {}",
                    v.volume_id,
                    gana.display()
                ),
            });
            if orden == Ordering::Less {
                keep[i] = v;
            }
        }
        // Se conservan los avisos previos y se agregan los de duplicados.
        self.notices.extend(avisos);
        self.volumes = keep;
    }

    /// Marca online/offline según los volúmenes montados ahora y remapea el
    /// prefijo de montaje cuando el `volume_id` coincide pero la ruta cambió.
    ///
    /// §10.2: mismo volumen en otro punto de montaje ⇒ remapeo, no reindexado.
    ///
    /// Las raíces sueltas (`dir:<host>:<ruta>`, §10.5) no aparecen como
    /// montaje, así que su estado se resuelve mirando si la ruta existe.
    pub fn reconcile(&mut self, mounted: &[Volume]) {
        for v in &mut self.volumes {
            let found = mounted.iter().find(|m| m.id.as_ref() == v.volume_id);
            match found {
                Some(m) => {
                    v.online = true;
                    if crate::index::normalize_dir(&m.mount) != v.mount {
                        v.snapshot.remap_mount(&m.mount);
                        v.mount = v.snapshot.volume.mount.clone();
                    }
                }
                // Raíz suelta: si la carpeta sigue ahí, el snapshot sirve.
                None if v.volume_id.starts_with("dir:") => {
                    v.online = std::path::Path::new(&v.mount).exists();
                }
                None => v.online = false,
            }
        }
    }

    /// `volume_id` de cada volumen cargado, ordenados. Es la clave estable para
    /// letras sintéticas y manifiestos (nunca el orden de montaje).
    pub fn volume_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.volumes.iter().map(|v| v.volume_id.clone()).collect();
        ids.sort();
        ids
    }

    /// Volúmenes que no están montados ahora mismo.
    pub fn offline(&self) -> Vec<&LoadedVolume> {
        self.volumes.iter().filter(|v| !v.online).collect()
    }

    /// Metadatos de cada volumen, en orden estable por `volume_id`.
    pub fn describe(&self) -> Vec<VolumeInfo> {
        let mut out: Vec<VolumeInfo> = self
            .volumes
            .iter()
            .map(|v| VolumeInfo {
                volume_id: v.volume_id.clone(),
                mount: v.mount.clone(),
                online: v.online,
                nodes: v.nodes(),
                marca: v.marca(),
                content_hash: format!("{:016x}", v.snapshot.index.content_hash),
                built_at: v.snapshot.index.built_at,
                generation: v.snapshot.index.generation,
                pc_id: v.snapshot.index.provenance.pc_id.clone(),
                host: v.snapshot.index.provenance.host.clone(),
                platform: v.snapshot.index.provenance.platform.clone(),
                arch: v.snapshot.index.provenance.arch.clone(),
                app_version: v.snapshot.index.provenance.app_version.clone(),
                path: v.path.display().to_string(),
            })
            .collect();
        out.sort_by(|a, b| a.volume_id.cmp(&b.volume_id));
        out
    }

    /// Consulta todos los volúmenes en paralelo y devuelve filas
    /// deterministas. `offset`/`max` se aplican después de mezclar.
    pub fn search(
        &self,
        text: &str,
        sort: Sort,
        offset: usize,
        max: usize,
        include_offline: bool,
    ) -> Vec<Hit> {
        let seleccion: Vec<&LoadedVolume> = self
            .volumes
            .iter()
            .filter(|v| include_offline || v.online)
            .collect();
        if seleccion.is_empty() {
            return Vec::new();
        }
        // Cada volumen devuelve como mucho `offset + max` filas: es cota
        // suficiente para el recorte global y evita traer todo a memoria.
        let limit = offset.saturating_add(max).max(1);
        let mut todos: Vec<Hit> = if seleccion.len() > 1 && hilos() > 1 {
            std::thread::scope(|s| {
                let handles: Vec<_> = seleccion
                    .iter()
                    .map(|v| s.spawn(move || search_one(v, text, sort, limit)))
                    .collect();
                handles
                    .into_iter()
                    .flat_map(|h| h.join().unwrap_or_default())
                    .collect()
            })
        } else {
            seleccion
                .iter()
                .flat_map(|v| search_one(v, text, sort, limit))
                .collect()
        };
        todos.sort_by(|a, b| compare(sort, a, b));
        todos.into_iter().skip(offset).take(max).collect()
    }

    /// Cantidad total de coincidencias (sin paginar).
    pub fn count(&self, text: &str, include_offline: bool) -> usize {
        self.volumes
            .iter()
            .filter(|v| include_offline || v.online)
            .map(|v| {
                let q = query::parse(text, &v.snapshot.index);
                v.snapshot.index.count(&q)
            })
            .sum()
    }
}

fn hilos() -> usize {
    std::thread::available_parallelism()
        .map(|v| v.get())
        .unwrap_or(1)
}

fn search_one(v: &LoadedVolume, text: &str, sort: Sort, limit: usize) -> Vec<Hit> {
    let ix = &v.snapshot.index;
    let q = query::parse(text, ix);
    let marca = v.marca();
    ix.search(&q, sort, 0, limit)
        .into_iter()
        .map(|id| {
            let n = &ix.nodes[id as usize];
            Hit {
                volume_id: v.volume_id.clone(),
                path: ix.path_of(id),
                name: ix.name_string_lossy(id),
                folded: ix.folded_name(id).to_vec(),
                size: n.size,
                mtime_ns: n.mtime_ns,
                kind: ix.kind(id),
                online: v.online,
                marca: marca.clone(),
            }
        })
        .collect()
}

/// Orden total: la clave pedida y después `(volume_id, path)`. Sin el
/// desempate, dos volúmenes con archivos iguales darían órdenes distintos en
/// cada corrida.
fn compare(sort: Sort, a: &Hit, b: &Hit) -> Ordering {
    let base = match sort {
        Sort::NameAsc => a.folded.cmp(&b.folded),
        Sort::NameDesc => b.folded.cmp(&a.folded),
        Sort::PathAsc => a.path.cmp(&b.path),
        Sort::PathDesc => b.path.cmp(&a.path),
        Sort::SizeAsc => a.size.cmp(&b.size),
        Sort::SizeDesc => b.size.cmp(&a.size),
        Sort::DateAsc => a.mtime_ns.cmp(&b.mtime_ns),
        Sort::DateDesc => b.mtime_ns.cmp(&a.mtime_ns),
    };
    base.then_with(|| a.volume_id.cmp(&b.volume_id))
        .then_with(|| a.path.cmp(&b.path))
        .then_with(|| a.size.cmp(&b.size))
        .then_with(|| a.mtime_ns.cmp(&b.mtime_ns))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::Builder;
    use crate::snapshot::{file_for, save_per_volume};
    use crate::types::{
        disk_hw_id, dir_volume_id, volume_identity, Encoding, Entrada, FileId, Marca, Provenance,
        VolumeId,
    };

    fn volume(mount: &str, serial: &str) -> Volume {
        let hw = disk_hw_id(Some(serial), "Seagate Backup+", 4_000_000, None).unwrap();
        Volume {
            id: volume_identity(&hw, 0, &format!("fs-{serial}")),
            disk_hw_id: Some(hw),
            serial: Some(serial.into()),
            model: Some("Seagate Backup+".into()),
            uuid: [9u8; 16],
            name: "Backup".into(),
            mount: mount.into(),
            fs: "apfs".into(),
            total: 4_000_000,
            free: 1_000_000,
            local: true,
            method: "walk".into(),
            roots: vec![mount.into()],
            ..Default::default()
        }
    }

    fn entrada(vol: &Volume, ino: u64, parent: u64, name: &str, dir: bool, size: u64) -> Entrada {
        let vid: VolumeId = vol.id.clone();
        Entrada {
            id: FileId::new(vid.clone(), ino, 0, 1_000 + ino as i64),
            parent_id: FileId::new(vid, parent, 0, 1_000 + parent as i64),
            dev_id: 1,
            kind: if dir { Kind::Dir } else { Kind::File },
            size,
            alloc: size,
            mtime_ns: 1_700_000_000_000_000_000 + ino as i64,
            btime_ns: 1_000 + ino as i64,
            nlink: 1,
            flags: 0,
            name: name.as_bytes().to_vec(),
            encoding: Encoding::Utf8,
        }
    }

    /// Índice con la raíz en la ruta completa y dos archivos, uno de ellos con
    /// el mismo nombre en todos los volúmenes (para probar el desempate).
    fn indice(vol: &Volume, etiqueta: &str, marca: u64) -> crate::index::Index {
        let mut b = Builder::new(vec![vol.clone()]);
        b.push_batch(&[entrada(vol, 1, 1, &vol.mount, true, 0)]);
        b.push_batch(&[
            entrada(vol, 10, 1, "informe.pdf", false, 4096),
            entrada(vol, 11, 1, format!("nota-{etiqueta}.txt").as_str(), false, 10),
        ]);
        b.finish(
            1_700_000_000,
            1,
            Marca(format!("m1:09090909090909090909090909090909:{marca}")),
            Provenance {
                pc_id: "pc-mac-1".into(),
                host: "mac-de-prueba".into(),
                platform: "macos".into(),
                arch: "aarch64".into(),
                env: "prueba".into(),
                app_version: "0.1.0".into(),
            },
        )
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("census-cat-{}-{tag}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("crear dir temporal");
        dir
    }

    fn catalogo(tag: &str, vols: &[(&str, &str, &str, u64)]) -> (PathBuf, Catalog) {
        let dir = tmp_dir(tag);
        for (mount, serial, etiqueta, marca) in vols {
            let v = volume(mount, serial);
            let ix = indice(&v, etiqueta, *marca);
            let (ok, fallos) = save_per_volume(&ix, &dir);
            assert!(fallos.is_empty(), "{fallos:?}");
            assert_eq!(ok.len(), 1);
        }
        let cat = Catalog::load(&dir);
        (dir, cat)
    }

    #[test]
    fn loads_all_snapshots_and_queries_are_deterministic() {
        let (dir, cat) = catalogo(
            "determinista",
            &[
                ("/Volumes/Uno", "SN-1", "uno", 10),
                ("/Volumes/Dos", "SN-2", "dos", 20),
                ("/Volumes/Tres", "SN-3", "tres", 30),
            ],
        );
        assert!(cat.notices.is_empty(), "{:?}", cat.notices);
        assert_eq!(cat.volumes.len(), 3);
        // Los ids salen ordenados y no dependen del orden de montaje.
        let ids = cat.volume_ids();
        let mut ordenados = ids.clone();
        ordenados.sort();
        assert_eq!(ids, ordenados);

        let a = cat.search("informe", Sort::PathAsc, 0, 50, false);
        let b = cat.search("informe", Sort::PathAsc, 0, 50, false);
        assert_eq!(a.len(), 3, "un archivo por volumen");
        assert_eq!(
            a.iter().map(|h| h.path.clone()).collect::<Vec<_>>(),
            b.iter().map(|h| h.path.clone()).collect::<Vec<_>>(),
            "la consulta no puede depender del orden de los hilos"
        );
        let mut rutas: Vec<String> = a.iter().map(|h| h.path.clone()).collect();
        rutas.sort();
        assert_eq!(
            a.iter().map(|h| h.path.clone()).collect::<Vec<_>>(),
            rutas,
            "orden por ruta pedido explícitamente"
        );
        // Con nombres empatados el desempate es (volume_id, path): sin él, dos
        // volúmenes con el mismo archivo darían un orden distinto por corrida.
        let empatados = cat.search("informe", Sort::NameAsc, 0, 50, false);
        assert_eq!(empatados.len(), 3);
        let ids: Vec<String> = empatados.iter().map(|h| h.volume_id.clone()).collect();
        let mut ids_ordenados = ids.clone();
        ids_ordenados.sort();
        assert_eq!(ids, ids_ordenados);
        assert_eq!(ids, cat.volume_ids());
        // La paginación global también es determinista.
        let pagina = cat.search("informe", Sort::PathAsc, 1, 1, false);
        assert_eq!(pagina.len(), 1);
        assert_eq!(pagina[0].path, a[1].path);
        assert_eq!(cat.count("informe", false), 3);
        // Nombres plegados: la consulta con acento no aplica acá, pero el
        // término simple encuentra los tres.
        assert_eq!(cat.search("nota", Sort::NameAsc, 0, 50, false).len(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn corrupt_snapshot_is_ignored_and_the_rest_still_answer() {
        let (dir, cat) = catalogo(
            "corrupto",
            &[
                ("/Volumes/Bueno", "SN-OK", "ok", 1),
                ("/Volumes/Bueno2", "SN-OK2", "ok2", 2),
            ],
        );
        assert!(cat.notices.is_empty(), "{:?}", cat.notices);
        // Basura con nombre de snapshot.
        std::fs::write(dir.join("roto.cnx"), b"esto no es un snapshot").unwrap();
        // Un snapshot válido pero truncado a la mitad.
        let sano = std::fs::read(dir.join(file_for(&dir, &cat.volumes[0].volume_id))).unwrap();
        std::fs::write(dir.join("truncado.cnx"), &sano[..sano.len() / 2]).unwrap();

        let cat = Catalog::load(&dir);
        assert_eq!(cat.volumes.len(), 2, "los buenos siguen cargando");
        assert_eq!(cat.notices.len(), 2, "{:?}", cat.notices);
        assert!(cat.notices.iter().any(|n| n.file.contains("roto")));
        assert!(cat.notices.iter().any(|n| n.file.contains("truncado")));
        assert_eq!(cat.search("informe", Sort::PathAsc, 0, 50, false).len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_disk_is_offline_and_not_read_by_default() {
        // Dos discos que se alternan en el mismo punto de montaje.
        let (dir, mut cat) = catalogo(
            "alternados",
            &[
                ("/Volumes/Backup", "SN-SEAGATE", "seagate", 10),
                ("/Volumes/Backup", "SN-WD", "wd", 20),
            ],
        );
        assert_eq!(cat.volumes.len(), 2, "dos .cnx, dos volúmenes");
        let seagate = volume("/Volumes/Backup", "SN-SEAGATE");
        let wd = volume("/Volumes/Backup", "SN-WD");
        // Sólo el Seagate está conectado ahora.
        cat.reconcile(std::slice::from_ref(&seagate));
        assert_eq!(cat.volumes.iter().filter(|v| v.online).count(), 1);
        assert_eq!(cat.offline().len(), 1);
        assert_eq!(cat.offline()[0].volume_id, wd.id.to_string());
        // Por defecto no se lee el ausente.
        let solo_online = cat.search("informe", Sort::PathAsc, 0, 50, false);
        assert_eq!(solo_online.len(), 1);
        assert_eq!(solo_online[0].volume_id, seagate.id.to_string());
        assert_eq!(cat.count("informe", false), 1);
        // Con `--todas` aparecen los dos, cada uno con su procedencia.
        let todos = cat.search("informe", Sort::PathAsc, 0, 50, true);
        assert_eq!(todos.len(), 2);
        assert!(todos.iter().any(|h| h.online));
        assert!(todos.iter().any(|h| !h.online));
        // Al conectar el otro disco, el mismo `.cnx` pasa a online y el
        // primero queda offline: no se pisan ni se mezclan.
        cat.reconcile(std::slice::from_ref(&wd));
        assert_eq!(cat.offline()[0].volume_id, seagate.id.to_string());
        // Los otros archivos existen: nota-<etiqueta>.txt no se confunde.
        let notas = cat.search("nota-seagate", Sort::PathAsc, 0, 50, true);
        assert_eq!(notas.len(), 1);
        assert!(!notas[0].online);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_mount_remaps_prefix_without_reindexing() {
        let (dir, mut cat) = catalogo(
            "remapeo",
            &[("/Volumes/A", "SN-A", "a", 5)],
        );
        let v = volume("/Volumes/A", "SN-A");
        cat.reconcile(std::slice::from_ref(&v));
        let antes = cat.search("informe", Sort::PathAsc, 0, 10, false);
        assert_eq!(antes[0].path, "/Volumes/A/informe.pdf");
        let marca_antes = antes[0].marca.clone();
        // El mismo disco aparece montado en otra ruta.
        let mut otro_montaje = v.clone();
        otro_montaje.mount = "/Volumes/B".into();
        otro_montaje.roots = vec!["/Volumes/B".into()];
        cat.reconcile(&[otro_montaje]);
        let despues = cat.search("informe", Sort::PathAsc, 0, 10, false);
        assert_eq!(despues.len(), antes.len(), "no se pierde ningún archivo");
        assert_eq!(despues[0].path, "/Volumes/B/informe.pdf");
        assert_eq!(despues[0].volume_id, antes[0].volume_id, "es el mismo volumen");
        assert_eq!(despues[0].marca, marca_antes, "no se reindexó");
        assert_eq!(despues[0].size, antes[0].size);
        // Y las dos notas siguen estando.
        assert_eq!(cat.count("nota", false), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn directory_root_volume_id_keeps_its_contract_form() {
        // `--root` usa `dir:<host>:<path>` (§10.5); el catálogo no lo confunde
        // con un volumen de hardware.
        assert!(dir_volume_id("mac", "/Users/lucio/Cosas").starts_with("dir:mac:"));
        assert_eq!(volume("/Volumes/X", "SN-X").id.len(), 16);
    }
}
