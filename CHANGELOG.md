# Changelog

Todos los cambios relevantes de Census. El formato sigue
[Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/) y el proyecto usa
[Versionado Semántico](https://semver.org/lang/es/).

## [0.1.1] — 2026-11-21

Ronda de revisión de bugs y dependencias. Sin cambios de formato ni de protocolo.

### Seguridad
- **`mft` 0.5.3 → 0.10**: la versión vieja arrastraba **`lru` 0.4.3, con un *use after
  free*** (RUSTSEC-2021-0130), además de `rand_core` 0.5.1, `atty`, `term`, `simplelog` 0.7
  y `encoding`. Con la actualización desaparecen todos esos avisos.
- **`chrono` → `jiff`** (ya venía con `mft`): `chrono` figura como *unmaintained* en RUSTSEC.
- **`ureq` con `native-tls`** en vez de `rustls`: usa schannel de Windows y saca `ring`,
  `rustls`, `webpki-roots` e `icu` del árbol.
- `tools/audit-advisories.py`: revisa el lock contra la base de RUSTSEC sin `cargo audit`
  (que no se puede instalar en este entorno). Queda **1 aviso**, informativo:
  `ttf-parser` *unmaintained* vía `egui`.
- CI corre `rustsec/audit-check` en cada push.

### Corregido
- **Panics con índice vacío**: `name_string(0)`, `is_drive(0)` y `dir_path(0)` indexaban
  `nodes[0]` sin comprobar. Ahora usan `get()` y devuelven vacío/false; `name_utf16`
  también tolera offsets corruptos.
- **Test flaky**: las cuotas de Groq viven en un `static` y los tests en paralelo se
  pisaban; los que dependen del orden de modelos ahora se serializan y limpian la tabla.
- `--compat` registraba sólo la primera clase libre, así que
  `EVERYTHING_TASKBAR_NOTIFICATION` nunca existía. Ahora hay una ventana por clase.
- `clippy -D warnings` limpio y `cargo fmt` aplicado.

### Agregado
- `tests/robustness.rs` y `crates/census-ipc/tests/fuzz_lite.rs`: 20 000 buffers al azar
  contra los parsers del IPC, truncado byte por byte, 500 snapshots de basura, cabeceras
  con conteos `u32::MAX`, 5 000 consultas y 5 000 rutas al azar.
- Suite con `-C overflow-checks=on` (99 tests) y workflow de CI (fmt, clippy, tests,
  release, auditoría).
- `census probe`: diagnóstico del IPC (ventanas, capacidades, flags de la respuesta).
- Mensaje de capacidades `WM_USER + 402` y `flags` de la respuesta con **sólo los campos
  que vinieron** (`run_count` nunca se anuncia).

## [0.2.0] - 2026-11-21

### Rendimiento

- **Snapshot `C.wcat` v2**: guarda los índices derivados (`folded`, `dir_bytes`,
  `hijos`, `por_nombre`, `euler`). Cargar `C:` (1,64 M de nodos) pasó de ~0,9 s
  a **240 ms**.
- **Fase de carpetas por subárbol** (`euler`/`tin`/`tout`): buscar un término en
  las carpetas ya no recorre el índice caminando padres.
- **Orden por nombre precalculado** y orden en paralelo con fusión.
- **Búsqueda incremental** al tipear: 3–7 ms por letra.
- Checksum por palabras de 8 bytes, independiente de cómo se parta la entrada.

### Cambios

- Binario afinado: `strip` y `panic=abort` → **5,28 MB**.
- Snapshot de `C:` con los índices completos: ~298 MB.

## [0.1.0] — 2026-11-21

Primera versión que se puede usar de punta a punta.

### Agregado
- **Motor**: lectura de la MFT de un volumen NTFS con `FSCTL_GET_NTFS_VOLUME_DATA` y los
  *data runs* de `$DATA`, en paralelo. 1.868.924 registros en 2.834 ms (543k nodos/s).
- **Índice**: nodos de 48 B en arrays planos y nombres en una arena UTF-16; 156 MB para
  todo un volumen de 1,6 M de entradas.
- **Consultas**: `""`, `path:"X"`, `file:`, `folder:`, `ext:`, `size:`, `dm:`, `rc:>=` y
  términos por subcadena, con orden por nombre y paginación.
- **Multiunidad**: `Index::merge` con una raíz por volumen; `root_for`, `find_dir`,
  `dir_path` y `full_path` resuelven contra la unidad correcta.
- **Unidades**: indexa **todas** las disponibles; locales NTFS por MFT, y red, nube
  (Google Drive, OneDrive, …) o exFAT por recorrido de directorios, con caída al
  recorrido si la MFT falla. Detecta el tipo como WizTree y lo muestra en la ventana.
- **Snapshot `C.wcat`**: formato propio versionado con checksum FNV-1a, escritura atómica
  (`.tmp` + rename), validación de límites al leer y reutilización si tiene menos de 30 s.
- **IPC**: servidor `QUERY2W` (clase `CENSUS_IPC`) y cliente del mismo protocolo, byte a
  byte, incluido el detalle de que el offset de cada ítem apunta al prefijo de largo del
  nombre (así lo lee WizEverything).
- **MCP**: `census mcp` por stdio con `search`, `count`, `largest`, `recent` y `stats`.
- **Lenguaje natural (Groq)**: `census ask` y el botón `IA` de la ventana; modelos
  rankeados, failover por cuota (`x-ratelimit-*`), tool-calling sobre las herramientas de
  Census y clave cifrada con DPAPI (`census key`).
- **Ventana**: tres temas `dark`/`light`/`contrast` (las claves de WizEverything) con
  contraste verificado por test, tipografía Segoe UI + Segoe UI Symbol, tabla
  virtualizada, `Enter` abre, `Ctrl+C` copia la ruta, selector de unidades.
- **Icono**: lupa ámbar con un índice de filas dentro, homenaje a la de Everything;
  `assets/icon.svg`, `icon.ico` (16/32/48 en DIB + 256 en PNG) y script `tools/make-icon.ps1`.

### Cambiado
- La ventana oculta la consola del binario al abrirse (`census gui`).
- Las capturas se toman por PID y clase de ventana, no por `MainWindowHandle`.

### Corregido
- El `.ico` guardaba los cuatro tamaños como PNG: Windows sólo acepta PNG de forma
  confiable en 256×256, así que el Explorador podía mostrar el icono genérico. Ahora
  16/32/48 van en DIB clásico y sólo 256 en PNG.
- A 16 px el anillo y el mango finos desaparecían: trazos más gruesos y lente más clara
  que el anillo en los tamaños chicos.
- El tema oscuro era poco legible: texto y secundario ahora pasan WCAG AA/AAA.
- `census mcp` mandaba las herramientas sin la clave `tools` al asistente de Groq.
- `scan_dir` rotulaba la raíz como `C:` aunque indexara otra unidad.
- El test de la ventana quedó desactualizado al agregar campos al estado.

### Créditos
- Homenaje a [Everything](https://www.voidtools.com/) de voidtools, el programa que más
  recomendé en mi vida.
- Parseo de MFT sobre [`omerbenamram/mft`](https://github.com/omerbenamram/mft) y
  [`omerbenamram/ntfs`](https://github.com/omerbenamram/ntfs) (MIT).

[Unreleased]: https://github.com/luciomerlo/census/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/luciomerlo/census/releases/tag/v0.1.1
[0.1.0]: https://github.com/luciomerlo/census/releases/tag/v0.1.0
