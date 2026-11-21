# Changelog

Todos los cambios relevantes de Census. El formato sigue
[Keep a Changelog](https://keepachangelog.com/es-ES/1.1.0/) y el proyecto usa
[Versionado Semántico](https://semver.org/lang/es/).

## [Unreleased]

### Por hacer
- Delta por USN Journal: hoy el snapshot se reusa con una ventana de 30 s (`--max-age`).
- Shim `Everything64.dll` para SortEverything (el crate existe, los exports no).
- Ordenar por tamaño, fecha y ruta además de por nombre.

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

[Unreleased]: https://github.com/luciomerlo/census/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/luciomerlo/census/releases/tag/v0.1.0
