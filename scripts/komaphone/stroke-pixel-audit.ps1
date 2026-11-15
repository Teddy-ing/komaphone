# Independent native-image oracle for the synthetic stroke comparison fixture.
# This uses only point-to-segment distance and chronological opaque brush/erase
# coverage. It does not call or reproduce the app's tessellator.
[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$ResultPath,
    [switch]$ExpectFailure
)
$ErrorActionPreference = 'Stop'
$resultFile = (Resolve-Path -LiteralPath $ResultPath).Path
$folder = Split-Path -Parent $resultFile
$result = Get-Content -LiteralPath $resultFile -Raw | ConvertFrom-Json
if ($result.kind -ne 'stroke-quality-native') { throw 'Expected a stroke-quality-native result.' }
if (@($result.fixture).Count -eq 0) { throw 'The saved comparison fixture is missing.' }
if (@($result.drawingRect).Count -ne 4) { throw 'The logical canvas drawing rectangle is missing.' }

Add-Type -AssemblyName System.Drawing
if (-not ('KomaphoneStrokePixelAuditV1' -as [type])) {
    Add-Type -ReferencedAssemblies System.Drawing.dll -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;

public sealed class KomaphoneAuditStrokeV1 {
    public double[] Coordinates;
    public double Width;
    public bool Erase;
}

public sealed class KomaphonePixelMismatchV1 {
    public int X;
    public int Y;
    public bool ExpectedInk;
    public int Red;
    public int Green;
    public int Blue;
    public double NearestBoundaryPixels;
}

public sealed class KomaphonePixelCountsV1 {
    public int ImageWidth;
    public int ImageHeight;
    public double PhysicalScale;
    public long Compared;
    public long BoundaryExcluded;
    public long ExpectedInk;
    public long ExpectedPaper;
    public long ExtraInk;
    public long MissingInk;
    public long AmbiguousTone;
    public int[] ErrorBounds;
    public List<KomaphonePixelMismatchV1> FirstMismatches = new List<KomaphonePixelMismatchV1>();
}

public static class KomaphoneStrokePixelAuditV1 {
    private static double Distance(double x, double y, double[] coordinates) {
        double bestSquared = Double.PositiveInfinity;
        for (int i = 0; i < coordinates.Length; i += 2) {
            double dx = x - coordinates[i];
            double dy = y - coordinates[i + 1];
            bestSquared = Math.Min(bestSquared, dx * dx + dy * dy);
        }
        for (int i = 2; i < coordinates.Length; i += 2) {
            double ax = coordinates[i - 2];
            double ay = coordinates[i - 1];
            double dx = coordinates[i] - ax;
            double dy = coordinates[i + 1] - ay;
            double lengthSquared = dx * dx + dy * dy;
            if (lengthSquared == 0.0) continue;
            double t = Math.Max(0.0, Math.Min(1.0, ((x - ax) * dx + (y - ay) * dy) / lengthSquared));
            double residualX = x - ax - t * dx;
            double residualY = y - ay - t * dy;
            bestSquared = Math.Min(bestSquared, residualX * residualX + residualY * residualY);
        }
        return Math.Sqrt(bestSquared);
    }

    public static KomaphonePixelCountsV1 Compare(
        string imagePath, KomaphoneAuditStrokeV1[] strokes,
        double uiWidth, double uiHeight, double[] rect,
        double viewportWidth, double viewportHeight, double zoom, double boundaryBand) {
        var result = new KomaphonePixelCountsV1();
        using (var loaded = new Bitmap(imagePath))
        using (var bitmap = loaded.Clone(new Rectangle(0, 0, loaded.Width, loaded.Height), PixelFormat.Format32bppArgb)) {
            result.ImageWidth = bitmap.Width;
            result.ImageHeight = bitmap.Height;
            double scale = bitmap.Width / uiWidth;
            if (Math.Abs(bitmap.Height / uiHeight - scale) > 0.000001)
                throw new ArgumentException("Screenshot dimensions do not have a uniform logical-to-physical scale.");
            result.PhysicalScale = scale;
            double originX = (rect[0] + viewportWidth * 0.5) * scale;
            double originY = (rect[1] + viewportHeight * 0.5) * scale;
            double worldToPixel = zoom * scale;
            var data = bitmap.LockBits(new Rectangle(0, 0, bitmap.Width, bitmap.Height), ImageLockMode.ReadOnly, PixelFormat.Format32bppArgb);
            try {
                int rowBytes = checked(bitmap.Width * 4);
                byte[] row = new byte[rowBytes];
                int minX = Math.Max(0, (int)Math.Ceiling(rect[0] * scale + boundaryBand));
                int maxX = Math.Min(bitmap.Width, (int)Math.Floor(rect[2] * scale - boundaryBand));
                int minY = Math.Max(0, (int)Math.Ceiling(rect[1] * scale + boundaryBand));
                int maxY = Math.Min(bitmap.Height, (int)Math.Floor(rect[3] * scale - boundaryBand));
                int errorMinX = bitmap.Width, errorMinY = bitmap.Height, errorMaxX = -1, errorMaxY = -1;
                for (int y = minY; y < maxY; y++) {
                    Marshal.Copy(IntPtr.Add(data.Scan0, checked(y * data.Stride)), row, 0, rowBytes);
                    for (int x = minX; x < maxX; x++) {
                        double worldX = (x + 0.5 - originX) / worldToPixel;
                        double worldY = (y + 0.5 - originY) / worldToPixel;
                        double nearestBoundary = Double.PositiveInfinity;
                        bool expectedInk = false;
                        foreach (var stroke in strokes) {
                            double signedDistance = Distance(worldX, worldY, stroke.Coordinates) - stroke.Width * 0.5;
                            nearestBoundary = Math.Min(nearestBoundary, Math.Abs(signedDistance) * worldToPixel);
                            if (signedDistance <= 0.0) expectedInk = !stroke.Erase;
                        }
                        if (nearestBoundary < boundaryBand) {
                            result.BoundaryExcluded++;
                            continue;
                        }
                        result.Compared++;
                        if (expectedInk) result.ExpectedInk++; else result.ExpectedPaper++;
                        int byteOffset = x * 4;
                        int blue = row[byteOffset], green = row[byteOffset + 1], red = row[byteOffset + 2];
                        bool dark = red < 128 && green < 128 && blue < 128;
                        bool white = red > 240 && green > 240 && blue > 240;
                        bool mismatch = false;
                        if (expectedInk && white) { result.MissingInk++; mismatch = true; }
                        else if (!expectedInk && dark) { result.ExtraInk++; mismatch = true; }
                        else if (!dark && !white) { result.AmbiguousTone++; mismatch = true; }
                        if (mismatch) {
                            errorMinX = Math.Min(errorMinX, x); errorMaxX = Math.Max(errorMaxX, x);
                            errorMinY = Math.Min(errorMinY, y); errorMaxY = Math.Max(errorMaxY, y);
                            if (result.FirstMismatches.Count < 20) {
                                result.FirstMismatches.Add(new KomaphonePixelMismatchV1 {
                                    X = x, Y = y, ExpectedInk = expectedInk, Red = red, Green = green, Blue = blue,
                                    NearestBoundaryPixels = nearestBoundary
                                });
                            }
                        }
                    }
                }
                if (errorMaxX >= 0) result.ErrorBounds = new int[] { errorMinX, errorMinY, errorMaxX, errorMaxY };
            } finally {
                bitmap.UnlockBits(data);
            }
        }
        return result;
    }
}
'@
}

