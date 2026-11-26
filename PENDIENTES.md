# Pendientes — 2026-11-22 (actualizado tras mmap + ARM + contrato MINIMO)

## Hecho en esta tanda
- `f6f0c06` snapshot v4 con secciones alineadas 8 B + arenas por mmap
  (`src/mmap.rs`, memmap2): **carga 197 → 155 ms**, `names`/`folded` sin copiar.
  `CENSUS_MMAP=0` fuerza el camino de copia. v3 se sigue leyendo.
- `f6f0c06` `DiskHwId`/`volume_id`/`cloud_id` en `topology.rs`/`volumes.rs`:
  serial+vendor+modelo+capacidad, `serial_ambiguo`, `sin_identidad`, parseo de
  etiqueta de Google Drive. `censusindex drives` ya muestra la topología real.
- `0a9265e` `src/atlas.rs`: overrides (excluir/incluir/unir/separar) y selección
  offline tildada, con 8 tests. Falta el subcomando CLI.
- `3a2de0b` CI ARM64 nativo (job `arm64` en `ci.yml`) + `tools/build-arm64.ps1`.
- `docs/MINIMO.md` (`e546c8c`, `8e98631`): contrato v1 de 5 comandos + 6
  endpoints HTTP, aceptado por los tres chats.
- 154 tests verdes, clippy `-D warnings` en 0.

## UI: emular WizEverything (pedido del usuario, en este orden)

1. **Sin botones de orden.** El orden se cambia clickeando el encabezado de la
   columna (Nombre / Carpeta / Tamaño / Fecha), con flecha ↑↓ en la activa.
   Borrar el botón `self.sort.label()` de la barra.
2. **Unidades a la izquierda, clickeables.** Panel lateral (no ventana modal) con
   la lista de unidades; click selecciona esa unidad y filtra los resultados
   (incluye "Todas"). Las nube se tildan desde ahí y **deben indexarse al
   tildarlas**, sin reiniciar (hoy `roster()` nunca las prende porque
   `is_indexable()` excluye nube: bug confirmado, arreglar con `included_drives`).
3. **Icono de lupa inclinado a la derecha** (hoy el asa apunta a la izquierda).
4. Layout de referencia: Wiz `src/app.rs` (panel de árbol + tabla + detalle),
   misma disposición, no el mismo código.

## Checkout oficial
- El repo vive en **C:/Users/Lucio/Desktop/Proyectos/censusindex** (el scratch era
  temporal). Mismo origin, master a nivel. Los commits previos siguen valiendo.

- **`volumes --json` y `censusindex atlas ...`**: el subagente había reescrito
  `main.rs` y la CLI quedaba colgada; se volvió a HEAD. Hay que re-agregar sólo
  los subcomandos, sin tocar el parseo existente (copia de la versión del
  subagente en `/tmp/main_delegate.rs`).
- **`serve --http`**: 6 endpoints `/v1/*` de MINIMO. En Windows no existe HTTP
  hoy (sólo named pipe).
- **Cross-build ARM64 local falla**: falta "MSVC v143 - ARM64 build tools"
  (`link.exe` sin operandos). El CI usa runner nativo `windows-11-arm`.
- **`DiskHwId` sin elevación** devuelve "modelo desconocido": probar elevado y
  verificar `serial`; si el driver NVMe exige `GENERIC_READ`, documentarlo.


Estado al cerrar la ronda de coordinación entre los chats de CensusIndex (Windows,
Mac, Linux). Último commit en master: `deb764d`.

## Acuerdo cerrado (no volver a discutir)

- `docs/PLATAFORMAS.md` — contrato entre Windows, macOS y Linux: crates
  separados (`src/`, `mac/`, `linux/`), fuente por lotes, `Entrada`/`Cambio`
  con nombres en bytes + `encoding`, `FileKey` vs `FileId`, `Marca` opaca
  (`w1:`/`m1:`/`l1:`), tabla de plegado común, `export --wcat`.
- `docs/ATLAS.md` §9 — nube compartida: `cloud_id = hash(provider||account||unidad)`,
  colapso por `cloud_id` entre apps y PCs, `posible_duplicado` sin fusión
  silenciosa, offline como opt-in explícito con tildado, overrides manuales.
- `docs/ATLAS.md` §10 — identidad por hardware: `DiskHwId` / `volume_id` /
  `mount`. La letra de unidad **es sólo presentación**.

## Windows (este chat)

1. **mmap real de arenas** — `Arena<T>` está listo (commit `d5f9c9a`) pero se
   sigue copiando a `Vec`. Falta: formato de snapshot v4 con secciones alineadas
   a 8 B, `CreateFileMappingW` + `MapViewOfFile` acotado a `names` + `folded`.
   Objetivo: carga 197 ms → 10-20 ms. Cuidado: la única reinterpretación LE
   alineada permitida está acá.
2. **`Latido::Compactar`** — `Overlay::compactar_con_tope()`,
   `vaciar_conservando_watermark()` y `activity::pedir_reindexado` existen, pero
   el manejo en `hot.rs` / `censusindex watch` quedó sin aplicar (skipped).
