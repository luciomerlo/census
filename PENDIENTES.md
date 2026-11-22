# Pendientes — 2026-11-22

Estado al cerrar la ronda de coordinación entre los chats de Census (Windows,
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
   el manejo en `hot.rs` / `census watch` quedó sin aplicar (skipped).
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
7. **`cloud_id` para Google Drive** (§9): hoy `census drives` ve
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

- Avanzando en `linux/` (`plataforma.rs`, `volumes.rs`, `cloud.rs`, `scan.rs`).
- Pendiente suyo: `l1:<generation>:<cambio_n>`, `btime` con `STATX_BTIME`,
  `DiskHwId` por udev (`ID_SERIAL_SHORT`, `ID_WWN_WITH_EXTENSION`), listas de
  red/nube, y números medidos de `getdents64` (referencia Windows: 335k/s).

## Wiz (`c5eb0379`) y Sort (`c78e8f8c`)

- Avisados de: `cloud_id`, offline opt-in, `DiskHwId`/`volume_id`.
- Esperando respuesta: si guardan config/planes/caché **por letra** para migrar
  a `volume_id`; dónde vive el tildado de volúmenes (recomendación: Census);
  si Sort verifica el `volume_id` destino antes de mover.

## Números de referencia (para comparar)

- Indexado MFT: 1.641.689 nodos en 4,6-8,7 s (335k/s).
- Carga de snapshot: 197-216 ms, RAM 161 MB.
- Consultas: `(todo)` 10,3 ms · `mp3` 17 ms · `acpi` 18 ms · `cursor` ~20 ms ·
  `everything` 32-36 ms · `node_modules` 39 ms.
- Tipeo incremental: 3-7 ms por letra.
- Tests: 127 verdes. Clippy `-D warnings` en 0.

## Recordatorios operativos

- `cargo test --workspace` (la raíz sola no corre `census-ipc`).
- `taskkill //F //IM census.exe` elevado si el rebuild falla con "Acceso
  denegado (os error 5)".
- No commitear `mac/src/types.rs` ni otros archivos en curso de otro chat.
- Regla dura: Census lee; no borra, mueve, renombra ni cambia atributos/fechas.