$strokes = [System.Collections.Generic.List[KomaphoneAuditStrokeV1]]::new()
foreach ($fixture in $result.fixture) {
    if (@($fixture.points).Count -eq 0 -or -not ([double]$fixture.width -gt 0) -or [double]::IsInfinity([double]$fixture.width)) { throw 'Invalid brush fixture.' }
    $stroke = [KomaphoneAuditStrokeV1]::new()
    $coordinates = [System.Collections.Generic.List[double]]::new()
    foreach ($point in $fixture.points) {
        if (@($point).Count -ne 2) { throw 'Invalid point in brush fixture.' }
        foreach ($axis in $point) {
            $value = [double]$axis
            if ([double]::IsNaN($value) -or [double]::IsInfinity($value)) { throw 'Non-finite fixture coordinate.' }
            $coordinates.Add($value)
        }
    }
    $stroke.Coordinates = $coordinates.ToArray()
    $stroke.Width = [double]$fixture.width
    $stroke.Erase = [bool]$fixture.erase
    $strokes.Add($stroke)
}

$rows = [System.Collections.Generic.List[object]]::new()
foreach ($zoom in @(8,16)) {
    $viewName = "zoom-$zoom"
    $view = $result.views.$viewName
    if (-not $view) { throw "Missing $viewName view." }
    $camera = $view.viewport.camera
    if ($camera.region.depth -ne 0 -or $camera.region.x -ne '0' -or $camera.region.y -ne '0' -or
        $camera.offset[0] -ne '0' -or $camera.offset[1] -ne '0' -or $camera.zoom -ne $zoom) {
        throw "The independent fixture oracle requires an unshifted root camera at $viewName."
    }
    if ([Math]::Abs(($result.drawingRect[2] - $result.drawingRect[0]) - $view.viewport.width) -gt 0.01 -or
        [Math]::Abs(($result.drawingRect[3] - $result.drawingRect[1]) - $view.viewport.height) -gt 0.01) {
        throw 'Drawing rectangle and viewport size do not agree.'
    }
    $screenshots = @($result.screenshots | Where-Object { [IO.Path]::GetFileName($_.path) -eq "stroke-$viewName.png" })
    if ($screenshots.Count -ne 1) { throw "Expected one actual native screenshot for $viewName." }
    $screenshot = $screenshots[0]
    $imagePath = (Resolve-Path -LiteralPath $screenshot.path).Path
    $imageHash = (Get-FileHash -LiteralPath $imagePath -Algorithm SHA256).Hash
    if ($imageHash -ne $screenshot.sha256) { throw "Screenshot hash changed: $imagePath" }
    $ui = $screenshot.ui
    if ($ui.window) { $ui = $ui.window }
    if (-not ($ui.width -gt 0) -or -not ($ui.height -gt 0)) { throw 'Missing logical screenshot dimensions.' }
    $counts = [KomaphoneStrokePixelAuditV1]::Compare(
        $imagePath, $strokes.ToArray(), [double]$ui.width, [double]$ui.height,
        [double[]]$result.drawingRect, [double]$view.viewport.width, [double]$view.viewport.height,
        [double]$zoom, 1.5
    )
    if ($counts.Compared -eq 0 -or $counts.ExpectedInk -eq 0 -or $counts.ExpectedPaper -eq 0) { throw 'The oracle did not exercise both brush and paper pixels.' }
    $rows.Add([ordered]@{
        view=$viewName; image=$imagePath; imageSha256=$imageHash
        passed=($counts.ExtraInk + $counts.MissingInk + $counts.AmbiguousTone -eq 0)
        counts=$counts
    })
}

