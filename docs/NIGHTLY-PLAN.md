# Plan de tandas nocturnas

> El reparto de funciones y los limites de cada app viven en
> `C:/Users/Lucio/Downloads/WizEverything/docs/APP-ROLES.md` (fuente unica, la mantiene
> WizEverything). Este documento solo cubre el calendario, los recursos y la pausa.


Noches de **8 h** (22:00–06:00, hora local). Cada tanda deja `nightly-state.json` con el
paso exacto para retomar la noche siguiente.

**Nada arranca solo.** Ninguna tarea se dispara por horario: el usuario arma la tanda y la
fuerza. Toda tarea registra `forced: true`, `auto: false`. Si el usuario no la fuerza, no
corre, por más que sea la noche asignada.

## Regla de exclusión

La unidad de exclusión es el **disco físico**, no la letra de unidad. Dos particiones del
mismo disco no pueden correr tareas pesadas a la vez. Para nube y red se usa
`cloud:<proveedor>`. Una sola tarea pesada por disco; las consultas del usuario siguen
pasando siempre.

Antes de empezar y durante la tarea se consulta
`%LOCALAPPDATA%\WizeShared\activity.json`:

```json
{"pid": 1234, "app": "Census|Wiz|Sort", "task": "C2",
 "resources": ["disk:0", "cloud:google"], "roots": ["C:\\"],
 "state": "running|paused", "forced": true, "heartbeat": 1760000000000}
```

Si hay conflicto: aviso en la app, notificación de Windows si está minimizada, y opciones
**Cancelar / Esperar / Forzar** (forzar pide confirmación y queda en el log). Si el usuario
fuerza Wiz y Census a la vez, **Wiz gana**: Census pausa en el próximo punto seguro y avisa
«pausado por WizEverything».

## Tareas

| Noche | Bloque | App | Tarea | Recursos | Raíces | Duración | Pausa | Forzada |
|---|---|---|---|---|---|---|---|---|
| 1 | 1 | Census | **C1** identidad por IPC (`vol_serial`+`frn`, string hex) | `disk:0` | — | 3,5 h | sí | sí |
| 1 | 1 | Wiz | **W-P0** pausa + checkpoint (`pause.json`, `nightly-state.json`) | `cpu` | — | 1,5 h | sí | sí |
| 1 | 2 | Wiz | **W-N1** contenido v2 + lease (`vol_serial`/`frn`, `source`) | `disk:0`, sqlite | `C:\Users` | 3,5 h | sí | sí |
| 1 | 2 | Sort | **S-1** cliente `census.rs` por `WM_COPYDATA` | `cpu` | — | 1,5 h | sí | sí |
| 2 | 1 | Census | **C2a** delta USN: watermark y journal id por volumen | `disk:0` | `C:\` | 4 h | sí | sí |
| 2 | 2 | Sort | **S-2** pausa + checkpoint propios | `cpu` | — | 3 h | sí | sí |
| 3 | 1 | Census | **C2b** USN: replay, compactación y journal corrupto | `disk:0` | `C:\` | 3,5 h | sí | sí |
| 3 | 2 | Wiz | **W-N3** worker de `dupdirs` + presupuesto de OCR | `disk:0`, `cpu` | `C:\Users` | 3,5 h | sí | sí |
| 4 | 1 | Wiz | **W-N2** comprimidos (`archive_docs`, `archive!member`) | `disk:0`, sqlite | `C:\Users` | 4 h | sí | sí |
| 4 | 2 | Sort | **S-3a** OCR de Windows: PDF → bitmap → OCR | `gpu:0`, `cpu` | — | 3,5 h | sí | sí |
| 5 | 1 | Census | **C3** robustez de `serve`: dos instancias, cliente colgado, sin elevación | `disk:0` | — | 2 h | sí | sí |
| 5 | 1 | Census | **C4** documentación del contrato + `census probe` extendido | `cpu` | — | 1,5 h | sí | sí |
| 5 | 2 | Sort | **S-3b** OCR: idiomas ausentes, caché y pruebas con escaneo real | `gpu:0`, `cpu` | — | 3,5 h | sí | sí |
| 6 | buffer | cualquiera | regresión cruzada: migración real, `.ecat`/WOF, integración | `disk:0` | — | 4 h | sí | sí |

Reparto por disco: en ninguna noche hay dos bloques pesados sobre el mismo `disk:0`
simultáneos. Si el usuario fuerza Wiz y Census a la vez, Census cede.

## Criterio de aceptación de cada tanda

1. `cargo test` verde y `cargo build --release` sin warnings.
2. `CHANGELOG.md` con lo hecho y `push` al final.
3. `nightly-state.json` con `{task, phase, cursor, last_commit, updated}` al pausar.
4. No queda ningún lock tomado ni transacción abierta al pausar.
5. Si la tanda no cierra, se anota el paso exacto y sigue la noche siguiente.

## Qué no entra en las noches

Google Drive y cualquier unidad de nube: sólo metadata, sin hidratar, sin hash, sin OCR,
sin comprimidos y sin copiar entre volúmenes. Hidratar es una acción explícita del usuario,
con aviso de GB y tiempo, progreso y cancelar. Forzar no saltea esta regla.
