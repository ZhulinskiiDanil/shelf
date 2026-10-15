# Generates assets/shelf.ico: a light rounded tile with blue capture-frame corners.
# Usage: powershell -ExecutionPolicy Bypass -File tools\make-icon.ps1
Add-Type -AssemblyName System.Drawing
$root = Split-Path $PSScriptRoot -Parent
$out = Join-Path $root "assets\shelf.ico"
New-Item -ItemType Directory -Force (Split-Path $out) | Out-Null

function RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $p = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = 2 * $r
    $p.AddArc($x, $y, $d, $d, 180, 90); $p.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $p.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90); $p.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $p.CloseFigure(); $p
}

function Render([int]$n) {
    $b = New-Object System.Drawing.Bitmap $n, $n, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($b)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $k = $n / 256.0
    # Tile: near-white with a soft vertical gradient and a hairline edge.
    $m = 12 * $k
    $tile = RoundRect $m $m ($n - 2 * $m) ($n - 2 * $m) (58 * $k)
    $grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.PointF 0, $m), (New-Object System.Drawing.PointF 0, ($n - $m)), ([System.Drawing.Color]::FromArgb(255, 255, 255, 255)), ([System.Drawing.Color]::FromArgb(255, 226, 232, 242))
    $g.FillPath($grad, $tile)
    $edge = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(70, 120, 130, 150)), ([Math]::Max(1, 4 * $k))
    $g.DrawPath($edge, $tile)
    # Capture frame: four blue corner brackets (thicker at small sizes so they stay legible).
    $t = [Math]::Max(1.6, 13 * $k * $(if ($n -le 24) { 1.7 } else { 1 }))
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 47, 140, 255)), $t
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round; $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $a = 66 * $k; $z = $n - 66 * $k; $l = 36 * $k
    foreach ($c in @(@($a, $a, 1, 1), @($z, $a, -1, 1), @($a, $z, 1, -1), @($z, $z, -1, -1))) {
        $x = $c[0]; $y = $c[1]; $dx = $c[2] * $l; $dy = $c[3] * $l
        $g.DrawLines($pen, [System.Drawing.PointF[]]@((New-Object System.Drawing.PointF ($x + $dx), $y), (New-Object System.Drawing.PointF $x, $y), (New-Object System.Drawing.PointF $x, ($y + $dy))))
    }
    # A small solid dot in the middle: the "shot".
    $r = [Math]::Max(1.5, 13 * $k)
    $dot = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 47, 140, 255))
    $g.FillEllipse($dot, $n / 2 - $r, $n / 2 - $r, 2 * $r, 2 * $r)
    $g.Dispose()
    $ms = New-Object System.IO.MemoryStream
    $b.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $b.Dispose()
    , $ms.ToArray()
}

$sizes = 16, 20, 24, 32, 40, 48, 64, 256
$pngs = foreach ($s in $sizes) { , (Render $s) }
$fs = [System.IO.File]::Create($out)
$w = New-Object System.IO.BinaryWriter $fs
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $s = $sizes[$i]; $len = $pngs[$i].Length
    $w.Write([byte]($s % 256)); $w.Write([byte]($s % 256)); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$len); $w.Write([uint32]$offset)
    $offset += $len
}
foreach ($p in $pngs) { $w.Write($p) }
$w.Close()
# Preview for review.
[System.IO.File]::WriteAllBytes((Join-Path $root "assets\shelf-256.png"), $pngs[-1])
"wrote $out"
