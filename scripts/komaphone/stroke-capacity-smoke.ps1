# Maximum retained-sample capacity stress. This is a separate workload from the
# mandatory K5 house benchmark; its 33ms timing result is reported independently.
param([Parameter(Mandatory=$true)][string]$AppExe)
$ErrorActionPreference = 'Stop'
$capacityHarnessHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
. (Join-Path $PSScriptRoot 'k5-native-smoke.ps1') -AppExe $AppExe -FunctionsOnly
$script:k5Evidence.kind = 'stroke-capacity-native'
$script:k5Evidence.validation = 'maximum-capacity-correctness-and-observational-performance'
$script:k5Evidence.capacityHarnessSha256 = $capacityHarnessHash
$script:k5Evidence.artInput = 'Synthetic retained curves through existing engine commands; navigation uses actual egui middle-drag and wheel input.'
$script:k5Evidence.classification = 'Overall passed covers capacity correctness and recovery only. performance.passed independently reports the unchanged 33ms target for this maximum-capacity workload; it cannot replace the mandatory ordinary K5 house gate.'
$script:k5Evidence.performanceContended = $null
$script:k5Evidence.contentionNote = 'Run after build/test processes are idle. Process snapshots record observed presence and cumulative CPU, not host exclusivity or GPU utilization.'
$script:k5Evidence.fixture = @{
    curves=16; samplesPerCurve=8192; totalRetainedSamples=131072; width=5.0
    anchor=@{depth=0;x='0';y='0'}; color=@(0.05,0.09,0.13,1.0); erase=$false
    formula='For curve c=0..15 and sample i=0..8191: t=i/8191; x=round(-700+1400*t,6); y=round(180*sin(2*pi*t)+40*sin(6*pi*t)-30+4*c,6).'
    generatedVertexBudget=2097152; window=@(1920,1080); camera='root region and offset zero, zoom1'
}
$script:capacitySeedRecords = [System.Collections.Generic.List[object]]::new()

function Capacity-Command([string]$command,[hashtable]$params=@{}) {
    Send-K5 'engine.execute' @{command="komaphone.canvas.$command";params=$params} | Out-Null
    Idle-K5 | Out-Null
}

function Capacity-Origin {
    $state = Idle-K5
    Capacity-Command 'camera' @{camera=@{canvas=$state.active.id;region=@{depth=0;x='0';y='0'};offset=@('0','0');zoom=1.0}}
    Origin-K5 0 | Out-Null
}

function Capacity-Process {
    $process = $script:k5App.Process
    $process.Refresh()
    @{
        id=$process.Id; startUtc=$process.StartTime.ToUniversalTime().ToString('o'); cpuSeconds=$process.TotalProcessorTime.TotalSeconds
        workingSetBytes=$process.WorkingSet64; peakWorkingSetBytes=$process.PeakWorkingSet64
        privateMemoryBytes=$process.PrivateMemorySize64; virtualMemoryBytes=$process.VirtualMemorySize64
        pagedMemoryBytes=$process.PagedMemorySize64; handles=$process.HandleCount; threads=$process.Threads.Count
    }
}

function Capacity-HostSnapshot {
    $snapshot = @{at=[DateTime]::UtcNow.ToString('o');knownProcesses=(Processes-K5);ownedProcess=(Capacity-Process)}
    try {
        $os = Get-CimInstance -ClassName Win32_OperatingSystem
        $snapshot.hostMemory = @{
            totalVisibleBytes=([long]$os.TotalVisibleMemorySize*1024); freePhysicalBytes=([long]$os.FreePhysicalMemory*1024)
            totalVirtualBytes=([long]$os.TotalVirtualMemorySize*1024); freeVirtualBytes=([long]$os.FreeVirtualMemory*1024)
        }
        $snapshot.hostProcessors = @(Get-CimInstance -ClassName Win32_Processor | ForEach-Object {
            @{name=$_.Name;cores=$_.NumberOfCores;logicalProcessors=$_.NumberOfLogicalProcessors;instantaneousLoadPercent=$_.LoadPercentage}
        })
    } catch {
        $snapshot.hostInstrumentationError = $_.Exception.Message
    }
    return $snapshot
}

function Capacity-Assert([string]$label,[string]$expectedHash='') {
    $state = Idle-K5
    $view = View-K5
    if ($state.active.stroke_count -ne 16 -or @($view.strokes).Count -ne 16 -or $view.stats.points -ne 131072 -or
        $view.stats.full_strokes -ne 16 -or $view.stats.summary_strokes -ne 0 -or
        @($view.strokes | Where-Object {$_.samples -ne 8192}).Count -ne 0) {
        throw "Capacity fixture was not fully loaded for ${label}: expected 16 full strokes and exactly 131072 retained points."
    }
    if (@($view.strokes.id | Sort-Object -Unique).Count -ne 16) { throw 'Capacity fixture stroke identities are not distinct.' }
    if ($expectedHash -and $view.contentHash -ne $expectedHash) { throw "Canonical artwork changed during $label." }
    $ui = Wait-Ui-K5 {param($ui)
        if ($ui.canvases.message -like '*Cannot render*') { throw $ui.canvases.message }
        $ui.canvases.drawn_paths -eq 16 -and $ui.canvases.drawn_points -gt 0
    } "all capacity paths rendered for $label"
    if ($ui.canvases.drawn_points -gt 2097152) { throw 'Capacity renderer exceeded its generated vertex budget.' }
    Record-K5 $label @{state=$state;view=$view;ui=$ui;generatedVertices=$ui.canvases.drawn_points;generatedVertexBudget=2097152}
    return $view
}

