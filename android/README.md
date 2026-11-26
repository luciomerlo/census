# CensusIndex Android

Núcleo Rust (`src/`, reutiliza `../linux`) + app Kotlin (`app/`). Diseño, comparación con productos existentes y medidas: [`docs/ANDROID.md`](../docs/ANDROID.md).

```bash
cargo test                                   # 16 tests del núcleo, en cualquier PC
cargo run --release --example bench -- 20000 10
cargo check --target aarch64-linux-android   # compila para el teléfono
```

## Compilar el APK

Requisitos: Android SDK (platform 35, build-tools 35), NDK 27, Gradle 8.9+, `cargo install cargo-ndk`, `rustup target add aarch64-linux-android`.

```bash
export ANDROID_HOME=... ANDROID_NDK_HOME=...
./scripts/build-android.sh          # -> app/src/main/jniLibs/arm64-v8a/libcensus_android.so
gradle :app:assembleDebug           # -> app/build/outputs/apk/debug/app-debug.apk
```

`minSdk 30` (Android 11): `StorageVolume.directory` y `MediaStore.getGeneration`. Instalar el APK, tocar **Permitir acceso a todos los archivos**, volver: indexa la primera vez y después se mantiene solo.

## API local

*Compartir índice* levanta `http://<ip-del-teléfono>:8787/v1/` (contrato [`MINIMO.md`](../docs/MINIMO.md)) con `Authorization: Bearer <token>`; el token se muestra en la pantalla principal. Usarlo por la IP de Tailscale del teléfono.
