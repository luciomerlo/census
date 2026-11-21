# Quién escribe qué: contrato de archivos entre Census, WizEverything y SortEverything

Regla de oro: **ninguna app asume que las otras están corriendo**. Sort y Wiz pueden no
estar abiertas a la vez, pueden abrirse en cualquier orden y pueden haber quedado
cerradas a mitad de un trabajo. Todo lo que se escribe sobrevive a eso sin corromperse.

Rutas verificadas leyendo el código de cada proyecto (no de memoria).

## 1. Archivos propios de cada app

### Census
| Ruta | Qué es | Quién escribe |
|---|---|---|
| `%LOCALAPPDATA%\Census\C.wcat` | snapshot del índice (nodos + arena de nombres) | **sólo Census** |
| `%LOCALAPPDATA%\Census\C.wcat.tmp.<pid>` | temporal de escritura | sólo Census; se barre si tiene > 1 h |
| `%LOCALAPPDATA%\Census\pause.json` | pausa del trabajo de fondo | sólo Census |
| `%LOCALAPPDATA%\Census\nightly-state.json` | avance de la tanda nocturna | sólo Census |
| `%APPDATA%\Census\settings.ini` | `excluded_drives=` y la clave de Groq cifrada con DPAPI | sólo Census |

### WizEverything
| Ruta | Qué es | Quién escribe |
|---|---|---|
| `%LOCALAPPDATA%\WizeShared\content-index.sqlite3` (+ `-wal`, `-shm`) | índice de contenido compartido | **Wiz** (dueño del esquema); Sort sólo con lease |
| `%LOCALAPPDATA%\WizEverything\` | bandeja de operaciones, `audit.jsonl` | sólo Wiz |
| `%LOCALAPPDATA%\WizEverything\pause.json`, `nightly-state.json` | pausa y checkpoint | sólo Wiz |
| `%APPDATA%\WizEverything\` (o `LOCALAPPDATA` si no hay) | ajustes y cachés propias | sólo Wiz |

### SortEverything
| Ruta | Qué es | Quién escribe |
|---|---|---|
| `%APPDATA%\SortEverything\` | `settings.json`, `done.json`, journal de undo | sólo Sort |
| `%LOCALAPPDATA%\WizeShared\content-index.sqlite3` | mismo índice de contenido | Sort **sólo** si Wiz no corre y con lease |
| `%LOCALAPPDATA%\SortEverything\pause.json`, `nightly-state.json` | pausa y checkpoint | sólo Sort |

Ambas apps permiten mover el sqlite con `WIZE_SHARED_INDEX=<ruta>` (para pruebas o
perfil portable). Census no lee ni escribe ese archivo nunca.

## 2. Lo que cada uno no toca, nunca

- **Census** no abre `content-index.sqlite3`, no extrae texto, no hace OCR, no abre
  comprimidos, no mueve ni renombra archivos del usuario.
- **Wiz** no abre `C.wcat` y no indexa la MFT: pide por IPC o MCP.
- **Sort** no abre `C.wcat`, no define bits de capacidad ni campos del protocolo, y no
  migra el esquema del sqlite (si le falta una columna, trabaja en modo lectura v1).
- Nadie borra ni renombra archivos de otro proyecto.

## 3. Escrituras seguras

- Siempre `archivo.tmp.<pid>` en **el mismo directorio** y después `rename`: en NTFS un
  lector ve el archivo viejo o el nuevo, nunca la mitad.
- Census toma `LockFileEx` sobre `C.wcat.lock` mientras escribe. Los lectores no toman
  lock: validan magic, versión y checksum, y si no cuadra reindexan.
- SQLite compartido: `journal_mode=WAL`, `busy_timeout=5000`, y **un solo escritor** por
  lease con `writer_id` (token por proceso, no PID), `heartbeat` cada 30 s y TTL 300 s.
- Si el disco se llena al guardar el snapshot: se conserva el anterior y Census sigue con
  el índice en RAM.

## 4. Identidad de archivo

- **Census** calcula `vol_serial` + `frn` leyendo la MFT y los expone por IPC cuando el
  cliente anuncia la capacidad `IDENTITY` (64 bits, y en JSON como **string hexadecimal**
  para no perder precisión).
- **Sort** además puede sacarla del handle (`GetFileInformationByHandleEx` /`FileIdInfo`),
  útil para archivos que todavía no están en el índice.
- **Wiz** la usa para que renombrar no obligue a re-extraer texto.
- Sin identidad (`frn=0`) todos caen a `path + size + mtime` sin romperse.

## 5. Pausa y conflicto de recursos

- `pause.json` por app: `{paused, resume_at (epoch **ms**), reason, updated}`. Persistido:
  cerrar la app no borra la pausa.
- La pausa frena **trabajo de fondo**, nunca las consultas del usuario. Libera el lock del
  disco; al reanudar se vuelve a pedir, y si no se consigue la tarea espera.
- Actividad: **un archivo por app** en `%LOCALAPPDATA%\WizeShared\`
  (`activity-Census.json`, `activity-Wiz.json`, `activity-Sort.json`). Cada proceso escribe
  sólo el suyo, con `tmp` + `rename`, y lee los otros dos sin modificarlos. Contenido:
  `{pid, app, task, resources, roots, state, forced, heartbeat}`; un `heartbeat` de más de
  120 s no bloquea. Si el disco no se puede identificar (`disk:?`) o es nube, no se inventa
  lock: se avisa por raíces solapadas.
- La unidad de exclusión es el **disco físico** (`disk:<DeviceNumber>`, vía
  `IOCTL_STORAGE_GET_DEVICE_NUMBER`), no la letra. Dos particiones del mismo disco no
  corren tareas pesadas a la vez. Para nube y red: `cloud:<provider>`.
- Conflicto: aviso visible + notificación si está minimizada, con **Cancelar / Esperar /
  Forzar**; forzar pide confirmación y queda en el log.
- **Wiz tiene prioridad** si el usuario fuerza las dos: Census pausa en el próximo punto
  seguro y avisa «pausado por WizEverything».

## 6. Nada arranca solo

- Ninguna tarea pesada se dispara por horario ni al abrir la app.
- Las tandas nocturnas las arma el usuario; cada tarea registra `forced: true`, `auto: false`.
- Google Drive streaming: metadata solamente (sin abrir contenido, sin hash, sin OCR, sin
  comprimidos, sin copiar entre volúmenes). Forzar no saltea esa regla.