$passed = @($rows | Where-Object { -not $_.passed }).Count -eq 0
$expectationSatisfied = if ($ExpectFailure) { -not $passed } else { $passed }
$audit = [ordered]@{
    kind='independent-native-stroke-pixel-audit'; version=1
    inputResult=$resultFile; inputResultSha256=(Get-FileHash -LiteralPath $resultFile -Algorithm SHA256).Hash
    scriptSha256=(Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
    appSha256=$result.appSha256; recordedAt=[DateTime]::UtcNow.ToString('o')
    oracle='Euclidean distance to retained point/segment union; chronological opaque brush and paper erasure'
    boundaryExclusionPhysicalPixels=1.5
    darkThreshold='all RGB channels <128'; whiteThreshold='all RGB channels >240'
    acceptance='Zero extra, missing, or ambiguous-tone pixels outside the boundary exclusion band in both zoom views'
    limitations=@('Synthetic opaque dark brush and white-paper eraser only.', 'Unshifted root camera only; tests zoom 8 and zoom 16.', 'Boundary feathering, color fidelity, and GPU frame completion are not established by this pixel oracle.')
    expectedFailure=[bool]$ExpectFailure; expectationSatisfied=$expectationSatisfied; passed=$passed
    views=$rows.ToArray()
}
$outputPath = Join-Path $folder 'pixel-audit.json'
$audit | ConvertTo-Json -Depth 14 | Set-Content -LiteralPath $outputPath -Encoding utf8
Write-Output $outputPath
foreach ($row in $rows) {
    Write-Output ("{0}: extra={1}, missing={2}, ambiguous={3}, compared={4}, excluded={5}" -f
        $row.view,$row.counts.ExtraInk,$row.counts.MissingInk,$row.counts.AmbiguousTone,$row.counts.Compared,$row.counts.BoundaryExcluded)
}
if (-not $expectationSatisfied) {
    if ($ExpectFailure) { throw 'The expected-bad baseline unexpectedly passed the independent pixel oracle.' }
    throw 'Native brush pixels failed the independent round-stroke oracle; see pixel-audit.json.'
}
