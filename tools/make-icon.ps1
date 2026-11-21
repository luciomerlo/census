# Genera assets/icon-256.png y assets/icon.ico (16/32/48/256) con la misma
# geometria que assets/icon.svg: lente ambar (homenaje a la lupa de Everything)
# con un indice de filas dentro.
# Uso: powershell -NoProfile -ExecutionPolicy Bypass -File tools\make-icon.ps1
param([string]$OutDir = "")

$here = Split-Path -Parent $PSScriptRoot
if ($OutDir -eq "") { $OutDir = Join-Path $here "assets" }
New-Item -ItemType Directory -Force -Path $OutDir | Out-Null

Add-Type -AssemblyName System.Drawing

function New-IconBitmap([int]$size) {
    $bmp = New-Object System.Drawing.Bitmap $size, $size, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)
    $s = $size / 256.0

    # A 16-24 px el anillo y el mango finos se pierden: se engordan.
    $ringW = if ($size -le 24) { 26 } elseif ($size -le 48) { 20 } else { 18 }
    $handW = if ($size -le 24) { 30 } elseif ($size -le 48) { 26 } else { 24 }

    # mango
    $handlePen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 0x28, 0x30, 0x3D)), ($handW * $s)
    $handlePen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $handlePen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $g.DrawLine($handlePen, [float](150 * $s), [float](150 * $s), [float](197 * $s), [float](197 * $s))
    $handlePen.Dispose()

    # lente
    $cx = 104 * $s; $cy = 104 * $s; $r = (62 - $ringW / 2) * $s
    $rect = New-Object System.Drawing.RectangleF ([float]($cx - $r)), ([float]($cy - $r)), ([float]($r * 2)), ([float]($r * 2))
    if ($size -le 24) {
        # A 16 px el degradado se funde con el anillo: lente clara y anillo oscuro.
        $brush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 0xFF, 0xD8, 0x9E))
    } else {
        $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, ([System.Drawing.Color]::FromArgb(235, 0xFF, 0xC2, 0x4A)), ([System.Drawing.Color]::FromArgb(240, 0xC8, 0x5A, 0x08)), 45.0
    }
    $g.FillEllipse($brush, $rect)
    $brush.Dispose()

    # anillo
    $ringColor = if ($size -le 24) { [System.Drawing.Color]::FromArgb(255, 0xC4, 0x54, 0x06) } else { [System.Drawing.Color]::FromArgb(255, 0xEE, 0x8A, 0x16) }
    $ringPen = New-Object System.Drawing.Pen $ringColor, ($ringW * $s)
    $ringRect = New-Object System.Drawing.RectangleF ([float]($cx - 62 * $s)), ([float]($cy - 62 * $s)), ([float](124 * $s)), ([float](124 * $s))
    $g.DrawEllipse($ringPen, $ringRect)
    $ringPen.Dispose()

    # filas del indice
    $barColor = [System.Drawing.Color]::FromArgb(255, 0xFF, 0xF3, 0xDC)
    $faint = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(140, 0xFF, 0xF3, 0xDC))
    $bright = New-Object System.Drawing.SolidBrush $barColor
    if ($size -ge 32) {
        $g.FillRectangle($faint, [float](72 * $s), [float](80 * $s), [float](64 * $s), [float](11 * $s))
        $g.FillRectangle($faint, [float](72 * $s), [float](118 * $s), [float](64 * $s), [float](11 * $s))
        $g.FillRectangle($bright, [float](72 * $s), [float](99 * $s), [float](72 * $s), [float](11 * $s))
    } else {
        # a 16 px las filas se funden: una sola barra ancha y legible
        $g.FillRectangle($bright, [float](74 * $s), [float](98 * $s), [float](60 * $s), [float](14 * $s))
    }
    $faint.Dispose(); $bright.Dispose()

    $g.Dispose()
    return $bmp
}

# PNG grande, para el README
$big = New-IconBitmap 256
$png = Join-Path $OutDir "icon-256.png"
$big.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
$big.Dispose()

