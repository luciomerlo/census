# Terceros

Census no copia código de Everything. Del proyecto de voidtools sólo se usó el
**layout documentado del protocolo IPC**, que es información de interfaz.

## Bases directas

| Paquete | Versión | Licencia | Uso |
|---|---|---|---|
| [`mft`](https://github.com/omerbenamram/mft) (Omer Ben-Amram) | 0.5.3 | MIT OR Apache-2.0 | Parseo de registros `$FILE`: fixups (USA), `$STANDARD_INFORMATION`, `$FILE_NAME`, extensiones. Los avisos de copyright originales están en `~/.cargo/registry/…/mft-0.5.3/LICENSE-{MIT,APACHE}` y se conservan al distribuir el binario. |
| [`winstructs`](https://github.com/omerbenamram/winstructs) | 0.3.2 | MIT/Apache-2.0 | Dependencia de `mft`: referencias de MFT y timestamps. |
| [`chrono`](https://github.com/chronotope/chrono) | 0.4 | MIT OR Apache-2.0 | Fechas locales para `dm:` y `rc:`. |
| [`serde_json`](https://github.com/serde-rs/json) | 1.0 | MIT OR Apache-2.0 | JSON del servidor MCP y del asistente. |
| [`ureq`](https://github.com/algesten/ureq) | 2.12 | MIT OR Apache-2.0 | HTTPS para la API de Groq (búsqueda en lenguaje natural). Trae `rustls`, `ring` y `webpki-roots`, todos permisivos. |
| [`windows-sys`](https://github.com/microsoft/windows-rs) | 0.59 | MIT OR Apache-2.0 | Bindings de Win32 (volumen, `DeviceIoControl`, ventana IPC, DPAPI). |

`cargo tree --depth 1` muestra el resto: son 368 paquetes en total contando
*dev-dependencies*, **todos** con licencias permisivas. Ninguno obliga a GPL/AGPL, así que
el binario se puede distribuir bajo MIT.

### Familias de licencia presentes

| Licencia | Paquetes | Nota |
|---|---:|---|
| MIT OR Apache-2.0 (y variantes `MIT/Apache-2.0`) | 262 | elección libre entre ambas |
| MIT sola | 77 | |
| Unicode-3.0 | 18 | tablas de `icu` que arrastra `rustls`; permisiva, tipo MIT |
| Apache-2.0 sola | 10 | |
| Zlib / CC0-1.0 / ISC / Unlicense / BSD | 37 | todas permisivas |
| BSL-1.0 | 2 | `clipboard-win`, `error-code`; permisiva |
| CDLA-Permissive-2.0 | 2 | `webpki-roots`: datos de certificados, uso libre |
| OFL-1.1 AND Ubuntu-font-1.0 | 1 | `epaint_default_fonts` (ver abajo) |
| MIT OR Apache-2.0 OR LGPL-2.1-or-later | 1 | `r-efi`: se toma MIT/Apache, la opción LGPL no se usa |

Dos detalles que conviene dejar por escrito:

- **Fuentes del tema por defecto de egui** (`epaint_default_fonts`): son Hack, Ubuntu-Light,
  NotoEmoji y emoji-icon-font, bajo OFL-1.1 / Ubuntu-font-1.0. Son permisivas y permiten
  redistribución; el aviso de copyright viaja en el propio crate y se mantiene al
  distribuir el binario. La ventana usa además **Segoe UI** y **Segoe UI Symbol** del
  sistema (`%WINDIR%\Fonts`), que **no** se empaquetan.
- **`pin-project`** no declara `license` en su `Cargo.toml` sino un archivo de licencia
  (MIT o Apache-2.0, a elección) en el crate; está en `~/.cargo/registry/…`.

Verificación reproducible:

```powershell
cargo tree --workspace --format "{p} {l}" | Sort-Object -Unique
```

## Referencias de interfaz (sin código copiado)

- **Everything SDK / documentación de voidtools**: layout del pedido QUERY2W y de la
  respuesta (cabecera de 20 bytes, tabla de items de 8 bytes, cadenas con prefijo de
  largo en UTF-16). Es el contrato que las apps ya hablan; se reimplementó desde la
  especificación del protocolo, no desde el binario.
- **SortEverything** y **WizEverything** (apps del autor del proyecto): de ahí salió la
  lista concreta de consultas y campos que el motor tiene que responder, y el parser de
  respuesta que sirve de oráculo en los tests.

## Nombres de terceros

"Everything" es un producto de voidtools. Census no está afiliado, no usa su nombre como
marca propia, no incluye su binario ni su formato de base de datos, y no redistribuye
`Everything64.dll`. El modo de compatibilidad del shim es opcional, explícito y está
documentado en el README.
