# Census para Android

Estado: núcleo Rust probado en este entorno; `.so` y APK debug compilan; **sin probar en un teléfono**.
Código en [`android/`](../android/README.md). Contrato de API/identidad: [`MINIMO.md`](MINIMO.md), [`ATLAS.md`](ATLAS.md) §9–10.

## 1. ¿Ya existe algo así?

Búsqueda hecha el 2026-11-26. Ninguno cubre el objetivo completo (indexar el teléfono **y** buscarlo junto con los discos de las PCs, prendidas o apagadas).

| Producto | Qué hace | Por qué no alcanza |
|---|---|---|
| [Everything](https://voidtools.com/forum/viewtopic.php?p=9182) (voidtools) | No tiene versión Android; estaba en su lista de pendientes en 2021 | — |
| [Android Everything](https://raw.githubusercontent.com/luli395/android_everything/master/docs/USER_GUIDE.md) (luli395) | App de **Windows**: lee el teléfono por ADB y arma un índice SQLite/FTS5 | Hay que tener el cable/ADB y reconstruye el índice completo cada vez; no corre en el teléfono ni se actualiza solo |
| Search Everything (Android) | Índice por nombre, modelado sobre Everything ([XDA](https://www.xda-developers.com/supercharge-search-with-search-everything/)) | Marcado como discontinuado en [AlternativeTo](https://alternativeto.net/software/search-everything) |
| [EverythingClient](https://voidtools.com/forum/viewtopic.php?p=80863) | Frontend Android del servidor HTTP de Everything: busca **en la PC** desde el teléfono | Es al revés: no indexa el teléfono |
| Fossify File Manager, Files by Google | Búsqueda dentro del gestor de archivos | Solo el teléfono, sin catálogo ni PCs |
| [VVV](https://linuxlinks.com/vvv-catalog-content-removable-volumes), [Catfish](https://github.com/crispstrobe/catfish), [Librer](https://alternativeto.net/software/librer/about), [Cathy](https://github.com/binsento42/Cathy) | Catálogos de discos **offline** (Windows/macOS/Linux) | Excelentes para discos desconectados, pero escritorio, índice manual, sin Android ni actualización en segundo plano |

**Conclusión:** no hay un producto que combine (a) índice en el teléfono, (b) actualización continua en segundo plano y (c) catálogo multi-equipo con copias offline. Census ya tiene (c) (Atlas, `cloud_id`/`volume_id`, offline opt-in) y el núcleo `.cns` de Linux; Android aporta (a) y (b).

## 2. Decisión de diseño

Android es el kernel Linux: se reutiliza el crate `linux/` (walker, índice, consultas, snapshot `.cns`, API HTTP v1). El teléfono produce el **mismo `.cns` y la misma API** que una PC Linux, así que entra al catálogo sin formato nuevo.

```
Kotlin (decide cuándo/dónde)                Rust (decide cómo, con mínimo trabajo)
 MediaChangeJob ─ MediaStore ─┐
 SweepWorker (6 h) ───────────┼─► nativeDirty / nativeSweep ─► delta::rescan ─► index.cns
 UI "Indexar ahora" ──────────┘                                   │
                                                  nativeServe (HTTP v1, token, Tailscale)
```

Cambios en `linux/` (mínimos): `scan::sys` pasa a `pub`; `lstat` en Android usa `libc::lstat` (no `statx` por syscall: el filtro seccomp de apps no lo garantiza antes de Android 11 y el almacenamiento compartido no da `btime`); `http::serve_live` sirve el índice vigente sin reiniciar.

## 3. Método más rápido para detectar cambios en segundo plano

No hay un único método; hay un orden de costo. La app usa los tres y todos alimentan la misma primitiva (*carpetas sucias*).

| # | Método | Costo | Qué detecta | Qué no |
|---|---|---|---|---|
| 1 | **`MediaStore` + `GENERATION_MODIFIED`** (API 30+) disparado por `JobScheduler` con `TriggerContentUri` | Proporcional a lo que cambió. El sistema despierta la app; no hay servicio ni sondeo | Archivos nuevos o modificados, con su ruta ([AOSP: generaciones, más robustas que `DATE_MODIFIED`](https://source.android.com/docs/core/media/media-provider)) | Borrados (no deja lápida); archivos que `MediaProvider` no registró |
| 2 | **Barrido podado por `mtime` de carpeta** | Un `stat` por carpeta; `getdents`+`statx` solo donde cambió | Altas, bajas y renombres en cualquier carpeta | Reescritura *in situ* de un archivo existente (no cambia el `mtime` de la carpeta) |
| 3 | `inotify` (`FileObserver`) | Un watch por carpeta | Todo, al instante | **No es viable recursivo**: el límite de watches por usuario es bajo en Android ([Syncthing lo reporta](https://forum.syncthing.net/t/failed-to-set-up-inotify-handler-please-increase-inotify-limits/11924)) y la cola puede desbordar. No se usa salvo, eventualmente, en 3–5 carpetas calientes |

**Borrados:** `MediaStore` no avisa qué se borró. Se detectan por aritmética exacta: `filas_antes + filas_nuevas − filas_ahora > 0`. Si da positivo, se agenda un barrido podado ~2 min después (método 2), agrupando ráfagas.

**Por qué es lo más rápido:** el caso común (sacás una foto, bajás un PDF) lo resuelve el método 1 con una consulta indexada y relistando **una** carpeta; el índice vigente se reemplaza atómicamente. El método 2 corre solo cuando hubo borrados o cada 6 h con batería no baja.

### Medido (esta máquina, Linux x86_64, árbol sintético, caché caliente — **no es un teléfono**)

`cargo run --release --example bench -- 20000 10` → 20 051 carpetas, 220 051 nodos:

| Operación | Tiempo | Carpetas listadas | `stat` de carpetas |
|---|---|---|---|
| Completo | 390 ms | 20 051 | — |
| Barrido, sin cambios | 82 ms | 0 | 20 050 |
| Barrido, 1 carpeta cambió | 80 ms | 1 | 20 050 |
| Dirty, 1 carpeta declarada | 55 ms | 1 | 2 |

Lo que sí transfiere al teléfono es la proporción de trabajo (0–1 carpetas listadas en vez de 20 051). Los milisegundos **no**: la FUSE de `/storage/emulated` y el UFS/eMMC cambian el costo del `stat`. Hay que medir en el equipo real.

## 4. Identidad y offline

- Interno: `volume_id = hash("emulated:<pc_id>" || "emulated" || 0)`. No expone serie de hardware; el tamaño se fija en 0 para que no cambie al llenarse. `pc_id` lo genera la app una vez (se pierde al borrar datos de la app).
- SD/USB: `volume_id = hash(UUID del filesystem || "sd" || capacidad)`. La ruta (`/storage/XXXX-XXXX`) es presentación; la misma SD en otro montaje no duplica. Dos SD con la misma etiqueta nunca se mezclan. Sin UUID: `sin_identidad`, no federable.
- Un volumen que ya no está montado **conserva su última copia** en el índice (probado): se puede buscar offline. El estado `offline` lo asigna el catálogo, no el teléfono.
- Teléfono apagado: la PC conserva el último `.cns` publicado (Atlas §"Offline"). Mientras el teléfono está prendido, la PC puede consultar `GET /v1/search` por Tailscale (activar *Compartir índice*; exige token).

## 4b. Nombre del dispositivo e IA

- **Nombre:** por defecto el que el sistema le da al teléfono (`Settings.Global.DEVICE_NAME`, o el modelo). Se puede reemplazar por uno propio **sin espacios** (letras, números, `-`, `_`, `.`; máx. 63); si lleva espacios la app rechaza y sugiere la versión con guiones. Vacío vuelve al del sistema. Lógica común en `linux/src/devname.rs`, ya cableada en la config de Linux para reutilizarla en macOS. El nombre nuevo llega al catálogo en el próximo reescaneo.
- **Índice:** solo nombres, carpetas y metadatos de tamaño/fecha; nunca contenido.
- **IA:** endpoint configurable (Groq por defecto; cualquier API OpenAI-compatible; IA local), auto-selección de modelo, lista, nombre forzado y fallback. Contrato en [`IA-ENDPOINT.md`](IA-ENDPOINT.md). En la pantalla principal, el interruptor *IA* manda la frase (3 palabras o más) al modelo; sin él, la búsqueda por nombre o ruta es instantánea y local.

## 5. Permisos y límites

- `MANAGE_EXTERNAL_STORAGE` (acceso a todos los archivos): obligatorio para ver más que multimedia desde Android 11. Google Play lo restringe; se distribuye como APK directo. La app es **solo lectura**.
- `Android/data` y `Android/obb` se excluyen (inaccesibles desde Android 11) junto con `.thumbnails` y papeleras.
- Android 14+: el servidor usa un servicio en primer plano `specialUse` con notificación; los fabricantes con ahorro agresivo de batería pueden matar el proceso. Los trabajos de `JobScheduler`/`WorkManager` sobreviven a eso mejor que cualquier servicio propio.
- El método 1 solo ve lo que `MediaProvider` indexó. Archivos creados por procesos que no pasan por la capa FUSE del sistema los recoge el barrido.

## 6. Verificado y pendiente

Verificado aquí: 18 tests del núcleo (incluye equivalencia incremental = escaneo completo, preorden válido, volumen ausente conservado, SD remontada sin duplicar, exclusiones); 59 tests de `linux/` (42 previos + 17 de IA y nombre de dispositivo); `clippy -D warnings` limpio; `cargo check --target aarch64-linux-android`; `libcensus_android.so` (arm64, 2,7 MB con TLS) enlazada con NDK 27 y con los 15 símbolos JNI exportados; APK debug generado con Gradle con la `.so` adentro (Kotlin compila, manifiesto válido).

**No verificado:** ejecución en un teléfono real; consultas a `MediaStore` (`MediaDelta.kt`); comportamiento de `JobScheduler`/`TriggerContentUri` con el proceso muerto; el servicio HTTP; la IA contra Groq real (solo se probó contra un servidor falso); la pantalla de ajustes en un teléfono; tiempos en el teléfono.

Pendiente: subir el `.cns` al hub de Atlas cuando exista (`atlas push`); `inotify` en carpetas calientes; i18n; `Marca` propia (`l1:` de Linux por ahora); persistir `ServeService` tras reinicio.
