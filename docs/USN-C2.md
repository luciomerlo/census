# C2 — Actualización en caliente con el USN Journal

Estado: **en curso**. Este archivo es también el punto de retome: si la sesión
se corta, leer esto y seguir por el primer casillero sin marcar.

## Por qué

El índice es una foto: lo que se crea después no aparece y lo que se borra
sigue estando. El journal de NTFS (`$UsnJrnl`) avisa exactamente qué cambió,
sin releer la MFT.

## Diseño elegido

El índice principal queda **inmutable** y los cambios van a una capa aparte:

- `overlay.nuevos`: archivos y carpetas creados (con su FRN y el FRN del padre).
- `overlay.renombrados`: FRN → (nombre nuevo, FRN del padre nuevo).
- `overlay.borrados`: FRNs que ya no existen.
- `overlay.watermark`: `journal_id` + `next_usn` aplicados.

Ventaja: aplicar 100 cambios cuesta microsegundos y no hay que reconstruir
subárboles ni el orden alfabético de 1,6 M de nodos. Cuando el overlay supera
un tope (~50.000 entradas) se compacta: se reindexa y se vacía.

Al consultar: se escanea el índice principal (descartando los borrados) y
después el overlay de nuevos (que es chico), con la ruta armada desde el padre.

## Pasos

- [x] `src/usn.rs`: abrir el journal, leer registros y parsear `USN_RECORD_V2`
      (con tests sobre buffers sintéticos).
- [x] `src/overlay.rs`: estructura, `aplicar(cambios)`, resolución de rutas y
      matching (misma semántica que el índice: nombre primero, carpeta después).
- [x] Integrar en `query`: descartar borrados, sumar nuevos, respetar filtros.
- [x] `next_usn` + `journal_id` en el snapshot (formato v3); replay al arrancar.
- [x] Journal reiniciado o dado vuelta → reindexar el volumen (avisar en
      `activity-Census.json`).
- [x] Hilo de fondo en `serve`/`gui`: consultar cada 2 s y aplicar.
- [ ] Compactación del overlay.
- [x] Medir: latencia de aparición de un archivo nuevo y costo de aplicar lotes.

## Casos límite

| Caso | Qué se hace |
|---|---|
| `FILE_CREATE` | se agrega a `nuevos` |
| `FILE_DELETE` | se agrega el FRN a `borrados` |
| `RENAME_OLD_NAME` / `RENAME_NEW_NAME` | se guarda el nombre nuevo y el padre nuevo |
| Varios registros del mismo FRN | se aplican en orden; gana el último |
| Cambio de un FRN que no está en el índice | se ignora |
| `journal_id` distinto o `first_usn` > watermark | reindexar el volumen |
| Unidad de nube | no tiene journal: no aplica |

## Constantes

- `FSCTL_QUERY_USN_JOURNAL` = `0x0009_00F4`
- `FSCTL_READ_USN_JOURNAL` = `0x0009_00BB`
- Motivos: `FILE_CREATE` 0x100, `FILE_DELETE` 0x200, `RENAME_OLD_NAME` 0x1000,
  `RENAME_NEW_NAME` 0x2000, `CLOSE` 0x8000_0000.
- `USN_RECORD_V2`: cabecera de 60 bytes, nombre UTF-16 en `FileNameOffset`.

## Verificado el 2026-11-22

```
vigilando C: (Ctrl+C para cortar)
aplicados 278 cambios (en el overlay: 534)
  "pruebac2" -> 1 resultados ["C:\Users\Lucio\pruebac2-caliente.txt"]
aplicados 18 cambios (en el overlay: 572)
  "pruebac2" -> 0 resultados []
```

Dos detalles que costaron encontrar:

1. El FRN del journal trae la secuencia en los 16 bits altos y el índice guarda
   sólo el número de registro: hay que enmascarar (`FRN_MASK`).
2. Windows reusa números de registro, así que un archivo creado puede heredar
   el FRN de uno borrado. Por eso, además de `borrados`, el overlay lleva
   `pisados`: FRN que el journal tocó y que ya estaban en la foto.
