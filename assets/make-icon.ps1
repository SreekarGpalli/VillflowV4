# Renders the VillFlow V4 app icon source (1024x1024 PNG). Regenerate all sizes with:
#   npx tauri icon assets/icon-source.png
Add-Type -AssemblyName System.Drawing
$S = 1024
$bmp = New-Object System.Drawing.Bitmap $S, $S
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = 'AntiAlias'; $g.TextRenderingHint = 'AntiAliasGridFit'; $g.InterpolationMode = 'HighQualityBicubic'
$g.Clear([System.Drawing.Color]::Transparent)

# Rounded square with a deep green gradient.
$r = 230; $rect = New-Object System.Drawing.RectangleF 32, 32, 960, 960
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$path.AddArc($rect.X, $rect.Y, $r, $r, 180, 90)
$path.AddArc($rect.Right - $r, $rect.Y, $r, $r, 270, 90)
$path.AddArc($rect.Right - $r, $rect.Bottom - $r, $r, $r, 0, 90)
$path.AddArc($rect.X, $rect.Bottom - $r, $r, $r, 90, 90)
$path.CloseFigure()
$grad = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, ([System.Drawing.Color]::FromArgb(255, 58, 122, 101)), ([System.Drawing.Color]::FromArgb(255, 30, 66, 56)), 60
$g.FillPath($grad, $path)

# "V4" wordmark.
$font = New-Object System.Drawing.Font "Segoe UI", 430, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
$fmt = New-Object System.Drawing.StringFormat; $fmt.Alignment = 'Center'; $fmt.LineAlignment = 'Center'
$white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 247, 245, 241))
$g.DrawString("V4", $font, $white, (New-Object System.Drawing.RectangleF 0, 20, $S, 760), $fmt)

# Voice wave underneath.
$pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(255, 156, 214, 188)), 46
$pen.StartCap = 'Round'; $pen.EndCap = 'Round'; $pen.LineJoin = 'Round'
$pts = @()
for ($i = 0; $i -le 120; $i++) {
  $x = 250 + $i * (524 / 120)
  $t = ($i / 120) * 2 * [Math]::PI * 1.5
  $amp = 58 * [Math]::Sin(($i / 120) * [Math]::PI)
  $pts += New-Object System.Drawing.PointF $x, (740 + $amp * [Math]::Sin($t))
}
$g.DrawCurve($pen, [System.Drawing.PointF[]]$pts)

$out = Join-Path $PSScriptRoot "icon-source.png"
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
"saved $out"
