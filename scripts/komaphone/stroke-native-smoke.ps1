# Compare the actual native renderer with identical saved, synthetic artwork.
# Seed commands deliberately bypass hand variability; K5's separate smoke covers
# pointer gestures, recovery, undo/redo and frame timing through the real UI.
param(
    [Parameter(Mandatory=$true)][string]$AppExe,
    [string]$SeedDirectory,
    [string]$BaselineResult
)
$ErrorActionPreference = 'Stop'
$strokeHarnessHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
. (Join-Path $PSScriptRoot 'k5-native-smoke.ps1') -AppExe $AppExe -FunctionsOnly
$script:k5Evidence.kind = 'stroke-quality-native'
$script:k5Evidence.strokeHarnessSha256 = $strokeHarnessHash
$script:k5Evidence.artInput = 'synthetic seed through existing engine commands; identical persisted samples across renderer comparison'
$script:k5Evidence.performanceContended = $true
$script:k5Evidence.contentionNote = 'Visual and persistence comparison only. Final ordinary K5 smoke measures frame timing separately.'
$script:strokeFixture=[System.Collections.Generic.List[object]]::new()

function Command-Stroke([string]$command,[hashtable]$params=@{}) {
    Send-K5 'engine.execute' @{command="komaphone.canvas.$command";params=$params} | Out-Null
    Idle-K5 | Out-Null
}
function Seed-Stroke($points,[double]$width,[bool]$erase=$false) {
    $state=Idle-K5
    Command-Stroke 'stroke' @{canvas=$state.active.id;anchor=@{depth=0;x='0';y='0'};points=$points;width=$width;color=@{rgba=@(0.05,0.09,0.13,1.0)};erase=$erase}
    $script:strokeFixture.Add(@{points=$points;width=$width;erase=$erase})
}
function Camera-Stroke([double]$zoom) {
    $state=Idle-K5
    $camera=@{canvas=$state.active.id;region=@{depth=0;x='0';y='0'};offset=@('0','0');zoom=$zoom}
    Command-Stroke 'camera' @{camera=$camera}
    Wait-K5 {param($s) $s.camera.zoom -eq $zoom -and -not $s.pending} 'exact comparison camera' | Out-Null
}
try {
    if ($SeedDirectory) {
        $source=(Resolve-Path -LiteralPath $SeedDirectory).Path
        if (-not (Test-Path -LiteralPath (Join-Path $source 'canvases.sqlite3'))) {throw 'Synthetic seed database missing'}
        Copy-Item -LiteralPath $source -Destination (Join-Path $k5Root 'data') -Recurse
        $script:k5Evidence.seed=$source
    }
    Start-K5
    Resize-K5 1280 900
    if (-not $SeedDirectory) {
        Command-Stroke 'create' @{name='Round brush comparison'}
        # Integer mouse steps, a reversal, repeated samples, and round dots.
        Seed-Stroke @(@(-25,-10),@(-24,-10),@(-24,-9),@(-23,-9),@(-23,-8),@(-22,-8),@(-22,-7)) 6
        Seed-Stroke @(@(-25,3),@(-22,3),@(-22,3),@(-24,3),@(-21,5)) 5
        Seed-Stroke (,@(-23,13)) 4
        # Rounded sampled circle. Quantized direction changes stress thick joins.
        $circle=[System.Collections.Generic.List[object]]::new()
        for($i=0;$i -le 100;$i++) {
            $angle=2*[Math]::PI*$i/100
            $circle.Add(@((10+[Math]::Round(12*[Math]::Cos($angle)*2)/2),([Math]::Round(12*[Math]::Sin($angle)*2)/2)))
        }
        Seed-Stroke $circle.ToArray() 4
        Seed-Stroke @(@(6,1),@(8,3),@(8,3),@(12,3),@(14,1)) 2
        Seed-Stroke @(@(3,-5),@(4,-5)) 2
        Seed-Stroke @(@(16,-5),@(17,-5)) 2
        Seed-Stroke @(@(-26,21),@(-18,21)) 3
        Seed-Stroke @(@(-23,19),@(-22,20),@(-22,21),@(-21,22)) 1.2 $true
        Seed-Stroke @(@(-8,20),@(18,20)) 0.125
    }
    $views=[ordered]@{}
    foreach($zoom in @(1,8,16)) {
        Camera-Stroke $zoom
        $view=View-K5
        $views["zoom-$zoom"]=$view
        Screenshot-K5 "stroke-zoom-$zoom"
        $ui=Send-K5 'ui.inspect'
        if ($ui.canvases.message -like '*Cannot render*') {throw $ui.canvases.message}
    }
    if ($BaselineResult) {
        $baseline=Get-Content -LiteralPath $BaselineResult -Raw | ConvertFrom-Json
        $script:k5Evidence.fixture=$baseline.fixture
        foreach($name in $views.Keys) {
            if($views[$name].contentHash -ne $baseline.views.$name.contentHash) {throw "Saved stroke data changed at $name"}
        }
        $script:k5Evidence.identicalSavedArtwork=$true
    } else {
        $script:k5Evidence.fixture=$script:strokeFixture.ToArray()
    }
    $script:k5Evidence.views=$views
    $script:k5Evidence.drawingRect=Drawing-K5
    $script:k5Evidence.passed=$true
} catch {
    $script:k5Evidence.error=$_.Exception.Message
    $script:k5Evidence.failureStack=$_.ScriptStackTrace
    throw
} finally {
    try {Stop-K5 'stroke comparison cleanup'} catch {$script:k5Evidence.cleanupError=$_.Exception.Message;$script:k5Evidence.passed=$false}
    foreach($key in $k5Original.Keys){[Environment]::SetEnvironmentVariable($key,$k5Original[$key],'Process')}
    $script:k5Evidence.finished=[DateTime]::UtcNow.ToString('o')
    $script:k5Evidence.screenshots=$script:k5Screenshots.ToArray()
    $script:k5Evidence.visualReview='Captured; requires actual image inspection'
    $script:k5Evidence | ConvertTo-Json -Depth 65 | Set-Content -LiteralPath (Join-Path $k5Root 'result.json') -Encoding utf8
    Write-Output (Join-Path $k5Root 'result.json')
}
