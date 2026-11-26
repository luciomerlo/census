# Cómo indexar más rápido, por sistema y hardware

Investigación del 2026-11-26 sobre proyectos abiertos y comerciales. **Aviso de calidad:** casi todas las cifras son de los propios autores (README, foros, PR); no encontré benchmarks independientes y controlados. Marco cada dato como *autor*, *anécdota* o *medido aquí*. Lo que no se pudo confirmar figura como tal.

## 1. Principio común

Hay tres costos distintos y conviene separarlos:

| Fase | Qué cuesta | Se acelera con |
|---|---|---|
| Enumerar (primera vez) | Metadatos de todos los archivos | Leer la estructura del filesystem en bloque en vez de pedir archivo por archivo |
| Mantener al día | Detectar qué cambió | Un diario/evento del sistema, no volver a recorrer |
| Reconectar un disco externo | Cambios hechos mientras no estaba | Recorrido completo (los extraíbles rara vez tienen diario) |

El hardware (x86, ARM, RISC-V) casi no cambia el *método*: lo que cambia el tiempo es el medio (NVMe, UFS/eMMC, SD, USB 2.0) y la CPU en el recorrido en paralelo. Las diferencias reales son de **sistema operativo y filesystem**.

## 2. Método más rápido por sistema y filesystem