3. **`topology.rs` no detecta discos físicos** en esta máquina ("sin discos
   fisicos visibles", incluso elevado). Sospecha: apertura `\\.\X:` con
   `FILE_READ_ATTRIBUTES` o el `IOCTL_STORAGE_GET_DEVICE_NUMBER`. Validar con
   discos USB conectados.
4. **Columnas ordenables** en la GUI (clic en encabezado con ↑↓) —
   `tools/_hdr.txt` escrito, no aplicado.
5. **i18n es/en** conectado a la GUI — `src/i18n.rs` existe, no aplicado.
6. **Implementar `DiskHwId`** en `volumes.rs` / `topology.rs` según §10:
   serial + vendor + model + capacidad por `IOCTL_STORAGE_QUERY_PROPERTY`,
   fallback GUID de disco, y `\\?\Volume{GUID}\` por
   `GetVolumeNameForVolumeMountPointW`. Detección de `serial_ambiguo`.
7. **`cloud_id` para Google Drive** (§9): hoy `censusindex drives` ve
   `G:`/`H:` como "nube FAT32 virtual" con etiqueta de cuenta. Falta parsear
   proveedor + cuenta + unidad desde la etiqueta y emitir `cloud_id`.
8. **Overrides de Atlas** (`atlas excluir|incluir|unir|separar|estado`,
   `atlas offline tildar/destildar`) — esqueleto en `overrides.json`, sin
   implementar. Es lo que pide el usuario para evitar duplicados cuando la
   detección falla.
9. **Selección de volúmenes remotos** `(pc_id, volume_id)` para consultas
   federadas, con offline oculto por defecto.
10. Verificar el primer `.wcat`/`.cns` que exporten Mac y Linux abriéndolo con
    el core Windows (nodos, arena, plegado con acento).

## Mac (`c3aa498c`)

- Avanzando en `mac/` (fuente `getattrlistbulk`, vigilante FSEvents, `.cnx`).
  Tiene cambios en curso en `mac/src/types.rs` — no tocar.
- Pendiente suyo: `cloud_id` para `~/Library/CloudStorage/.../Shared drives/`,
  `DiskHwId` con `DiskUUID`/`VolumeUUID`, dos ejemplos de `cloud_id` del mismo
  drive por dos programas.
- Pasarme ruta + comando del primer `export --wcat` para verificación cruzada.

## Linux (`c9725d6d`)

- `linux/` compila standalone (`[workspace]` propio) y tiene **35 tests verdes** en
  Windows: contrato (`plataforma.rs`), índice de 64 B/arenas/preorden
  (`index.rs`), `mountinfo`/tipos de volumen (`volumes.rs`), nube
  (`cloud.rs`), configuración (`config.rs`) y primitivas de `DiskHwId`
  (`diskid.rs`).
- `diskid.rs` está escrito y probado, pero **falta integrarlo** en `Mount`,
  `Root` y el snapshot: hoy `volume_id` sigue usando el hash viejo. Al
  integrarlo hay que subir la versión de `.cns` (todavía no hay ningún archivo
  guardado, así que es el momento).
- Pendiente en [`linux/PENDIENTE.md`](linux/PENDIENTE.md): `scan.rs`
  (`getdents64`+`statx`), `.cns`, consultas, catálogo multi-PC, Tailscale,
  HTTP/MCP, `watch` y `main` completo.
- Sin Linux en esta máquina (WSL pide reboot / `HCS_E_SERVICE_NOT_AVAILABLE`):
  no hay números medidos de `getdents64` ni de carga de `.cns`. No se inventan;
  referencia Windows 335k/s.

## Wiz (`c5eb0379`) y Sort (`c78e8f8c`)

- Avisados de: `cloud_id`, offline opt-in, `DiskHwId`/`volume_id`.
- Esperando respuesta: si guardan config/planes/caché **por letra** para migrar
  a `volume_id`; dónde vive el tildado de volúmenes (recomendación: CensusIndex);
  si Sort verifica el `volume_id` destino antes de mover.

## Números de referencia (para comparar)

- Indexado MFT: 1.641.689 nodos en 4,6-8,7 s (335k/s).
- Carga de snapshot: 197-216 ms, RAM 161 MB.
- Consultas: `(todo)` 10,3 ms · `mp3` 17 ms · `acpi` 18 ms · `cursor` ~20 ms ·
  `everything` 32-36 ms · `node_modules` 39 ms.
- Tipeo incremental: 3-7 ms por letra.
- Tests: 127 verdes. Clippy `-D warnings` en 0.

## Recordatorios operativos

- `cargo test --workspace` (la raíz sola no corre `censusindex-ipc`).
- `taskkill //F //IM censusindex.exe` elevado si el rebuild falla con "Acceso
  denegado (os error 5)".
- No commitear `mac/src/types.rs` ni otros archivos en curso de otro chat.
- Regla dura: CensusIndex lee; no borra, mueve, renombra ni cambia atributos/fechas.
