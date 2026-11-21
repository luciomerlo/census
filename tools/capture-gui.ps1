# Captura la ventana de Census en PNG (solo la ventana, no el escritorio).
# Uso: powershell -NoProfile -ExecutionPolicy Bypass -File tools\capture-gui.ps1 -Theme light
param(
    [string]$Theme = "light",
    [string]$Root = "C:/Users/Lucio/Downloads/WizEverything/src",
    [string]$Query = "",
    [int]$WaitMs = 2500
)

$here = Split-Path -Parent $PSScriptRoot
$exe = Join-Path $here "target\release\census.exe"
$out = Join-Path $here "docs\gui-$Theme.png"
New-Item -ItemType Directory -Force -Path (Join-Path $here "docs") | Out-Null

Add-Type -AssemblyName System.Drawing
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
public class CensusWin {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT r);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern int GetWindowThreadProcessId(IntPtr hWnd, out int pid);
    [DllImport("user32.dll")] public static extern int GetClassName(IntPtr hWnd, System.Text.StringBuilder sb, int max);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc cb, IntPtr p);
    [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr hWnd, int x, int y, int w, int h, bool repaint);
    public delegate bool EnumProc(IntPtr hWnd, IntPtr p);

    // La ventana del proceso que interesa: la mas grande que no sea la consola.
    public static IntPtr FindWindow(int pid) {
        IntPtr best = IntPtr.Zero;
        long bestArea = 0;
        EnumWindows(delegate(IntPtr h, IntPtr p) {
            int owner; GetWindowThreadProcessId(h, out owner);
            if (owner != pid || !IsWindowVisible(h)) return true;
            var sb = new System.Text.StringBuilder(256);
            GetClassName(h, sb, 256);
            if (sb.ToString() == "ConsoleWindowClass") return true;
            RECT r; if (!GetWindowRect(h, out r)) return true;
            long area = (long)(r.Right - r.Left) * (r.Bottom - r.Top);
            if (area > bestArea) { bestArea = area; best = h; }
            return true;
        }, IntPtr.Zero);
        return best;
    }
}
"@

$args = @('gui', '--root', $Root, '--theme', $Theme)
if ($Query -ne "") { $args += $Query }
$p = Start-Process -FilePath $exe -ArgumentList $args -PassThru
Start-Sleep -Milliseconds $WaitMs

try {
    $proc = Get-Process -Id $p.Id -ErrorAction Stop
    $proc.Refresh()
    $h = [CensusWin]::FindWindow($p.Id)
    if ($h -eq [IntPtr]::Zero -or $h -eq $null) { throw "la ventana no apareció" }
    [CensusWin]::SetForegroundWindow($h) | Out-Null
    # Tamano y posicion fijos: las capturas salen siempre iguales.
    $wa = [System.Windows.Forms.Screen]::PrimaryScreen.WorkingArea
    $ww = [Math]::Min($wa.Width - 20, 1180)
    $wh = [Math]::Min($wa.Height - 20, 660)
    [CensusWin]::MoveWindow($h, 10, 10, $ww, $wh, $true) | Out-Null
    Start-Sleep -Milliseconds 700

    $r = New-Object CensusWin+RECT
    [CensusWin]::GetWindowRect($h, [ref]$r) | Out-Null
    if ($r.Right -le $r.Left) { throw "ventana sin rect" }
    # Clamp al area visible: la ventana puede pedir mas alto que la pantalla.
    $screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
    $left = [Math]::Max(0, $r.Left)
    $top = [Math]::Max(0, $r.Top)
    $w = [Math]::Min($r.Right, $screen.Right) - $left
    $hgt = [Math]::Min($r.Bottom, $screen.Bottom) - $top
    if ($w -le 0 -or $hgt -le 0) { throw "rect inválido" }

    $bmp = New-Object System.Drawing.Bitmap $w, $hgt
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.CopyFromScreen($left, $top, 0, 0, $bmp.Size)
    $bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
    $g.Dispose()
    $bmp.Dispose()
    "ok: $out ($w x $hgt)"
} catch {
    "error: $_"
} finally {
    if (-not $p.HasExited) { $p.Kill() }
}