function Capacity-MeasureFrames {
    $before = Idle-K5
    $rect = Drawing-K5
    $x = ($rect[0]+$rect[2])/2
    $y = ($rect[1]+$rect[3])/2
    $samples = @{}
    $afterFrame = -1
    $ui = $null
    $beforeSnapshot = Capacity-HostSnapshot
    $wall = [Diagnostics.Stopwatch]::StartNew()
    for ($round=0; $round -lt 72; $round++) {
        $direction = if ($round%2 -eq 0) {1} else {-1}
        Input-K5 'down' $x $y @{button='middle'}
        Input-K5 'move' ($x+4*$direction) $y
        Input-K5 'up' ($x+4*$direction) $y @{button='middle'}
        Input-K5 'wheel' $x $y @{deltaY=(4*$direction);unit='point'}
        $ui = Send-K5 'ui.inspect'
        if ($ui.canvases.message -like '*Cannot render*') { throw $ui.canvases.message }
        if (-not $ui.canvasFrameSamples) { throw 'Native CPU/frame interval instrumentation is unavailable.' }
        if ($round -lt 8) {
            $afterFrame = ($ui.canvasFrameSamples | Measure-Object -Property frame -Maximum).Maximum
        } else {
            foreach ($sample in $ui.canvasFrameSamples) {
                if ($sample.frame -gt $afterFrame) {
                    if ($null -eq $sample.cpu_ms -or [double]::IsNaN($sample.cpu_ms) -or [double]::IsInfinity($sample.cpu_ms) -or $sample.cpu_ms -lt 0) {
                        throw 'Invalid native CPU frame sample.'
                    }
                    $samples["$($sample.frame)"] = $sample
                }
            }
        }
    }
    $after = Idle-K5
    $wall.Stop()
    if ($after.active.stroke_count -ne $before.active.stroke_count) { throw 'Capacity navigation changed the stroke count.' }
    $raw = @($samples.Values | Sort-Object frame)
    if ($raw.Count -lt 100) { throw "Too few capacity CPU frame samples: $($raw.Count)." }
    $cpuTimes = @($raw.cpu_ms | Sort-Object)
    $intervals = @($raw | Where-Object {$null -ne $_.interval_ms} | ForEach-Object {
        $interval = [double]$_.interval_ms
        if ([double]::IsNaN($interval) -or [double]::IsInfinity($interval) -or $interval -le 0) { throw 'Invalid native frame interval.' }
        $interval
    } | Sort-Object)
    if ($intervals.Count -lt 100) { throw "Too few capacity frame intervals: $($intervals.Count)." }
    $p50 = $intervals[[Math]::Max(0,[Math]::Ceiling($intervals.Count*0.50)-1)]
    $p95 = $intervals[[Math]::Max(0,[Math]::Ceiling($intervals.Count*0.95)-1)]
    $afterSnapshot = Capacity-HostSnapshot
    $script:k5Evidence.performance = @{
        scope='Observed maximum-capacity stress, separate from the mandatory ordinary K5 house benchmark.'
        intervalScope='Frame-start intervals from consecutive native input timestamps; includes CPU, GPU/presentation waits, scheduling and automation gaps. Not input-to-display latency.'
        cpuScope='Previous eframe CPU duration includes UI/tessellation/encoding; excludes get_current_texture, queue.submit and present call durations. GPU execution completion is unmeasured.'
        workload='1920x1080; 16 smooth curves with 8192 retained samples each; 8 warmup then 64 measured rounds of actual middle-drag +/-4 pixels and wheel +/-4 points; no bookmark navigation.'
        adapter=$ui.perf.timings.gpuInfo; nativePerf=$ui.perf; window=$ui.window; canvas=$before.active.id
        strokeCount=16; retainedSampleCount=131072; generatedVertexBudget=2097152
        warmupRounds=8; measuredRounds=64; sampleCount=$raw.Count; intervalSampleCount=$intervals.Count
        p50_ms=$p50; p95_ms=$p95; max_ms=$intervals[-1]; target_p95_ms=33.0; passed=($p95 -le 33.0)
        cpu_p50_ms=$cpuTimes[[Math]::Max(0,[Math]::Ceiling($cpuTimes.Count*0.50)-1)]
        cpu_p95_ms=$cpuTimes[[Math]::Max(0,[Math]::Ceiling($cpuTimes.Count*0.95)-1)]; cpu_max_ms=$cpuTimes[-1]
        measurementWallSeconds=$wall.Elapsed.TotalSeconds
        ownedCpuDeltaSeconds=($afterSnapshot.ownedProcess.cpuSeconds-$beforeSnapshot.ownedProcess.cpuSeconds)
        snapshotsBefore=$beforeSnapshot; snapshotsAfter=$afterSnapshot; samples=$raw
    }
    if ($p95 -gt 33.0) {
        $script:k5Evidence.performanceLimitation = "Maximum-capacity native frame interval p95 ${p95}ms exceeds the unchanged 33ms target. This stress timing failed; correctness/recovery continues independently. The ordinary K5 house gate must still pass separately."
    }
}

