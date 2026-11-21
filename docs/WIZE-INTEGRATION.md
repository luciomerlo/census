# Integración con WizEverything

WizEverything puede usar Census como motor de índice alternativo a Everything.
No necesita DLL ni un formato propio: habla el mismo IPC `QUERY2W` por
`WM_COPYDATA`, pero busca la ventana de clase `CENSUS_IPC`.

## Cómo se conecta

1. Arrancar Census con el volumen a indexar:

   ```powershell
   census serve --volume C
   ```

   Requiere elevación para leer la MFT. Para pruebas sin elevación:

   ```powershell
   census serve --root C:\Users\Lucio\Downloads\WizEverything
   ```

2. En WizEverything: **Opciones → Motor de índice → Census** (o **Auto**).
   Auto prefiere Everything si está listo; si no, Census.

3. WizEverything pide páginas de 250.000 items con `QUERY2W`, flags
   `NAME|PATH|SIZE|DATE_MODIFIED`, y arma su árbol en memoria.

## Requisito de UIPI (importante)

Cuando `census serve` corre elevado y WizEverything corre como usuario normal,
Windows bloquea `WM_COPYDATA` y `WM_USER + 401` desde el proceso de integridad
media hacia la ventana elevada. El servidor debe permitir explícitamente esos
mensajes:

```rust
ChangeWindowMessageFilterEx(hwnd, WM_COPYDATA, MSGFLT_ALLOW, null_mut());
ChangeWindowMessageFilterEx(hwnd, WM_USER + IS_DB_LOADED, MSGFLT_ALLOW, null_mut());
```

Está aplicado en `crates/census-ipc/src/server.rs`. Sin esto, WizEverything
recibe `SendMessageW == 0` (error 5) y muestra un mensaje de permisos, no un
falso "Census no está en ejecución".

## Límites actuales

- Census sirve **un volumen por vez**. Everything puede mostrar todas las
  unidades; si elegís Census, WizEverything muestra sólo ese volumen.
- Los metadatos `accessed` y `run_count` pueden venir vacíos según el motor;
  WizEverything los marca como parciales en los análisis que dependen de ellos.
- WizEverything no inicia Census solo: el diálogo de arranque ofrece
  `census.exe`, `census serve` y la instalación de Everything por winget.

## Prueba rápida

```powershell
census serve --root C:\Users\Lucio\Downloads\WizEverything
# en otra terminal, con engine=census en settings.ini:
wizeverything.exe --mcp
# tools/call refresh -> {"items": >0, "engine": "Census"}
```
