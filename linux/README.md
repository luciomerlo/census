# Census para Linux

Índice de archivos en RAM, snapshot global y API HTTP de sólo lectura. Corre en
**x86_64 (Intel/AMD)** y **aarch64 (ARM64)** Linux. Un solo binario estático:
no necesita glibc, librerías ni Python.

## v1 en 30 segundos

```bash
census index                 # montajes locales (sin red/nube)
census query informe --ext pdf --sort size --limit 20
census top 20
census stats
census volumes --json
census serve --port 8787    # API en http://127.0.0.1:8787/v1/
```

`index` sin argumentos indexa los volúmenes locales montados (fijos y
extraíbles), nunca red/nube por defecto. Para una carpeta o una nube montada:

```bash
census index /home/lucio/Documentos
census index /mnt/google-drive
```

El snapshot vive en `$XDG_DATA_HOME/census/index.cns` (o
`$CENSUS_DATA_DIR/index.cns`) y se reemplaza atómicamente. `index` recorre con
`getdents64` + `statx` y guarda nombres no UTF-8 como bytes; no borra, mueve ni
renombra nada.

## Build: Intel y ARM

Requisitos: Rust estable (1.80+), `rustup`, `git`. En Windows/macOS el script
usa `rust-lld`; en Linux usa el linker nativo. Los targets son **musl estático**
para que el mismo artefacto corra en cualquier distro.

```bash
cd linux
./scripts/build-linux.sh
# dist/census-0.1.0-linux-x86_64.tar.gz
# dist/census-0.1.0-linux-aarch64.tar.gz
# dist/SHA256SUMS.txt
```

Sólo un target:

```bash
./scripts/build-linux.sh x86_64-unknown-linux-musl
./scripts/build-linux.sh aarch64-unknown-linux-musl
```

Instalación desde el tarball:

```bash
tar xzf census-0.1.0-linux-x86_64.tar.gz -C /tmp/census
/tmp/census/install.sh          # ~/.local/bin/census + unidad systemd
census index
census serve --port 8787
```

En ARM64 el binario es `aarch64-unknown-linux-musl`; en Intel/AMD,
`x86_64-unknown-linux-musl`. El JSON es idéntico en ambas arquitecturas.

## API read-only (`/v1/`)

Loopback por defecto en `127.0.0.1:8787`. `--lan` bindea `0.0.0.0` y exige
`Authorization: Bearer <token>` (token en la config); sin token no arranca.
Sin discovery UDP: el cliente usa IP/host manual.

| GET | Devuelve |
|---|---|
| `/v1/info` | `protocol,pc_id,name,version,port` |
| `/v1/health` | `{ok:true}` |
| `/v1/volumes` | `{pc_id,built_at,volumes:[...]}` |
| `/v1/search?q=&path=&ext=&min_size=&sort=&limit=` | `{query,total,returned,results:[...]}` |
| `/v1/top?limit=` | `{pc_id,built_at,results:[...]}` |
| `/v1/stats` | `{pc_id,built_at,age_s,generation,volumes,nodes,files,dirs,bytes}` |

Cada resultado incluye `path,name,size_bytes,alloc_bytes,mtime_ns,kind,volume_id`
y `source:{pc_id,state}`. `limit=0` devuelve todo; `search` usa 500 y `top` 50
por defecto. El contrato completo está en [`../docs/MINIMO.md`](../docs/MINIMO.md).

## Identidad (breve)

- `volume_id` sale de `DiskHwId + offset de partición + UUID de filesystem`;
  **nunca** de `sdX`, de la letra ni de `/media/...`.
- Dos discos con la misma etiqueta `BACKUP` y distinta serial son dos
  volúmenes; el que no está montado queda offline y no se fusiona.
- `identity_status` ∈ `ok | serial_ambiguo | posible_duplicado | sin_identidad | cloud`.
  `federable:true` sólo con `ok` e identidad real; en v1 nadie federa.
- La nube sólo se indexa si pasás la ruta montada explícitamente.

## Estado

v1: CLI, snapshot global, consultas y API. **No** incluye GUI, MCP, Tailscale,
federación multi-PC, watchers ni `export --wcat`. Lo pendiente y los bloqueos
están en [`PENDIENTE.md`](PENDIENTE.md).

Tests: `cargo test` (42 en Windows; CI Linux corre además clippy `-D warnings`).
