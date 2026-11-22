# Census Core v1 — contrato mínimo

Census es la **base de datos madre** de las demás aplicaciones (SortEverything,
WizEverything, Ocupa y cualquier cliente futuro). No es una app de escritorio
con funciones propias: indexa una vez y ofrece consultas de sólo lectura con un
contrato estable.

Este documento congela el **v1**. Todo lo que no está acá queda fuera, aunque
el código ya exista. Nada se borra: lo diferido se apaga y no se documenta como
parte del producto.

## Lo que el cliente puede esperar

- Un índice local por PC, guardado en un snapshot opaco. El cliente **nunca**
  lee el snapshot ni conoce su formato.
- Consultas rápidas por nombre y ruta, más listados de volúmenes y estadísticas.
- La misma semántica y el mismo JSON en Windows, macOS y Linux.
- Sólo lectura. Census no mueve, renombra ni borra archivos.

## Fuera de v1

Atlas, Tailscale, federación entre PCs, volúmenes offline, alias de nube,
overrides manuales, FSEvents/USN en caliente, MCP, GUI y exportación
`C.wcat` como función de cliente. El código existente de esas áreas se
conserva apagado; no se elimina.

La nube y la red se indexan **sólo** si el usuario pasa una ruta explícita.
No hay identidad de nube ni fusión de unidades en v1.

## CLI

Cinco comandos de usuario y un daemon:

| Comando | Para qué |
|---|---|
| `census index [ruta...]` | Indexa los volúmenes locales montados, o las rutas dadas, y guarda el snapshot. |
| `census query <texto>` | Busca por nombre/ruta. Uso local y de diagnóstico. |
| `census top [N]` | Archivos más grandes. |
| `census stats` | Estado y tamaño del índice. |
| `census volumes` | Volúmenes indexados y su identidad. |
| `census serve` | Levanta la API HTTP de sólo lectura para las apps. |

`query`, `top`, `stats` y `volumes` aceptan `--json`. El JSON va a stdout y los
diagnósticos a stderr; con `--json` no se imprime texto humano.

Flags comunes de `query`/`top`:

- `--path <prefijo>` — limita a una ruta.
- `--ext <ext>` — extensión sin punto.
- `--min-size <bytes>` — tamaño mínimo.
- `--sort name|size|mtime` — por defecto `name`.
- `--limit <N>` — `0` = todos. `query` usa 500 por defecto; `top`, 50.

`index` sin argumentos indexa todos los volúmenes locales montados (fijos y
extraíbles), sin red ni nube. Con una ruta explícita indexa esa ruta aunque sea
de red o nube. Si un volumen falla, se indexa el resto y `complete` queda en
`false`.

## API HTTP

Sólo lectura. Por defecto `127.0.0.1:8787`. `serve --port <N>` permite otro
puerto; `--lan` bindea `0.0.0.0` y **exige** un token no vacío. Si hay token
configurado, se exige también en loopback. Sin token no se puede usar `--lan`.

```
GET /v1/info
GET /v1/health
GET /v1/volumes
GET /v1/search?q=&path=&ext=&min_size=&sort=&limit=
GET /v1/top?limit=
GET /v1/stats
```

- `/v1/info` → `{protocol:"census-v1", pc_id, name, version, port}`
- `/v1/health` → `{ok:true}`
- `/v1/volumes` → `{pc_id, built_at, volumes:[Volume]}`
- `/v1/search` → `{query, total, returned, results:[File]}`
- `/v1/top` → `{pc_id, built_at, results:[File]}` (sólo archivos)
- `/v1/stats` → `{pc_id, built_at, age_s, generation, volumes, nodes, files, dirs, bytes}`

`search` incluye archivos y carpetas, como Everything; `top` sólo archivos.
`limit=0` significa todos. Todas las respuestas son un único objeto JSON.

### Volume

```json
{
  "pc_id": "0123456789abcdef0123456789abcdef",
  "volume_id": "vol:...",
  "label": "Macintosh HD",
  "mount": "/",
  "fs": "apfs",
  "total_bytes": 994662584320,
  "free_bytes": 512000000000,
  "used_bytes": 482662584320,
  "removable": false,
  "online": true,
  "built_at": 1700000000,
  "model": "",
  "serial": "",
  "partition_offset": 0,
  "cloud_id": null,
  "identity_status": "ok",
  "federable": true,
  "marca": "m1:...:123"
}
```

- `label` es el nombre visible; `model` es hardware o `""`.
- `used_bytes = total_bytes - free_bytes`, nunca negativo.
- `online` es verdadero sólo si el volumen está montado ahora.
- `marca` es opaco; el cliente no lo interpreta. Si el snapshot no la tiene, es `null` (no se omite el campo).
- `marca`: string opaco o `null`; nunca se omite.
- `identity_status`: `ok | serial_ambiguo | posible_duplicado | sin_identidad | cloud`.
- `federable` es `true` sólo con `identity_status:"ok"` y `volume_id` real. En
  v1 nadie federa; el campo queda para no migrar el JSON después.

### File

```json
{
  "path": "/Users/lucio/Documentos/informe.pdf",
  "name": "informe.pdf",
  "size_bytes": 123456,
  "alloc_bytes": 131072,
  "mtime_ns": 1700000000000000000,
  "kind": "file",
  "volume_id": "vol:...",
  "source": {"pc_id": "0123456789abcdef0123456789abcdef", "state": "online"}
}
```

- `kind`: `file | dir | symlink | other` (string, no número).
- `source.state`: `online | stale | offline`. En v1 siempre `online`; los otros
  valores quedan reservados para cuando exista catálogo multi-PC.
- `path` es absoluto y usa `/` en las tres plataformas.
- Nombres no UTF-8: el JSON los muestra con reemplazo U+FFFD; el snapshot los
  conserva.

## Índice y snapshot

- Un snapshot global por PC, guardado atómicamente, con una sección por
  volumen que incluye `volume_id`, `marca`, raíces y metadatos.
- Si cambia el punto de montaje de un volumen, se remapea **su sección**; no se
  reindexa el resto.
- El formato es interno de cada plataforma. El contrato es la API, no el
  archivo.
- `volume_id` sale del hardware/partición/UUID, nunca de la letra ni del punto
  de montaje. Un disco ausente no se fusiona con otro por llamarse igual.

## Errores

- `query`, `top`, `stats` o `volumes` sin snapshot: exit 2 y stderr
  `no hay índice; corré census index`.
- `index` sin volúmenes indexables: exit 1.
- API sin índice: `503` con `{"error":"no_index"}`.
- API con token y sin `Authorization: Bearer`: `401`.
- Parámetro inválido: `400` con `{"error":"bad_request","detail":"..."}`.

## Paridad

| Plataforma | CLI | API HTTP | Snapshot |
|---|---|---|---|
| Windows | existente | **prevista en v1** | `.wcat` |
| macOS | existente | a implementar | `.cnx` |
| Linux | existente | a implementar | `.cns` |

La API de Windows es trabajo nuevo: hoy `serve` es IPC por named pipe. No se
documenta como hecha hasta que pase las pruebas.

## Cambios

Cualquier cambio a campos, endpoints o semántica requiere acuerdo de los tres
chats de plataforma y del cliente que lo consume, y sube la versión
(`census-v2`). Agregar un campo opcional no rompe; cambiar un nombre o un tipo,
sí.
