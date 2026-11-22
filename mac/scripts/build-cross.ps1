# Compila Census para macOS Intel + Apple Silicon desde Windows, sin Mac.
#
# Requiere una vez:
#   python -m pip install --user cargo-zigbuild ziglang
#   rustup target add x86_64-apple-darwin aarch64-apple-darwin
#
# Salida: target/universal2-apple-darwin/release/census (Mach-O universal2).
# No firma ni notariza; para un .app firmado usá el workflow mac.yml.

$ErrorActionPreference = "Stop"
$pyScripts = python -c "import sysconfig; print(sysconfig.get_path('scripts', scheme='nt_user'))"
if ($pyScripts -and (Test-Path $pyScripts)) { $env:PATH = "$pyScripts;$env:PATH" }
Set-Location (Join-Path $PSScriptRoot "..")

if (-not (Get-Command cargo-zigbuild -ErrorAction SilentlyContinue)) {
    Write-Error "falta cargo-zigbuild; instalá: python -m pip install --user cargo-zigbuild ziglang"
    exit 1
}

# ziglang de pip no deja `zig` en PATH; se lo pasamos a cargo-zigbuild.
if (-not $env:CARGO_ZIGBUILD_ZIG_PATH) {
    $zig = python -c "import pathlib, ziglang; print(pathlib.Path(ziglang.__file__).parent / 'zig.exe')"
    if (-not (Test-Path $zig)) {
        Write-Error "no encontré zig.exe; revisá la instalación de ziglang"
        exit 1
    }
    $env:CARGO_ZIGBUILD_ZIG_PATH = $zig
}

rustup target add x86_64-apple-darwin aarch64-apple-darwin | Out-Host
cargo zigbuild --release --target universal2-apple-darwin
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }

$bin = "target/universal2-apple-darwin/release/census"
if (-not (Test-Path $bin)) {
    Write-Error "no se generó $bin"
    exit 1
}

$size = (Get-Item $bin).Length
Write-Host "listo: $bin ($size bytes)"
if (Get-Command llvm-objdump -ErrorAction SilentlyContinue) {
    llvm-objdump --macho --universal-headers $bin
} elseif (Get-Command file -ErrorAction SilentlyContinue) {
    file $bin
} else {
    Write-Host "verificá con: llvm-objdump --macho --universal-headers $bin"
}
