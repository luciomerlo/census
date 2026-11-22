# Compila Census para Windows on ARM64 (Snapdragon X, Surface ARM, etc.).
#
# Es el mismo código que x64: no hay una sola rama `cfg(target_arch)` en el
# proyecto, así que no hay port que hacer. Lo que cambia es el linker.
#
# Uso:  pwsh tools/build-arm64.ps1
#       pwsh tools/build-arm64.ps1 -Run      # además corre los tests nativos
#
# Requisitos en la máquina de desarrollo x64:
#   rustup target add aarch64-pc-windows-msvc
#   Visual Studio 2022 con "MSVC v143 - ARM64 build tools" (linker arm64)
param([switch]$Run)

$ErrorActionPreference = "Stop"
$target = "aarch64-pc-windows-msvc"

rustup target add $target | Out-Null

Write-Host "== Clippy =="
cargo clippy --workspace --all-targets --target $target -- -D warnings
if ($LASTEXITCODE -ne 0) { throw "clippy falló" }

if ($Run) {
    Write-Host "== Tests (nativo ARM) =="
    cargo test --workspace --target $target
    if ($LASTEXITCODE -ne 0) { throw "tests fallaron" }
}

Write-Host "== Release =="
cargo build --workspace --release --target $target
if ($LASTEXITCODE -ne 0) { throw "build falló" }

$v = (Select-String -Path Cargo.toml -Pattern '^version = "(.*)"').Matches[0].Groups[1].Value
New-Item -ItemType Directory -Force dist | Out-Null
$out = "dist/census-$v-windows-arm64.exe"
Copy-Item "target/$target/release/census.exe" $out -Force
$size = (Get-Item $out).Length
Write-Host ("== Listo: {0} ({1:N0} bytes) ==" -f $out, $size)
