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

    # mango
    $handlePen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 0x28, 0x30, 0x3D)), (24 * $s)
    $handlePen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $handlePen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $g.DrawLine($handlePen, [float](150 * $s), [float](150 * $s), [float](197 * $s), [float](197 * $s))
    $handlePen.Dispose()

    # lente
    $cx = 104 * $s; $cy = 104 * $s; $r = (62 - 9) * $s
    $rect = New-Object System.Drawing.RectangleF ([float]($cx - $r)), ([float]($cy - $r)), ([float]($r * 2)), ([float]($r * 2))
    $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, ([System.Drawing.Color]::FromArgb(235, 0xFF, 0xC2, 0x4A)), ([System.Drawing.Color]::FromArgb(240, 0xC8, 0x5A, 0x08)), 45.0
    $g.FillEllipse($brush, $rect)
    $brush.Dispose()

    # anillo
    $ringPen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 0xEE, 0x8A, 0x16)), (18 * $s)
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

# ICO con 16/32/48/256 en PNG (soportado desde Vista)
$sizes = @(16, 32, 48, 256)
$payloads = @()
foreach ($sz in $sizes) {
    $b = New-IconBitmap $sz
    $ms = New-Object System.IO.MemoryStream
    $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $payloads += , ($ms.ToArray())
    $ms.Dispose(); $b.Dispose()
}

$icoPath = Join-Path $OutDir "icon.ico"
$fs = [System.IO.File]::Create($icoPath)
$bw = New-Object System.IO.BinaryWriter $fs
$bw.Write([UInt16]0)                 # reserved
$bw.Write([UInt16]1)                 # type: icon
$bw.Write([UInt16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $sz = $sizes[$i]
    $dim = if ($sz -ge 256) { 0 } else { $sz }
    $data = $payloads[$i]
    $bw.Write([byte]$dim)            # width
    $bw.Write([byte]$dim)            # height
    $bw.Write([byte]0)               # palette
    $bw.Write([byte]0)               # reserved
    $bw.Write([UInt16]1)             # planes
    $bw.Write([UInt16]32)            # bpp
    $bw.Write([UInt32]$data.Length)
    $bw.Write([UInt32]$offset)
    $offset += $data.Length
}
foreach ($data in $payloads) { $bw.Write($data) }
$bw.Flush(); $bw.Dispose(); $fs.Dispose()

"ok: $png"
"ok: $icoPath ($($sizes -join '/'))"
