# Comprueba que `census serve` registre la ventana esperada y que las dos clases
# sigan soportadas. Uso: powershell -File tools\check-ipc-window.ps1 [-Compat]
param([string]$Root = "C:/Users/Lucio/Downloads/WizEverything/src", [switch]$Compat)
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class W {
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern IntPtr FindWindow(string c, string n);
}
"@
$here = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $here "target\release\census.exe"
$args = @("serve", "--root", $Root)
if ($Compat) { $args += "--compat" }
$p = Start-Process -FilePath $exe -ArgumentList $args -PassThru -RedirectStandardOutput "$env:TEMP\census-serve.out" -RedirectStandardError "$env:TEMP\census-serve.err"
Start-Sleep -Milliseconds 2500
try {
    $hOwn = [W]::FindWindow("CENSUS_IPC", $null)
    $hOther = [W]::FindWindow("EVERYTHING_TASKBAR_NOTIFICATION", $null)
    "CENSUS_IPC: $(if ($hOwn -ne [IntPtr]::Zero) { 'ventana viva' } else { 'no' })"
    "EVERYTHING_TASKBAR_NOTIFICATION: $(if ($hOther -ne [IntPtr]::Zero) { 'existe (puede ser el Everything real)' } else { 'libre' })"
    "salida:"
    Get-Content "$env:TEMP\census-serve.out" -ErrorAction SilentlyContinue | Select-Object -First 3
} finally {
    if (-not $p.HasExited) { $p.Kill() }
}
