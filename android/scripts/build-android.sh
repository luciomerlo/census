#!/usr/bin/env bash
# Compila el nucleo Rust para Android y lo deja donde Gradle lo empaqueta.
# Requisitos: Android NDK (ANDROID_NDK_HOME), `cargo install cargo-ndk`,
# `rustup target add aarch64-linux-android`.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo ndk -t arm64-v8a -o app/src/main/jniLibs build --release
ls -l app/src/main/jniLibs/arm64-v8a/libcensus_android.so
# APK: ./gradlew :app:assembleRelease   (o assembleDebug para probar)