try {
    Start-K5
    Resize-K5 1920 1080
    $script:k5Evidence.environmentBefore = Capacity-HostSnapshot
    Capacity-Command 'create' @{name='Maximum retained stroke capacity'}
    Capacity-Origin
    $canvas = (Idle-K5).active.id
    $seedClock = [Diagnostics.Stopwatch]::StartNew()
    for ($curve=0; $curve -lt 16; $curve++) {
        $points = [System.Collections.Generic.List[object]]::new()
        for ($sample=0; $sample -lt 8192; $sample++) {
            $t = [double]$sample/8191.0
            $x = [Math]::Round(-700.0+1400.0*$t,6)
            $y = [Math]::Round(180.0*[Math]::Sin(2.0*[Math]::PI*$t)+40.0*[Math]::Sin(6.0*[Math]::PI*$t)-30.0+4.0*$curve,6)
            $points.Add(@($x,$y))
        }
        Capacity-Command 'stroke' @{
            canvas=$canvas;anchor=@{depth=0;x='0';y='0'};points=$points.ToArray();width=5.0
            color=@{rgba=@(0.05,0.09,0.13,1.0)};erase=$false
        }
        $view = View-K5
        $script:capacitySeedRecords.Add(@{curve=$curve;strokeCount=@($view.strokes).Count;retainedPoints=$view.stats.points;contentHash=$view.contentHash})
    }
    $seedClock.Stop()
    $script:k5Evidence.seedingSeconds = $seedClock.Elapsed.TotalSeconds
    $initial = Capacity-Assert 'capacity-before-navigation'
    $script:k5Evidence.initialView = $initial
    $script:k5Evidence.drawingRect = Drawing-K5
    Screenshot-K5 'capacity-initial'
    Capacity-MeasureFrames
    $afterNavigation = Capacity-Assert 'capacity-after-navigation' $initial.contentHash
    Capacity-Origin
    $restored = Capacity-Assert 'capacity-restored-camera' $initial.contentHash
    if ((Json-K5 $initial.strokes) -ne (Json-K5 $restored.strokes)) { throw 'Navigation changed retained stroke identities or hashes.' }
    Screenshot-K5 'capacity-after-navigation'
    $script:k5Evidence.beforeRestart = Capacity-HostSnapshot
    Stop-K5 'capacity acknowledged-save recovery'
    Start-K5
    Open-K5 $canvas
    Capacity-Origin
    $reopened = Capacity-Assert 'capacity-reopened' $initial.contentHash
    if ((Json-K5 $initial.strokes) -ne (Json-K5 $reopened.strokes)) { throw 'Reopening changed retained stroke identities or hashes.' }
    Screenshot-K5 'capacity-reopened'
    $script:k5Evidence.afterNavigationView = $afterNavigation
    $script:k5Evidence.reopenedView = $reopened
    $script:k5Evidence.environmentAfter = Capacity-HostSnapshot
    $script:k5Evidence.correctnessPassed = $true
    $script:k5Evidence.passed = $true
} catch {
    $script:k5Evidence.error = $_.Exception.Message
    $script:k5Evidence.failureStack = $_.ScriptStackTrace
    $script:k5Evidence.correctnessPassed = $false
    $script:k5Evidence.passed = $false
    throw
} finally {
    try { Stop-K5 'capacity harness cleanup' } catch {
        $script:k5Evidence.cleanupError = $_.Exception.Message
        $script:k5Evidence.passed = $false
    }
    foreach ($key in $k5Original.Keys) { [Environment]::SetEnvironmentVariable($key,$k5Original[$key],'Process') }
    $script:k5Evidence.finished = [DateTime]::UtcNow.ToString('o')
    $script:k5Evidence.seedRecords = $script:capacitySeedRecords.ToArray()
    $script:k5Evidence.interactions = $script:k5Interactions.ToArray()
    $script:k5Evidence.snapshots = $script:k5Snapshots.ToArray()
    $script:k5Evidence.screenshots = $script:k5Screenshots.ToArray()
    $script:k5Evidence.visualReview = 'Captured native screenshots require separate inspection; path/vertex counts establish submitted geometry, not GPU completion.'
    $script:k5Evidence | ConvertTo-Json -Depth 65 | Set-Content -LiteralPath (Join-Path $k5Root 'result.json') -Encoding utf8
    Write-Output (Join-Path $k5Root 'result.json')
}