| Sistema | Filesystem | Primera pasada | Mantener al día | Evidencia |
|---|---|---|---|---|
| **Windows** (x86, x64, ARM64) | NTFS | Leer la **MFT** en bloque (requiere administrador) | **USN Journal** | [WizTree y Everything usan la MFT; Everything sigue el USN](https://news.ycombinator.com/item?id=40451333). UFFS reporta [≈1,96× más rápido que Everything y 26 M de registros en <200 ms con daemon caliente](https://awesome.ecosyste.ms/projects/github.com%2Fskyllc-ai%2Fultrafastfilesearch) (*autor*). CensusIndex: 1,64 M nodos en 4,6–8,7 s (*medido, repo*) |
| Windows | FAT/exFAT, red, nube | Recorrido de directorios (la MFT no existe) | Recorrido o notificaciones de carpeta | Las [unidades FAT32/exFAT, SMB y MTP no tienen MFT](https://windowsforum.com/news/wiztree-fast-ntfs-mft-scans-explained-admin-setup-folder-modes-and-windirstat.447023/); en red se vuelve a la caminata lenta |
| Windows | ReFS | No confirmado (sin MFT) | No confirmado | Sin fuente; CensusIndex ya recorre ReFS |
| **macOS** (Intel, Apple silicon) | APFS/HFS+ | **`getattrlistbulk`** + varios hilos | **FSEvents** con `eventId` persistido (recupera lo ocurrido con la app cerrada) | [1,9× sobre `readdir`+`lstat` en Apple silicon, caché caliente](https://github.com/iikachu/dirstats/pull/17) (*PR*); otro proyecto midió [~1,06× y atribuye la ganancia a los hilos](https://github.com/william1010121/dutype/pull/6); [si solo hay nombres, `readdir` compite](https://mail-archive.com/search?f=1&l=filesystem-dev%40lists.apple.com&o=newest&q=subject%3A%22Re\%3A+readdir+vs.+getdirentriesattr%22). `dust` [bajó 38 % de CPU](https://github.com/bootandy/dust/issues/649) |
| macOS PowerPC | HFS+ | `readdir`+`lstat` (no existe `getattrlistbulk`, es 10.10+) | `kqueue` o reescaneo | Sin fuente de rendimiento; ver `PLATAFORMAS-SOPORTE.md` |
| **Linux** (x86, ARM, RISC-V, PPC, s390x…) | cualquiera | **`getdents64` + `statx` en paralelo** | **`fanotify`** a nivel de montaje (sin un watch por carpeta) | Los indexadores vivos [AntiFind](https://github.com/antisoftworks/antifind), [rfind](https://github.com/YanivSaar/rfind), [repertoire](https://github.com/Sigma-studios/repertoire) y [fflocate](https://github.com/gmatht/fflocate) usan fanotify con handles; versión mínima del kernel **no confirmada** (rfind dice 5.9+) |
| Linux | btrfs | Leer los árboles con `TREE_SEARCH` | fanotify | AntiFind: [10 M de archivos en 177 ms contra 1,90 s de plocate y 17,2 s de `find`](https://github.com/antisoftworks/antifind) (*autor*) |
| Linux | XFS | Árboles de inodos (`bulkstat`) | fanotify | AntiFind lo declara; sin cifra |
| Linux | ext4 | Recorrido paralelo (no hay enumeración masiva sin root) | fanotify | Sin alternativa mejor documentada |
| Linux | NTFS/exFAT/FAT (pendrives) | Recorrido de directorios | Reescaneo al conectar | exFAT y FAT [no tienen índice por nombre: cada listado es secuencial](https://what.thedailywtf.com/post/834228) |
| Linux | cualquiera, base prearmada | `plocate`/`updatedb` | Cada corrida de `updatedb` | [Resultados viejos hasta el próximo `updatedb`](https://github.com/antisoftworks/antifind) |
| **Android** (arm64, armv7, x86, RISC-V) | `/storage/emulated` (FUSE sobre f2fs/ext4) | **`MediaStore.Files`** para lo indexado + recorrido nativo para el resto | **Generaciones de `MediaStore`** disparadas por `JobScheduler` | En un teléfono con ~31 000 archivos: [MediaStore 0,2 s contra 5 s del recorrido pelado](https://github.com/AlanHuang99/Voyager/issues/179) (*anécdota*), pero **13 % de los archivos no estaba** (carpetas `.nomedia`). [`DocumentFile`/SAF fue ~500× más lento que `File`](https://androiderrors.com/why-is-documentfile-so-slow-and-what-should-i-use-instead/) (*anécdota antigua*) |
| Android | Pendrive/SD (FAT32, exFAT) | Recorrido de directorios | Reescaneo al conectar | Android reconoce [principalmente FAT32 y exFAT en OTG](https://en.androidayuda.com/android/general/Android-compatible-file-systems-on-OTG-USB-drives/); NTFS/ext4 [se detectan pero no siempre se montan](https://f-droid.org/packages/app.fayaz.otgmaster/) |
| **FreeBSD, illumos** | UFS/ZFS | Recorrido | `kqueue`/FEN | Compila (`cargo check`); sin medición |

### Hallazgo que cambia el diseño de Android

El experimento de Voyager encontró que **combinar MediaStore con un recorrido solo de lo que le falta no fue más rápido que recorrer todo** (6,9–8,1 s contra ~5 s). Conclusiones para CensusIndex:

1. **Primera indexación:** recorrer todo con el walker nativo; no vale la pena el híbrido.
2. **Mantenimiento:** `MediaStore` sí gana, porque la consulta de cambios es proporcional a lo que cambió y no a los 31 000 archivos. Pero **no ve `.nomedia`**: ese 13 % solo lo cubre el barrido por `mtime` de carpeta. Es lo que ya hace la app (`delta::rescan`).
3. **Pendiente de evaluar:** sembrar el índice con una consulta de `MediaStore` (0,2 s) para que la búsqueda ya sirva mientras el recorrido completo termina en segundo plano.

## 3. Qué hace hoy CensusIndex frente a lo mejor encontrado

| Plataforma | Estado | Brecha |
|---|---|---|
| Windows | MFT + USN (medido) | ReFS sin vía rápida |
| macOS | `getattrlistbulk` implementado; FSEvents pendiente | Persistir `eventId` para recuperar cambios con la app cerrada |
| Linux | `getdents64`+`statx` paralelo | **Falta fanotify** (`watch.rs` vacío) y las vías btrfs/XFS (documentadas como aceleradores opcionales) |
| Android | Recorrido nativo + MediaStore + barrido podado | Siembra por MediaStore; sin medir en teléfono |
| Extraíbles | Recorrido completo al reconectar (implementado, probado) | NTFS/ext4 en Android dependen de que el sistema los monte |

Los números de proyectos externos **no son comparables entre sí** (distinto hardware, caché y archivos). Para decidir hace falta correr `bench` en el equipo real.

## 4. Particiones y medios extraíbles

**Regla:** CensusIndex indexa todo lo que el sistema operativo le deja leer. Una partición que el sistema no monta (por ejemplo NTFS o ext4 en muchos Android, o un volumen cifrado) no es visible para la app; leer el disco crudo requiere root/administrador y no se hace.

- Un pendrive con varias particiones llega como **varios volúmenes**; cada uno tiene su propia identidad (UUID del filesystem) y su propia política.
- Al conectar un volumen extraíble se aplica una política, global o por volumen:

| Política | Qué hace |
|---|---|
| `ask` (por defecto) | Notificación con **Indexar** (esta vez) / **Siempre** / **Nunca**. Se pregunta una sola vez por conexión; ignorarla no cambia nada |
| `always` (forzar) | Indexa el disco entero apenas se conecta |
| `never` | No lo indexa. Si ya tenía copia, queda congelada (no se borra) y sigue buscable offline |

- Excepciones por volumen mandan sobre la global. El almacenamiento interno **siempre** se indexa. Red y nube solo si se activa `include_network`.
- Un extraíble que **vuelve a conectarse** se recorre completo, sin la poda por `mtime`: en FAT/exFAT no se puede confiar en que la fecha de carpeta cambie si se editó en otro equipo.
- Código: `linux/src/volpolicy.rs` (reutilizable por Linux y macOS), `android/src/engine.rs`, `VolumePrompt.kt`. Probado con 7 tests de política y 4 de integración (pregunta, siempre, nunca, reconexión). **No probado con un pendrive real**: los eventos de conexión de Android (`StorageVolumeCallback`, disparador de `MediaStore`) están sin verificar en dispositivo.