# ICO: 16/32/48 en DIB clasico (BGRA + mascara AND) y 256 en PNG.
# Windows solo acepta PNG de forma confiable en 256x256; con PNG en los chicos
# el Explorador muestra un icono generico o en blanco.
function Get-DibBytes([System.Drawing.Bitmap]$bmp) {
    $w = $bmp.Width; $h = $bmp.Height
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter $ms
    # BITMAPINFOHEADER: alto doble (XOR + AND), 32 bpp, sin compresion.
    $bw.Write([UInt32]40)
    $bw.Write([Int32]$w)
    $bw.Write([Int32]($h * 2))
    $bw.Write([UInt16]1)
    $bw.Write([UInt16]32)
    $bw.Write([UInt32]0)
    $bw.Write([UInt32]($w * $h * 4))
    $bw.Write([Int32]0); $bw.Write([Int32]0)
    $bw.Write([UInt32]0); $bw.Write([UInt32]0)
    # XOR: BGRA de abajo hacia arriba
    for ($y = $h - 1; $y -ge 0; $y--) {
        for ($x = 0; $x -lt $w; $x++) {
            $c = $bmp.GetPixel($x, $y)
            $bw.Write([byte]$c.B); $bw.Write([byte]$c.G); $bw.Write([byte]$c.R); $bw.Write([byte]$c.A)
        }
    }
    # AND: 1 bit por pixel, filas alineadas a 4 bytes, todo 0 (el alfa manda)
    $rowBytes = [int]([Math]::Ceiling($w / 32.0) * 4)
    for ($y = 0; $y -lt $h; $y++) {
        for ($b = 0; $b -lt $rowBytes; $b++) { $bw.Write([byte]0) }
    }
    $bw.Flush()
    $bytes = $ms.ToArray()
    $bw.Dispose(); $ms.Dispose()
    return $bytes
}

# Payloads explicitos: nada de arrays anidados (PowerShell los aplana).
$b16 = New-IconBitmap 16;  $p16 = Get-DibBytes $b16; $b16.Dispose()
$b32 = New-IconBitmap 32;  $p32 = Get-DibBytes $b32; $b32.Dispose()
$b48 = New-IconBitmap 48;  $p48 = Get-DibBytes $b48; $b48.Dispose()
$b256 = New-IconBitmap 256
$ms = New-Object System.IO.MemoryStream
$b256.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
$p256 = $ms.ToArray()
$ms.Dispose(); $b256.Dispose()

$frames = @(
    @{ Size = 16;  Bpp = 32; Data = $p16 },
    @{ Size = 32;  Bpp = 32; Data = $p32 },
    @{ Size = 48;  Bpp = 32; Data = $p48 },
    @{ Size = 256; Bpp = 32; Data = $p256 }
)

$icoPath = Join-Path $OutDir "icon.ico"
# Se arma todo en una lista de bytes y se escribe de una: con BinaryWriter los
# overloads de Write(byte[]) no son confiables desde PowerShell.
$all = New-Object System.Collections.Generic.List[byte]
$all.AddRange([byte[]](0, 0, 1, 0))                 # reserved + type = icon
$all.Add([byte]($frames.Count % 256))
$all.Add([byte]0)
$offset = 6 + 16 * $frames.Count
foreach ($f in $frames) {
    $dim = if ($f.Size -ge 256) { 0 } else { $f.Size }
    $all.Add([byte]$dim)
    $all.Add([byte]$dim)
    $all.Add([byte]0)
    $all.Add([byte]0)
    $all.Add([byte]1); $all.Add([byte]0)            # planes = 1
    $all.Add([byte]($f.Bpp % 256)); $all.Add([byte]0)
    $sz = [BitConverter]::GetBytes([UInt32]$f.Data.Length)
    $all.AddRange($sz)
    $of = [BitConverter]::GetBytes([UInt32]$offset)
    $all.AddRange($of)
    $offset += $f.Data.Length
}
foreach ($f in $frames) { $all.AddRange([byte[]]$f.Data) }
[System.IO.File]::WriteAllBytes($icoPath, $all.ToArray())

"ok: $png"
"ok: $icoPath ($($all.Count) bytes; 16/32/48 en DIB, 256 en PNG)"
