# Ejecuta el benchmark de CensusIndex con elevación y deja el resultado en un archivo.
# Uso: click derecho > Ejecutar con PowerShell, o:
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\bench-elevated.ps1
param(
    [string]$Volume = "C",
    [int]$Repeat = 3
)

$here = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $here "target\release\censusindex.exe"
$out = Join-Path $here "bench.txt"

if (-not (Test-Path $exe)) {
    "no existe $exe (compilá con: cargo build --release)" | Set-Content $out
    exit 1
}

"# CensusIndex bench - $(Get-Date -Format o)" | Set-Content $out
"# exe: $exe" | Add-Content $out
& $exe bench --volume $Volume --repeat $Repeat *>> $out
"# exit=$LASTEXITCODE" | Add-Content $out
