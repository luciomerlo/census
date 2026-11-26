# Plataformas: arquitecturas y sistemas

Fecha: 2026-11-26. Distingo tres niveles: **verificado** (compilé o corrí algo aquí), **viable** (el toolchain lo soporta, sin probar) y **no viable con Rust actual**.

## 1. Lo verificado hoy

`cargo check` del crate `linux/` (índice, consultas, snapshot `.cns`, API HTTP; sin TLS → `--no-default-features --features plain`):

| Destino | Resultado |
|---|---|
| x86_64 / i686 Linux | OK (x86_64 además ejecuta los 59 tests) |
| ARM: aarch64 Android (**enlazado y empaquetado**), armv7 Android, armv7 Linux gnueabihf | OK |
| RISC-V: riscv64gc Linux gnu y musl | OK |
| PowerPC: powerpc (32 BE), powerpc64 (BE), powerpc64le | OK |
| s390x, loongarch64 | OK |
| x86 Android: i686, x86_64 | OK |
| FreeBSD x86_64, illumos x86_64 | OK |
| macOS aarch64 y x86_64 (solo este crate; el de macOS es `mac/`) | OK |
| NetBSD x86_64 | **Falla**: `mio 0.8` (dependencia de `notify`) no compila; se arregla subiendo `notify` o sacando el vigilante en esa plataforma |

`check` no es ejecutar: confirma que compila, no que corre. Solo aarch64-Android se enlazó con NDK y entró en un APK; **nada se ejecutó en hardware ARM/RISC-V/PowerPC**.

**TLS (Groq/OpenRouter por https):** la IA usa `rustls` con `ring`, verificado solo en aarch64-Android. `ring` no soporta powerpc, s390x ni mips. En esas arquitecturas: compilar con `--features plain` y apuntar a (a) una IA local por `http://` o (b) un relé en otra máquina de la red.

## 2. Matriz por sistema

| Sistema | x86 / x64 | ARM | RISC-V | Notas |
|---|---|---|---|---|
| **Windows 10/11** | Sí (MFT) | ARM64 en CI | Sin target Rust | Windows 7/8: existen targets `*-win7-*` (nivel 3), sin probar |
| **Linux** | Sí | Sí | Sí | Todos los destinos de la tabla anterior; musl estático recomendado |
| **Android** | x86_64, i686 | arm64, armv7 | `riscv64-linux-android` existe (nivel 3) | minSdk 30 por `MediaStore` generación |
| **macOS Intel / Apple silicon** | `x86_64-apple-darwin` | `aarch64-apple-darwin` | — | Crate `mac/` con `getattrlistbulk` (macOS 10.10+) y FSEvents |
| **macOS PowerPC (Tiger/Leopard)** | — | — | — | **No viable con Rust actual**: `powerpc-apple-darwin` no figura en la lista de destinos de rustc 1.97. Ver §3 |

## 3. Mac PowerPC: qué sí se puede

1. **Cliente de consulta** (lo más realista): la API v1 de otra máquina se consulta desde un navegador viejo (p. ej. TenFourFox) o `curl`. No indexa el Mac, pero lo hace buscar en todo el catálogo.
2. **Exportador en C99** (`census-lite`): `readdir`+`lstat` recursivo que escribe un `.cns` o JSON de nombres, ejecutado por `cron`/`launchd`. Sin `getattrlistbulk` (10.10+) ni FSEvents (10.5+ parcial). Es un programa aparte: no comparte código con el crate Rust.
3. **Sin TLS moderno** en 10.4/10.5: la IA en la nube no sirve ahí; usar relé o IA local por `http://`.
No lo implementé; es una decisión de producto si vale la pena.

## 4. Opciones de plataforma que no te había dado

| Opción | Por qué sirve | Viabilidad |
|---|---|---|
| **NAS y routers** (Synology, QNAP, OpenWrt, TrueNAS) | Es donde viven los discos grandes: indexarlos en el propio equipo en vez de por la red. Tu plan con BigTreeTech Pi 2 + OpenMediaVault cae acá | Linux aarch64/armv7/mips musl estático; mips sin `ring` → `plain` |
| **Raspberry Pi / SBC RISC-V (VisionFive, Milk-V)** | Hub 24/7 de Atlas y servidor de catálogo con poco consumo | Verificado `check` para armv7/aarch64/riscv64 |
| **FreeBSD / TrueNAS CORE / pfSense** | Mismo caso NAS en BSD | FreeBSD OK; NetBSD y OpenBSD sin probar |
| **ChromeOS (Crostini)** | Linux en contenedor | Es Linux: sirve el binario Linux |
| **Termux (Android sin APK)** | Probar el núcleo en un teléfono sin firmar nada: `census index /sdcard` | El binario `aarch64-linux-android` sirve; sin servicio en segundo plano |
| **iOS / iPadOS** | Tablet/teléfono Apple | **No viable** para indexar: el sandbox no deja leer el sistema de archivos; solo cliente de consulta |
| **HarmonyOS / OpenHarmony** | Teléfonos Huawei sin Google | Existen destinos `*-linux-ohos`; app ArkTS aparte; solo si lo usás |
| **Cliente web (PWA)** | Una sola interfaz de búsqueda para cualquier navegador contra `/v1/search` | Viable y barato: cubre iOS, PowerPC (navegador viejo) y TVs |
| **Cliente de consola (`census query` por SSH/Tailscale SSH)** | Cualquier equipo con SSH | Ya existe en Linux/mac |
| **Windows en 32 bits / Windows 7** | PCs viejas de oficina | `i686-pc-windows-msvc` es nivel 1; Win7 nivel 3 |
| **Haiku, Redox, Solaris/illumos** | Curiosidad | illumos compila; el resto sin probar y sin demanda real |

Recomendación de orden: (1) cliente web, porque cubre todo lo que no se puede compilar; (2) NAS/Pi/Termux, que reutilizan el binario Linux; (3) iOS solo como cliente.
