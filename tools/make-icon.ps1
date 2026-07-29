# Generates the Verba mark: an open ring with a hue sweep running
# pink -> violet -> indigo -> orange, starting at 210 degrees.
#
# From the design's component sheet: "open ring, stroke = 6% of diameter, hue
# sweep pink->violet->indigo->orange starting 210 deg. Original mark - no
# borrowed glyphs."
#
# GDI+ has no conic gradient, so the sweep is drawn as many thin arc segments
# with interpolated colour. Run from the repo root:
#   powershell -ExecutionPolicy Bypass -File tools/make-icon.ps1

Add-Type -AssemblyName System.Drawing

$size = 256
$out  = Join-Path $PSScriptRoot '..\src-tauri\icons'
New-Item -ItemType Directory -Force -Path $out | Out-Null

# Gradient stops as (position, R, G, B). sRGB approximations of the design's
# oklch accents: pink 80% .14 350, violet 72% .13 300, indigo 62% .13 275,
# orange 82% .13 60.
$stops = @(
  @(0.00, 255, 160, 208),
  @(0.30, 196, 137, 232),
  @(0.55, 125, 130, 217),
  @(0.82, 240, 180, 120),
  @(1.00, 255, 160, 208)
)

function Get-SweepColor([double]$t) {
  for ($i = 0; $i -lt $stops.Count - 1; $i++) {
    $a = $stops[$i]; $b = $stops[$i + 1]
    if ($t -ge $a[0] -and $t -le $b[0]) {
      $span = $b[0] - $a[0]
      $f = if ($span -eq 0) { 0 } else { ($t - $a[0]) / $span }
      return [System.Drawing.Color]::FromArgb(255,
        [int]($a[1] + ($b[1] - $a[1]) * $f),
        [int]($a[2] + ($b[2] - $a[2]) * $f),
        [int]($a[3] + ($b[3] - $a[3]) * $f))
    }
  }
  return [System.Drawing.Color]::FromArgb(255, 255, 160, 208)
}

# Use ::new() rather than `New-Object Type(...)`. PowerShell misparses the
# latter when the argument list contains arithmetic, silently handing the whole
# parenthesised group across as one array.
$bmp = [System.Drawing.Bitmap]::new($size, $size)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = 'AntiAlias'
$g.Clear([System.Drawing.Color]::Transparent)

# Ring geometry. The mask in the mockup leaves the ring occupying roughly the
# outer 38% of the radius, which reads much heavier than a hairline.
$pad       = $size * 0.06
$outer     = $size - 2 * $pad
$thickness = $size * 0.15
$edge      = [single]($pad + $thickness / 2)
$span      = [single]($outer - $thickness)
$rect      = [System.Drawing.RectangleF]::new($edge, $edge, $span, $span)

# Overlap each segment slightly or antialiasing leaves seams between them.
$steps = 720
$sweep = [single](360.0 / $steps + 0.8)
for ($i = 0; $i -lt $steps; $i++) {
  $t = $i / $steps
  $pen = [System.Drawing.Pen]::new((Get-SweepColor $t), [single]$thickness)
  $pen.StartCap = 'Round'; $pen.EndCap = 'Round'
  $g.DrawArc($pen, $rect, [single](210 + $t * 360), $sweep)
  $pen.Dispose()
}

$g.Dispose()
$png = Join-Path $out 'icon.png'
$bmp.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)

# Wrap the PNG in an ICO container. Vista and later accept PNG-compressed
# entries directly, so no bitmap re-encode is needed. A width byte of 0 means 256.
$pngBytes = [System.IO.File]::ReadAllBytes($png)
$ico = [System.IO.MemoryStream]::new()
$w = [System.IO.BinaryWriter]::new($ico)
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]1)   # ICONDIR
$w.Write([byte]0); $w.Write([byte]0)                            # 256 x 256
$w.Write([byte]0); $w.Write([byte]0)                            # palette, reserved
$w.Write([uint16]1); $w.Write([uint16]32)                       # planes, bpp
$w.Write([uint32]$pngBytes.Length)
$w.Write([uint32]22)                                            # offset past header
$w.Write($pngBytes)
$w.Flush()
[System.IO.File]::WriteAllBytes((Join-Path $out 'icon.ico'), $ico.ToArray())
$w.Dispose(); $bmp.Dispose()

# Tauri also wants square PNGs at fixed sizes for non-Windows bundles.
foreach ($s in 32, 128) {
  $src = [System.Drawing.Image]::FromFile($png)
  $r = [System.Drawing.Bitmap]::new($s, $s)
  $rg = [System.Drawing.Graphics]::FromImage($r)
  $rg.InterpolationMode = 'HighQualityBicubic'
  $rg.DrawImage($src, 0, 0, $s, $s)
  $rg.Dispose()
  $r.Save((Join-Path $out "${s}x${s}.png"), [System.Drawing.Imaging.ImageFormat]::Png)
  $r.Dispose(); $src.Dispose()
}

Get-ChildItem $out | Select-Object Name, Length
