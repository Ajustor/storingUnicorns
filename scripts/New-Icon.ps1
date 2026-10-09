<#
.SYNOPSIS
    Convert assets/icon.png to assets/icon.ico using built-in Windows APIs.
    No external tools required.

.DESCRIPTION
    Generates a multi-size ICO file (256x256, 128x128, 64x64, 48x48, 32x32, 16x16)
    from the PNG source image. The resulting icon.ico is used by the WiX installer
    and can be used anywhere you need an ICO file.

    256px is stored as embedded PNG (full quality, modern Windows).
    Smaller sizes are stored as 32-bit BMP DIB for maximum Windows compatibility,
    including Add/Remove Programs and legacy contexts.

.EXAMPLE
    .\scripts\New-Icon.ps1
    .\scripts\New-Icon.ps1 -Source "assets\icon.png" -Destination "assets\icon.ico"
#>

param(
    [string]$Source      = "$PSScriptRoot\..\assets\icon.png",
    [string]$Destination = "$PSScriptRoot\..\assets\icon.ico"
)

Add-Type -AssemblyName System.Drawing

$Source      = (Resolve-Path $Source).Path
$Destination = [System.IO.Path]::GetFullPath($Destination)

Write-Host "Source:      $Source"
Write-Host "Destination: $Destination"

# Sizes to embed in the ICO (largest first)
$sizes = @(256, 128, 64, 48, 32, 16)

# Load source PNG
$srcBitmap = [System.Drawing.Bitmap]::FromFile($Source)

$imageDataList = [System.Collections.Generic.List[byte[]]]::new()

foreach ($size in $sizes) {
    $resized = [System.Drawing.Bitmap]::new($size, $size,
        [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($resized)
    $g.InterpolationMode  = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.SmoothingMode      = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.PixelOffsetMode    = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)
    $g.DrawImage($srcBitmap, 0, 0, $size, $size)
    $g.Dispose()

    if ($size -ge 256) {
        # 256px: embed as PNG (ICO format supports this, best quality)
        $ms = [System.IO.MemoryStream]::new()
        $resized.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
        $imageDataList.Add($ms.ToArray())
        $ms.Dispose()
    } else {
        # Smaller sizes: 32-bit BMP DIB for maximum Windows compatibility.
        # ICO format uses a DIB without the BITMAPFILEHEADER, with pixel rows
        # stored bottom-to-top and an AND mask appended after the pixel data.
        $andMaskRowBytes = [int]([Math]::Ceiling($size / 32.0)) * 4
        $andMaskBytes    = $andMaskRowBytes * $size
        $pixelBytes      = $size * $size * 4  # 32-bit BGRA

        $ms  = [System.IO.MemoryStream]::new()
        $bw2 = [System.IO.BinaryWriter]::new($ms)

        # BITMAPINFOHEADER (40 bytes)
        $bw2.Write([int32]40)           # biSize
        $bw2.Write([int32]$size)        # biWidth
        $bw2.Write([int32]($size * 2))  # biHeight (x2 to include AND mask)
        $bw2.Write([uint16]1)           # biPlanes
        $bw2.Write([uint16]32)          # biBitCount
        $bw2.Write([int32]0)            # biCompression (BI_RGB)
        $bw2.Write([int32]0)            # biSizeImage
        $bw2.Write([int32]0)            # biXPelsPerMeter
        $bw2.Write([int32]0)            # biYPelsPerMeter
        $bw2.Write([int32]0)            # biClrUsed
        $bw2.Write([int32]0)            # biClrImportant

        # Pixel data: bottom-to-top rows, BGRA byte order
        for ($row = $size - 1; $row -ge 0; $row--) {
            for ($col = 0; $col -lt $size; $col++) {
                $pixel = $resized.GetPixel($col, $row)
                $bw2.Write([byte]$pixel.B)
                $bw2.Write([byte]$pixel.G)
                $bw2.Write([byte]$pixel.R)
                $bw2.Write([byte]$pixel.A)
            }
        }

        # AND mask: all zeros (transparency is driven by the 32-bit alpha channel)
        $bw2.Write([byte[]]([byte[]]::new($andMaskBytes)))

        $bw2.Flush()
        $imageDataList.Add($ms.ToArray())
        $bw2.Close()
        $ms.Dispose()
    }

    $resized.Dispose()
}

$srcBitmap.Dispose()

# Write ICO file
$fs = [System.IO.FileStream]::new($Destination, [System.IO.FileMode]::Create)
$bw = [System.IO.BinaryWriter]::new($fs)

$count = $sizes.Count

# ICONDIR header (6 bytes)
$bw.Write([uint16]0)       # reserved
$bw.Write([uint16]1)       # type: 1 = icon
$bw.Write([uint16]$count)

# Calculate initial data offset: 6 (ICONDIR) + 16 * count (ICONDIRENTRYs)
$dataOffset = 6 + 16 * $count

# ICONDIRENTRY array (16 bytes each)
for ($i = 0; $i -lt $count; $i++) {
    $sz   = $sizes[$i]
    $data = $imageDataList[$i]

    $bw.Write([byte]($sz -band 0xFF))   # width  (0 means 256)
    $bw.Write([byte]($sz -band 0xFF))   # height (0 means 256)
    $bw.Write([byte]0)                  # color count (0 = true color)
    $bw.Write([byte]0)                  # reserved
    $bw.Write([uint16]1)                # color planes
    $bw.Write([uint16]32)               # bits per pixel
    $bw.Write([uint32]$data.Length)     # size of image data
    $bw.Write([uint32]$dataOffset)      # offset of image data

    $dataOffset += $data.Length
}

# Image data
foreach ($data in $imageDataList) {
    $bw.Write($data)
}

$bw.Close()
$fs.Close()

Write-Host "Done — icon written to: $Destination"
