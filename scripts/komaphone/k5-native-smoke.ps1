# Real Windows/egui K5 acceptance. All art is synthetic and every mutation of art,
# names, bookmarks and navigation uses observed UI controls or mouse events.
# The engine seam is used only to inspect durable status and canonical view hashes.
param(
    [Parameter(Mandatory=$true)][string]$AppExe,
    [switch]$Diagnostic,
    [switch]$FunctionsOnly
)
$ErrorActionPreference = 'Stop'
$k5Repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$AppExe = (Resolve-Path -LiteralPath $AppExe).Path
$k5Hash = (Get-FileHash -LiteralPath $AppExe -Algorithm SHA256).Hash
$k5HarnessHash = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
$k5Root = Join-Path $k5Repo ('log/k5/native-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $k5Root | Out-Null
$k5Original = @{}
foreach ($key in @('KOMAPHONE_DATA_DIR','PHOTOCRAFT_CONFIG_DIR','KOMAPHONE_SERVICE_ENDPOINT','KOMAPHONE_TRUSTED_CERT')) { $k5Original[$key] = [Environment]::GetEnvironmentVariable($key,'Process') }
$script:k5App = $null
$script:k5Generation = 0
$script:k5Interactions = [System.Collections.Generic.List[object]]::new()
$script:k5Screenshots = [System.Collections.Generic.List[object]]::new()
$script:k5Snapshots = [System.Collections.Generic.List[object]]::new()
$script:k5Evidence = [ordered]@{ validation = $(if ($Diagnostic) {'diagnostic'} else {'acceptance-candidate'}); app = $AppExe; appSha256 = $k5Hash; started = [DateTime]::UtcNow.ToString('o'); root = $k5Root; artInput = 'actual egui screen-coordinate down/move/up'; network = 'authenticated loopback control only; no service'; passed = $false }
$script:k5Evidence.harnessSha256=$k5HarnessHash
$script:k5Evidence.inputIntegrityVersion=1
$script:k5Evidence.performanceContended=[bool]$Diagnostic
$script:k5Evidence.contentionNote='Diagnostic timing may overlap compilation and GPU tests; process snapshots show presence and cumulative CPU only, not complete host/GPU utilization. Final acceptance requires the ordinary 33ms gate.'

function Json-K5($value) { ConvertTo-Json -InputObject $value -Depth 45 -Compress }
function Processes-K5 {
    @{at=[DateTime]::UtcNow.ToString('o');harnessPid=$PID;logicalProcessors=$env:NUMBER_OF_PROCESSORS;os=[Environment]::OSVersion.VersionString
      scope='Known compiler/build/test/app process names, PIDs and cumulative CPU/memory only; no command lines, credentials or environment dump; no host exclusivity'
      processes=@(Get-Process | Where-Object {$_.ProcessName -match '^(cargo|rustc|xtask|cl|link|lld|cmake|ninja|msbuild)$|^(photocraft|snapshot|engine_it)'} | ForEach-Object {
          @{name=$_.ProcessName;id=$_.Id;cpuSeconds=$_.CPU;workingSetBytes=$_.WorkingSet64}
      })}
}
function Record-K5([string]$name,$state) {
    $script:k5Snapshots.Add(@{name=$name;state=$state;at=[DateTime]::UtcNow.ToString('o')})
}
function Port-K5 {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback,0)
    $listener.Start(); $port = $listener.LocalEndpoint.Port; $listener.Stop(); return $port
}
function Connect-K5 {
    if ($script:k5App.Client) { $script:k5App.Client.Dispose(); $script:k5App.Client=$null }
    $client = [System.Net.Sockets.TcpClient]::new()
    try {
    $client.Connect('127.0.0.1',$script:k5App.Port)
    $stream = $client.GetStream(); $stream.ReadTimeout = 30000
    $writer = [System.IO.StreamWriter]::new($stream,[System.Text.UTF8Encoding]::new($false)); $writer.AutoFlush = $true
    $reader = [System.IO.StreamReader]::new($stream)
    $writer.WriteLine((Json-K5 @{id=[guid]::NewGuid().ToString('N');method='auth';params=@{token=(Get-Content -LiteralPath $script:k5App.Token -Raw).Trim()}}))
    $reply = $reader.ReadLine() | ConvertFrom-Json
    if (-not $reply.ok) { $client.Dispose(); throw 'K5 isolated control authentication failed' }
    $script:k5App.Client = $client; $script:k5App.Writer = $writer; $script:k5App.Reader = $reader; $script:k5App.LastUse = [DateTime]::UtcNow
    } catch { $client.Dispose(); throw }
}
function Send-K5([string]$method,[hashtable]$params=@{}) {
    if (-not $script:k5App -or $script:k5App.Process.HasExited) { throw "App unavailable for $method" }
    if (-not $script:k5App.Client -or ([DateTime]::UtcNow-$script:k5App.LastUse).TotalSeconds -gt 20) { Connect-K5 }
    $request = @{id=[guid]::NewGuid().ToString('N');method=$method;params=$params}
    $script:k5App.Writer.WriteLine((Json-K5 $request))
    $line = $script:k5App.Reader.ReadLine()
    if (-not $line) { throw "Control closed during $method; request may have completed" }
    $reply = $line | ConvertFrom-Json; $script:k5App.LastUse = [DateTime]::UtcNow
    if (-not $reply.ok) { throw "K5 $method refused: $($reply.error)" }
    return $reply.result
}
function Status-K5 { Send-K5 'engine.execute' @{command='komaphone.canvas.status';params=@{}} }
function View-K5 { Send-K5 'engine.execute' @{command='komaphone.canvas.view';params=@{}} }
function Start-K5 {
    if ($script:k5App -and -not $script:k5App.Process.HasExited) { throw 'Do not launch a competing K5 app' }
    if ((Get-FileHash -LiteralPath $AppExe -Algorithm SHA256).Hash -ne $k5Hash) { throw 'App artifact changed during acceptance' }
    $script:k5Generation++
    $env:KOMAPHONE_DATA_DIR = Join-Path $k5Root 'data'
    $env:PHOTOCRAFT_CONFIG_DIR = Join-Path $k5Root 'config'
    $env:KOMAPHONE_SERVICE_ENDPOINT = $null; $env:KOMAPHONE_TRUSTED_CERT = $null
    $token = Join-Path $k5Root 'control.token'; $port = Port-K5
    $arguments = @('--new-instance','--control',"$port",'--control-token-file',('"'+$token+'"'),'--automation-read-root',('"'+$k5Root+'"'),'--automation-write-root',('"'+$k5Root+'"'))
    $process = Start-Process -FilePath $AppExe -ArgumentList $arguments -WindowStyle Hidden -PassThru -WorkingDirectory $k5Repo `
        -RedirectStandardOutput (Join-Path $k5Root "app-$script:k5Generation.stdout.log") -RedirectStandardError (Join-Path $k5Root "app-$script:k5Generation.stderr.log")
    $script:k5App = @{Process=$process;StartTicks=$process.StartTime.ToUniversalTime().Ticks;Port=$port;Token=$token;Client=$null;Generation=$script:k5Generation}
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    do {
        if ($process.HasExited) { throw "Native app exited during startup: $($process.ExitCode)" }
        try { Connect-K5; break } catch { Start-Sleep -Milliseconds 100 }
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $script:k5App.Client) { throw 'Native app control startup timeout' }
    Wait-K5 {param($s) $s.available} 'canvas storage initialization' | Out-Null
    Resize-K5 1920 1080
    $ui = Send-K5 'ui.inspect'
    if (-not $ui.canvases.open) { Click-K5 'home.canvases' }
    Wait-Ui-K5 {param($ui) $ui.canvases.open} 'canvas workspace' | Out-Null
}
function Stop-K5([string]$reason='cleanup') {
    if (-not $script:k5App) { return }
    if ($script:k5App.Client) { $script:k5App.Client.Dispose(); $script:k5App.Client=$null }
    $process = $script:k5App.Process
    if ($process -and -not $process.HasExited) {
        $actual = Get-Process -Id $process.Id -ErrorAction Stop
        if ($actual.StartTime.ToUniversalTime().Ticks -ne $script:k5App.StartTicks -or $actual.Path -ne $AppExe) { throw 'Refusing to stop a process whose identity changed' }
        Stop-Process -Id $actual.Id -Force
        $process.WaitForExit()
        $script:k5Interactions.Add(@{kind='process-kill';reason=$reason;generation=$script:k5Generation;pid=$actual.Id;at=[DateTime]::UtcNow.ToString('o')})
    }
}
function Wait-K5([scriptblock]$condition,[string]$label,[int]$seconds=45) {
    $deadline=[DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $state=Status-K5
        if ($state.error) { throw "Canvas error while waiting for ${label}: $($state.error)" }
        if (& $condition $state) { return $state }
        Start-Sleep -Milliseconds 40
    } while ([DateTime]::UtcNow -lt $deadline)
    $state | ConvertTo-Json -Depth 45 | Set-Content -LiteralPath (Join-Path $k5Root 'timeout-state.json') -Encoding utf8
    throw "K5 timeout: $label"
}
function Idle-K5 {
    Wait-K5 {param($s) -not $s.pending -and -not $s.busy -and (-not $s.active -or ($s.view -and $s.view.revision -eq $s.active.revision -and (Json-K5 $s.view.viewport.camera) -eq (Json-K5 $s.camera)))} 'durable save and current view'
}
function Wait-Ui-K5([scriptblock]$condition,[string]$label) {
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    do { $ui=Send-K5 'ui.inspect'; if (& $condition $ui) {return $ui}; Start-Sleep -Milliseconds 60 } while ([DateTime]::UtcNow -lt $deadline)
    throw "K5 UI timeout: $label"
}
function Resize-K5([int]$width,[int]$height) {
    Send-K5 'ui.resize' @{width=$width;height=$height} | Out-Null
    Wait-Ui-K5 {param($ui) [Math]::Abs($ui.window.width-$width) -le 2 -and [Math]::Abs($ui.window.height-$height) -le 2} "${width}x$height" | Out-Null
}
function Visible-K5($ui,$rect) {
    return $rect -and $rect.Count -eq 4 -and $rect[0] -ge 0 -and $rect[1] -ge 43 -and $rect[2] -le $ui.window.width -and $rect[3] -le $ui.window.height-5
}
function Click-K5([string]$id) {
    $deadline=[DateTime]::UtcNow.AddSeconds(20); $last=''; $lastFrame=-1; $stable=0; $scrolls=0
    do {
        $ui=Send-K5 'ui.inspect'; $rect=$ui.roomControls.$id
        if (Visible-K5 $ui $rect) {
            $key=$rect -join ','
            if ($key -eq $last -and $ui.frame -ne $lastFrame) {$stable++} else {$stable=1}
            $last=$key; $lastFrame=$ui.frame
            if ($stable -ge 2) {
                Send-K5 'ui.click' @{x=($rect[0]+$rect[2])/2;y=($rect[1]+$rect[3])/2} | Out-Null
                $script:k5Interactions.Add(@{kind='click';id=$id;rect=$rect;frame=$ui.frame;at=[DateTime]::UtcNow.ToString('o')})
                return
            }
        } elseif ($ui.canvases.open -and ($id -like 'canvas.bookmark*' -or $id -like 'canvas.flight*' -or $id -like 'canvas.open.*' -or $id -in @('canvas.name','canvas.rename','komaphone.canvas.create','komaphone.canvas.rename','komaphone.canvas.bookmark'))) {
            $dy=if ($rect -and $rect[1] -lt 43) {500} elseif ($rect -and $rect[3] -gt $ui.window.height-5) {-500} elseif ($scrolls -lt 2) {2000} else {-500}
            Send-K5 'ui.canvasInput' @{kind='wheel';x=150;y=[Math]::Min(500,$ui.window.height-100);deltaY=$dy;unit='point'} | Out-Null
            $scrolls++; $stable=0
        }
        Start-Sleep -Milliseconds 60
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "K5 control is not visibly clickable: $id"
}
function Type-K5([string]$control,[string]$text,[string]$property) {
    Click-K5 $control
    Send-K5 'ui.key' @{key='A';ctrl=$true} | Out-Null
    Send-K5 'ui.type' @{text=$text} | Out-Null
    Send-K5 'ui.key' @{key='Enter'} | Out-Null
    Wait-Ui-K5 {param($ui) $ui.canvases.$property -eq $text} "typed $control" | Out-Null
    $script:k5Interactions.Add(@{kind='typed';id=$control;text=$text;at=[DateTime]::UtcNow.ToString('o')})
}
function Drawing-K5 {
    $ui=Send-K5 'ui.inspect'; $rect=$ui.roomControls.'canvas.drawing'
    if (-not (Visible-K5 $ui $rect)) {throw 'Visible canvas drawing area is unavailable'}
    return $rect
}
function Input-K5([string]$kind,[double]$x,[double]$y,[hashtable]$extra=@{}) {
    $params=@{kind=$kind;x=$x;y=$y}; foreach($key in $extra.Keys){$params[$key]=$extra[$key]}
    Send-K5 'ui.canvasInput' $params | Out-Null
    $script:k5Interactions.Add(@{kind='canvas-input';input=$params;at=[DateTime]::UtcNow.ToString('o')})
}
function Draw-K5([object[]]$offsets,[string]$label) {
    $before=Idle-K5; $rect=Drawing-K5; $x=($rect[0]+$rect[2])/2; $y=($rect[1]+$rect[3])/2
    $expectedSamples=0; $previous=$null
    foreach($offset in $offsets){
        if($null -eq $previous -or $offset[0] -ne $previous[0] -or $offset[1] -ne $previous[1]){$expectedSamples++}
        $previous=$offset
    }
    $sourceStart=$script:k5Interactions.Count
    Input-K5 'down' ($x+$offsets[0][0]) ($y+$offsets[0][1])
    for($i=1;$i -lt $offsets.Count;$i++){Input-K5 'move' ($x+$offsets[$i][0]) ($y+$offsets[$i][1])}
    $last=$offsets[-1]; Input-K5 'up' ($x+$last[0]) ($y+$last[1])
    $saved=Wait-K5 {param($s) $s.active.id -eq $before.active.id -and $s.active.stroke_count -eq $before.active.stroke_count+1 -and -not $s.pending} "mouse stroke $label"
    Idle-K5 | Out-Null
    $view=View-K5
    $newStrokes=@($view.strokes | Where-Object {$_.revision -gt $before.active.revision})
    $actualSamples=if($newStrokes.Count -eq 1){$newStrokes[0].samples}else{$null}
    $sourceCommands=@($script:k5Interactions | Select-Object -Skip $sourceStart | Where-Object kind -eq 'canvas-input' | ForEach-Object {@{method='ui.canvasInput';params=$_.input}})
    $inputCheck=@{passed=($newStrokes.Count -eq 1 -and $actualSamples -eq $expectedSamples);expectedSamples=$expectedSamples;actualSamples=$actualSamples;newStrokeCount=$newStrokes.Count;strokes=$newStrokes;requestedOffsets=$offsets;sourceCommands=$sourceCommands;sourceInteractionStart=$sourceStart;sourceInteractionEnd=$script:k5Interactions.Count;rule='One retained sample per requested point, removing consecutive duplicates only; closing the path repeats the first point and remains a sample. Up repeats the last point.'}
    Record-K5 $label @{before=$before.active;saved=$saved.active;view=$view;inputCheck=$inputCheck}
    if(-not $inputCheck.passed){throw "K5 input integrity failed for ${label}: expected $expectedSamples retained samples in one new stroke, observed $actualSamples samples across $($newStrokes.Count) new strokes. Physical pointer movement may have interleaved with injected mouse events; inspect the saved inputCheck/sourceCommands and stroke evidence, then rerun with pointer input quiet. Visual shape inspection remains required."}
    return $saved
}
function House-K5([string]$label) {
    Draw-K5 @(@(-140,120),@(-140,-70),@(0,-180),@(140,-70),@(140,120),@(-140,120)) "$label-outline" | Out-Null
    Draw-K5 @(@(45,120),@(45,30),@(100,30),@(100,120)) "$label-door" | Out-Null
    Draw-K5 @(@(-105,-30),@(-55,-30),@(-55,25),@(-105,25),@(-105,-30)) "$label-window" | Out-Null
}
function Bookmark-K5([string]$name) {
    Idle-K5 | Out-Null
    Type-K5 'canvas.bookmarkName' $name 'bookmark_name'
    Click-K5 'komaphone.canvas.bookmark'
    Wait-K5 {param($s) @($s.active.bookmarks | Where-Object name -eq $name).Count -eq 1 -and -not $s.pending} "bookmark $name" | Out-Null
}
function Screenshot-K5([string]$name) {
    Idle-K5 | Out-Null
    Send-K5 'ui.screenshot' @{path="$name.png";focus=$false} | Out-Null
    $path=Join-Path $k5Root "$name.png"
    if (-not (Test-Path -LiteralPath $path)) {throw "Screenshot missing: $path"}
    $script:k5Screenshots.Add(@{path=$path;sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash;ui=(Send-K5 'ui.inspect').window;inspected=$false})
}
function Open-K5([string]$id) {
    Idle-K5 | Out-Null; Click-K5 "canvas.open.$id"
    Wait-K5 {param($s) $s.active.id -eq $id -and -not $s.pending} "open $id" | Out-Null
    Idle-K5 | Out-Null
}
function Go-K5([int]$index) {
    Idle-K5 | Out-Null; Click-K5 "canvas.bookmark.$index.go"; Idle-K5 | Out-Null
}
function Origin-K5([int]$depth) {
    $state=Idle-K5
    if ($state.camera.region.depth -ne $depth -or $state.camera.region.x -ne '0' -or $state.camera.region.y -ne '0' -or
        $state.camera.offset[0] -ne '0' -or $state.camera.offset[1] -ne '0') {throw "Expected the exact x/y region seam at depth $depth"}
    return $state
}
function Ancestor-K5([string]$canvas) {
    Open-K5 $canvas; Click-K5 'komaphone.canvas.home'; Origin-K5 0 | Out-Null
    $ui=Send-K5 'ui.inspect'; $radius=[double]$ui.canvases.brush_width/2
    if ($radius -le 0 -or $ui.canvases.lock_width -or $ui.canvases.erase) {throw 'Ancestor fixture needs the current opaque screen-width brush'}
    # The original root stroke's bottom edge is exactly world y=0. Its centreline
    # is offscreen at deep zoom; the visible half-plane must still be retained.
    $before=View-K5
    Draw-K5 @(@(-250,-$radius),@(250,-$radius)) 'ancestor-root-edge' | Out-Null
    $rootView=View-K5
    $ancestor=@($rootView.strokes | Where-Object {$_.id -notin @($before.strokes.id)})
    if ($ancestor.Count -ne 1 -or $ancestor[0].depth -ne 0) {throw 'Expected one original root ancestor stroke'}
    Bookmark-K5 'Ancestor edge root'; Screenshot-K5 'ancestor-edge-0'
    Click-K5 'canvas.tool.eraser'; Wait-Ui-K5 {param($state) $state.canvases.erase} 'eraser selected' | Out-Null
    $beforeErase=Idle-K5; $beforeEraseView=View-K5
    Draw-K5 @(@(100,-35),@(100,35)) 'ancestor-root-eraser' | Out-Null
    $erased=Idle-K5; $erasedView=View-K5; Screenshot-K5 'ancestor-erased-root'
    if ($erasedView.contentHash -eq $beforeEraseView.contentHash) {throw 'Eraser gesture did not change the canonical root view'}
    Click-K5 'komaphone.canvas.undo'; Idle-K5 | Out-Null
    $restoredView=View-K5
    if ($restoredView.contentHash -ne $beforeEraseView.contentHash) {throw 'Undo did not restore the exact pre-eraser artwork'}
    Record-K5 'native-eraser-and-undo' @{before=$beforeErase.active;erased=$erased.active;erasedView=$erasedView;restored=$restoredView}
    Click-K5 'canvas.tool.brush'; Wait-Ui-K5 {param($state) -not $state.canvases.erase} 'brush restored' | Out-Null
    # Lock at 1x, then draw at 2x: the locked line is 10px and the following
    # unlocked line is 5px. Undo both temporary lines before the ancestor test.
    Click-K5 'canvas.lockWidth'; Wait-Ui-K5 {param($state) $state.canvases.lock_width} 'canvas thickness lock' | Out-Null
    $rect=Drawing-K5; $x=($rect[0]+$rect[2])/2; $y=($rect[1]+$rect[3])/2
    Input-K5 'wheel' $x $y @{deltaY=([Math]::Log(2)/0.0025);unit='point'}
    Wait-K5 {param($s) [Math]::Abs($s.camera.zoom-2) -lt 0.001 -and -not $s.pending} 'twofold zoom with locked width' | Out-Null
    Draw-K5 @(@(200,-100),@(200,100)) 'width-locked-at-2x' | Out-Null
    Click-K5 'canvas.lockWidth'; Wait-Ui-K5 {param($state) -not $state.canvases.lock_width} 'screen thickness restored' | Out-Null
    Draw-K5 @(@(240,-100),@(240,100)) 'width-unlocked-at-2x' | Out-Null
    Screenshot-K5 'width-lock-comparison-2x'
    Record-K5 'native-width-lock' @{ui=(Send-K5 'ui.inspect').canvases;view=(View-K5);expected='left vertical stroke 10px; right vertical stroke 5px'}
    Click-K5 'komaphone.canvas.undo'; Idle-K5 | Out-Null; Click-K5 'komaphone.canvas.undo'; Idle-K5 | Out-Null
    Click-K5 'komaphone.canvas.home'; Origin-K5 0 | Out-Null
    if ((View-K5).contentHash -ne $rootView.contentHash) {throw 'Temporary brush-width gestures changed the original ancestor fixture'}
    for($depth=1;$depth -le 6;$depth++) {
        $rect=Drawing-K5; $x=($rect[0]+$rect[2])/2; $y=($rect[1]+$rect[3])/2
        Input-K5 'wheel' $x $y @{deltaY=([Math]::Log(100.001)/0.0025);unit='point'}
        Wait-K5 {param($s) $s.camera.region.depth -eq $depth -and -not $s.pending} "ancestor detail step $depth" | Out-Null
        $state=Origin-K5 $depth; $view=View-K5
        $loaded=@($view.strokes | Where-Object id -eq $ancestor[0].id)
        if ($loaded.Count -ne 1 -or $loaded[0].blake3 -ne $ancestor[0].blake3) {throw "Original ancestor missing or changed at depth $depth"}
        Record-K5 "ancestor-depth-$depth" @{camera=$state.camera;view=$view;edge='world y=0 at viewport centre; painted upper half-plane'}
        if ($depth -in @(1,3,6)) {Screenshot-K5 "ancestor-edge-$depth"}
    }
    Draw-K5 @(@(-100,260),@(-100,100),@(0,30),@(100,100),@(100,260),@(-100,260)) 'ancestor-fine-house' | Out-Null
    Draw-K5 @(@(-60,120),@(-20,120),@(-20,160),@(-60,160),@(-60,120)) 'ancestor-fine-window' | Out-Null
    Bookmark-K5 'Ancestor edge detail'; $baseline=View-K5; Screenshot-K5 'ancestor-with-detail'
    $rect=Drawing-K5; $x=($rect[0]+$rect[2])/2; $y=($rect[1]+$rect[3])/2
    Input-K5 'down' $x $y @{button='middle'}; Input-K5 'move' ($x+32) ($y+32); Input-K5 'up' ($x+32) ($y+32) @{button='middle'}
    Idle-K5 | Out-Null; Screenshot-K5 'ancestor-edge-panned'
    $panned=View-K5
    if (@($panned.strokes | Where-Object id -eq $ancestor[0].id).Count -ne 1) {throw 'Ancestor disappeared after deep lateral pan'}
    Go-K5 2
    if ((View-K5).contentHash -ne $baseline.contentHash) {throw 'Ancestor detail bookmark did not restore exact loaded art'}
    Record-K5 'ancestor-edge-and-detail' @{canvas=$canvas;original=$ancestor[0];baseline=$baseline;panned=$panned}
    return $baseline
}
function CloseDraft-K5([string]$canvas) {
    Open-K5 $canvas; $before=Idle-K5; $beforeView=View-K5
    $rect=Drawing-K5; $x=($rect[0]+$rect[2])/2; $y=($rect[1]+$rect[3])/2
    Input-K5 'down' ($x-60) ($y-20); Input-K5 'move' ($x+60) ($y+20)
    # Exercise the real native close route while a pointer stroke is unfinished.
    Send-K5 'app.quit' | Out-Null
    $script:k5Interactions.Add(@{kind='close-request';method='app.quit';at=[DateTime]::UtcNow.ToString('o')})
    $prompt=Wait-Ui-K5 {param($ui) $ui.canvases.close_prompt} 'unsaved canvas close guard'
    $blocked=Status-K5
    if($blocked.active.stroke_count -ne $before.active.stroke_count){throw 'Interrupted draft committed before the close decision'}
    Screenshot-K5 'close-unsaved-canvas-prompt'
    Click-K5 'canvas.close.keep'
    $kept=Wait-Ui-K5 {param($ui) -not $ui.canvases.close_prompt -and $ui.roomControls.'canvas.retryStroke'} 'retained stroke after Keep drawing'
    Input-K5 'up' ($x+60) ($y+20)
    Click-K5 'canvas.retryStroke'
    $saved=Wait-K5 {param($s) $s.active.id -eq $canvas -and $s.active.stroke_count -eq $before.active.stroke_count+1 -and -not $s.pending} 'retried interrupted stroke saved'
    Idle-K5 | Out-Null; $savedView=View-K5
    if($savedView.contentHash -eq $beforeView.contentHash){throw 'Retry did not preserve the interrupted stroke'}
    Click-K5 'komaphone.canvas.undo'; $undone=Idle-K5; $restored=View-K5
    if($restored.contentHash -ne $beforeView.contentHash -or (Json-K5 $restored.strokes) -ne (Json-K5 $beforeView.strokes)){throw 'Undo did not restore the pre-close canvas artwork'}
    Record-K5 'native-close-draft-recovery' @{before=$before;beforeView=$beforeView;promptBlocked=$prompt.canvases.close_prompt;keptDraft=($null -ne $kept.roomControls.'canvas.retryStroke');saved=$saved;savedView=$savedView;undone=$undone;restoredView=$restored}
}
function Measure-K5Frames {
    Go-K5 7
    $before=Idle-K5; $rect=Drawing-K5; $x=($rect[0]+$rect[2])/2; $y=($rect[1]+$rect[3])/2
    $samples=@{}; $afterFrame=-1; $ui=$null
    $processesBefore=Processes-K5
    $script:k5App.Process.Refresh(); $startWorkingSet=$script:k5App.Process.WorkingSet64
    for($round=0;$round -lt 72;$round++) {
        $direction=if($round%2 -eq 0){1}else{-1}
        Input-K5 'down' $x $y @{button='middle'}
        Input-K5 'move' ($x+4*$direction) $y
        Input-K5 'up' ($x+4*$direction) $y @{button='middle'}
        Input-K5 'wheel' $x $y @{deltaY=(4*$direction);unit='point'}
        $ui=Send-K5 'ui.inspect'
        if(-not $ui.canvasFrameSamples){throw 'Native CPU frame instrumentation is unavailable'}
        if($round -lt 8) {
            $afterFrame=($ui.canvasFrameSamples | Measure-Object -Property frame -Maximum).Maximum
        } else {
            foreach($sample in $ui.canvasFrameSamples) {
                if($sample.frame -gt $afterFrame) {
                    if($null -eq $sample.cpu_ms -or [double]::IsNaN($sample.cpu_ms) -or [double]::IsInfinity($sample.cpu_ms) -or $sample.cpu_ms -lt 0){throw 'Invalid native CPU frame sample'}
                    $samples["$($sample.frame)"]=$sample
                }
            }
        }
    }
    $raw=@($samples.Values | Sort-Object frame)
    if($raw.Count -lt 100){throw "Too few measured native frames: $($raw.Count)"}
    $times=@($raw.cpu_ms | Sort-Object)
    $cpuP50=$times[[Math]::Max(0,[Math]::Ceiling($times.Count*0.50)-1)]
    $cpuP95=$times[[Math]::Max(0,[Math]::Ceiling($times.Count*0.95)-1)]
    $intervals=@($raw | Where-Object {$null -ne $_.interval_ms} | ForEach-Object {
        $interval=[double]$_.interval_ms
        if ([double]::IsNaN($interval) -or [double]::IsInfinity($interval) -or $interval -le 0) {throw 'Invalid native frame interval sample'}
        $interval
    } | Sort-Object)
    if($intervals.Count -lt 100){throw "Too few measured native frame intervals: $($intervals.Count)"}
    $p50=$intervals[[Math]::Max(0,[Math]::Ceiling($intervals.Count*0.50)-1)]
    $p95=$intervals[[Math]::Max(0,[Math]::Ceiling($intervals.Count*0.95)-1)]
    $script:k5App.Process.Refresh(); $after=Idle-K5
    if($after.active.stroke_count -ne $before.active.stroke_count){throw 'Performance navigation changed artwork'}
    $script:k5Evidence.performance=@{
        scope='frame-start interval from consecutive native input timestamps; includes CPU, GPU/presentation waits, scheduling and automation gaps; not input-to-display latency'
        cpuScope='previous eframe CPU frame duration, including UI/tessellation/encoding but excluding get_current_texture, queue.submit and present call durations; GPU execution timing unmeasured'
        workload='1920x1080 window, depth-six nested house fixture, 8 warmup then 64 measured rounds of real middle-drag ±4px and wheel ±4points; four independent canvases and far lateral artwork retained'
        adapter=$ui.perf.timings.gpuInfo; window=$ui.window; canvas=$before.active.id; strokes=$before.active.stroke_count
        sampleCount=$raw.Count; intervalSampleCount=$intervals.Count; p50_ms=$p50; p95_ms=$p95; max_ms=$intervals[-1]; target_p95_ms=33.0
        cpu_p50_ms=$cpuP50; cpu_p95_ms=$cpuP95; cpu_max_ms=$times[-1]
        startWorkingSetBytes=$startWorkingSet; endWorkingSetBytes=$script:k5App.Process.WorkingSet64; peakWorkingSetBytes=$script:k5App.Process.PeakWorkingSet64
        processesBefore=$processesBefore; processesAfter=(Processes-K5)
        samples=$raw; passed=($p95 -le 33.0)
    }
    if($p95 -gt 33.0){
        if(-not $Diagnostic){throw "K5 native frame interval p95 ${p95}ms exceeds 33ms"}
        $script:k5Evidence.performanceLimitation="Diagnostic p95 ${p95}ms exceeds 33ms; timing is not accepted. Continue independent functional/recovery checks, then rerun the unchanged final gate without Diagnostic after contention settles."
    }
}

if ($FunctionsOnly) { return }

try {
    Start-K5
    $ids=[System.Collections.Generic.List[string]]::new()
    foreach($name in @('Nested houses','Twin','Twin','Solo')) {
        Type-K5 'canvas.name' $name 'name'; $before=Status-K5
        Click-K5 'komaphone.canvas.create'
        $created=Wait-K5 {param($s) $s.items.Count -eq $before.items.Count+1 -and $s.active.name -eq $name -and -not $s.pending} 'created independent canvas'
        $ids.Add($created.active.id)
        Draw-K5 @(@(-80,-80),@(80,-80),@(80,80),@(-80,80),@(-80,-80)) "initial-$($ids.Count)" | Out-Null
        Bookmark-K5 'Start'
    }
    if (@($ids | Sort-Object -Unique).Count -ne 4) {throw 'Canvas identities are not independent'}
    Type-K5 'canvas.rename' 'Solo renamed' 'rename'; Click-K5 'komaphone.canvas.rename'
    Wait-K5 {param($s) $s.active.name -eq 'Solo renamed' -and -not $s.pending} 'renamed canvas' | Out-Null
    $initial=Idle-K5; Record-K5 'four-canvases' $initial
    if (@($initial.items | Where-Object name -eq 'Twin').Count -ne 2) {throw 'Duplicate canvas names were not preserved'}
    CloseDraft-K5 $ids[1]
    Open-K5 $ids[0]
    Click-K5 'komaphone.canvas.home'; Idle-K5 | Out-Null
    $baselineViews=@{}
    for($depth=0;$depth -le 6;$depth++) {
        if ($depth -gt 0) {
            $before=Idle-K5; $rect=Drawing-K5; $x=($rect[0]+$rect[2])/2; $y=($rect[1]+$rect[3])/2
            # 100.001x leaves normalization safely past the boundary after f32 input.
            Input-K5 'wheel' $x $y @{deltaY=([Math]::Log(100.001)/0.0025);unit='point'}
            $after=Wait-K5 {param($s) $s.camera.region.depth -eq $depth -and -not $s.pending} "100x detail step $depth"
            $ratio=([Math]::Pow(100,$after.camera.region.depth)*$after.camera.zoom)/([Math]::Pow(100,$before.camera.region.depth)*$before.camera.zoom)
            if ([Math]::Abs($ratio-100)/100 -gt 0.002) {throw "Detail step ratio was $ratio"}
            Record-K5 "zoom-$depth" @{before=$before.camera;after=$after.camera;ratio=$ratio}
        }
        House-K5 "depth-$depth"
        $seam=Origin-K5 $depth
        Record-K5 "native-seam-$depth" @{camera=$seam.camera;screenBoundsRelativeToSeam=@(-140,-180,140,120);logicalStroke='one house outline crosses both x=0 and y=0 region boundaries';view=(View-K5)}
        Bookmark-K5 "Depth $depth"
        Screenshot-K5 "depth-$depth"
    }
    # At 1920px, repeated real middle drags extend more than ten party widths.
    Click-K5 'komaphone.canvas.home'; Idle-K5 | Out-Null
    $beforePan=Status-K5
    for($i=0;$i -lt 32;$i++) {
        $rect=Drawing-K5; $x=($rect[0]+$rect[2])/2; $y=($rect[1]+$rect[3])/2; $distance=($rect[2]-$rect[0])*0.35
        Input-K5 'down' $x $y @{button='middle'}; Input-K5 'move' ($x-$distance) $y; Input-K5 'up' ($x-$distance) $y @{button='middle'}
    }
    $lateral=Idle-K5
    if ([bigint]$lateral.camera.region.x-[bigint]$beforePan.camera.region.x -lt 12) {throw 'Lateral mouse travel did not exceed twelve root regions'}
    House-K5 'lateral'; Bookmark-K5 'Lateral'; Screenshot-K5 'lateral-1920'
    $lastIndex=@((Status-K5).active.bookmarks).Count-1
    $beforeUndo=Idle-K5; $beforeUndoView=View-K5
    Click-K5 'komaphone.canvas.undo'
    Wait-K5 {param($s) $s.active.undo_count -eq $beforeUndo.active.undo_count-1 -and -not $s.pending} 'undo' | Out-Null
    $undoView=View-K5
    if ($undoView.contentHash -eq $beforeUndoView.contentHash) {throw 'Undo did not change loaded artwork'}
    Click-K5 'komaphone.canvas.redo'; Idle-K5 | Out-Null
    if ((View-K5).contentHash -ne $beforeUndoView.contentHash) {throw 'Redo did not restore canonical loaded strokes'}
    Click-K5 'komaphone.canvas.home'; Idle-K5 | Out-Null
    Click-K5 "canvas.bookmark.$lastIndex.play"
    Wait-K5 {param($s) $null -ne $s.flight} 'automatic zoom started' | Out-Null
    Click-K5 'canvas.flight.pause'; $paused=Wait-K5 {param($s) $s.flight.paused} 'automatic zoom paused'
    Start-Sleep -Milliseconds 250; $still=Status-K5
    if ($still.flight.elapsed -ne $paused.flight.elapsed -or (Json-K5 $still.camera) -ne (Json-K5 $paused.camera)) {throw 'Paused camera moved'}
    Click-K5 'canvas.flight.resume'; Start-Sleep -Milliseconds 100; Click-K5 'canvas.flight.cancel'
    Wait-K5 {param($s) $null -eq $s.flight -and -not $s.pending} 'automatic zoom cancelled' | Out-Null
    Click-K5 "canvas.bookmark.$lastIndex.play"
    $flightEnd=Wait-K5 {param($s) $null -eq $s.flight -and -not $s.pending -and (Json-K5 $s.camera) -eq (Json-K5 $s.active.bookmarks[$lastIndex].camera)} 'automatic zoom completed'
    if ($flightEnd.active.stroke_count -ne $beforeUndo.active.stroke_count) {throw 'Navigation changed artwork'}
    Record-K5 'flight-pause-cancel-complete' @{paused=$paused;still=$still;complete=$flightEnd}
    Resize-K5 1280 720; Screenshot-K5 'lateral-1280'; Resize-K5 1920 1080; Idle-K5 | Out-Null
    Measure-K5Frames
    $ancestorBaseline=Ancestor-K5 $ids[3]
    Open-K5 $ids[0]
    # Compare exact loaded original/summary identities at every named deep view.
    for($index=1;$index -le 7;$index++) {Go-K5 $index; $baselineViews["$index"]=View-K5; Record-K5 "before-reopen-depth-$index" $baselineViews["$index"]}
    Go-K5 8; $lateralBaseline=View-K5; Record-K5 'before-reopen-lateral' $lateralBaseline; Go-K5 7
    $beforeKill=Idle-K5; Record-K5 'before-acknowledged-kill' $beforeKill
    Stop-K5 'force kill after every completed edit and camera ACK'
    Start-K5; $reopened=Idle-K5
    if ((@($reopened.items.id | Sort-Object)-join ',') -ne (@($ids | Sort-Object)-join ',')) {throw 'Canvas list changed across process kill'}
    if ($reopened.active.id -ne $beforeKill.active.id -or (Json-K5 $reopened.camera) -ne (Json-K5 $beforeKill.camera)) {throw 'Acknowledged selected canvas/camera was not recovered'}
    for($index=1;$index -le 7;$index++) {
        Go-K5 $index; $actual=View-K5
        if ($actual.contentHash -ne $baselineViews["$index"].contentHash -or (Json-K5 $actual.strokes) -ne (Json-K5 $baselineViews["$index"].strokes)) {throw "Recovered depth bookmark $index differs from its canonical saved view"}
        Record-K5 "reopened-depth-$index" $actual
        Screenshot-K5 "reopened-depth-$($index-1)"
    }
    Go-K5 8; $reopenedLateral=View-K5
    if($reopenedLateral.contentHash -ne $lateralBaseline.contentHash -or (Json-K5 $reopenedLateral.strokes) -ne (Json-K5 $lateralBaseline.strokes)){throw 'Far lateral artwork changed across process kill'}
    Record-K5 'reopened-lateral' $reopenedLateral; Screenshot-K5 'reopened-lateral'; Go-K5 7
    Open-K5 $ids[3]; Go-K5 2; $reopenedAncestor=View-K5
    if ($reopenedAncestor.contentHash -ne $ancestorBaseline.contentHash -or (Json-K5 $reopenedAncestor.strokes) -ne (Json-K5 $ancestorBaseline.strokes)) {throw 'Root ancestor or deep fine detail changed across process kill'}
    Screenshot-K5 'reopened-ancestor-with-detail'; Record-K5 'reopened-ancestor' $reopenedAncestor
    foreach($id in $ids) {
        Open-K5 $id; $current=Idle-K5
        $old=@($beforeKill.items | Where-Object id -eq $id)[0]
        if ($current.active.name -ne $old.name -or $current.active.owner -ne $old.owner -or $current.active.stroke_count -ne $old.stroke_count -or $current.active.undo_count -ne $old.undo_count -or (Json-K5 $current.active.bookmarks) -ne (Json-K5 $old.bookmarks)) {throw "Canvas $id mixed metadata or history during recovery"}
        Record-K5 "independent-$id" @{state=$current;view=(View-K5)}
    }
    # Deliver a real switch click and kill without waiting for its acknowledgment.
    $switchFrom=(Status-K5).active.id
    Click-K5 "canvas.open.$($ids[0])"; Stop-K5 'after switch click, before waiting for ACK (transaction boundary unknown)'
    Start-K5; $switchRecovered=Idle-K5
    if ($switchRecovered.active.id -notin @($switchFrom,$ids[0])) {throw 'Interrupted switch recovered an unrelated canvas'}
    foreach($id in $ids) {Open-K5 $id; $s=Idle-K5; $old=@($beforeKill.items | Where-Object id -eq $id)[0]; if ($s.active.stroke_count -ne $old.stroke_count -or (Json-K5 $s.active.bookmarks) -ne (Json-K5 $old.bookmarks)) {throw 'Interrupted switch mixed or lost saved art/bookmarks'}}
    Open-K5 $ids[0]; Go-K5 7; Screenshot-K5 'final-recovered-depth-6'
    $script:k5Evidence.canvases=$ids.ToArray(); $script:k5Evidence.final=Idle-K5; $script:k5Evidence.switchRecovery=$switchRecovered
    $script:k5Evidence.functionalChecksPassed=$true
    $script:k5Evidence.passed=[bool]$script:k5Evidence.performance.passed
} catch {
    $script:k5Evidence.error=$_.Exception.Message
    $script:k5Evidence.failureStack=$_.ScriptStackTrace
    throw
} finally {
    try {Stop-K5 'harness cleanup'} catch {$script:k5Evidence.cleanupError=$_.Exception.Message; $script:k5Evidence.passed=$false}
    foreach($key in $k5Original.Keys){[Environment]::SetEnvironmentVariable($key,$k5Original[$key],'Process')}
    $script:k5Evidence.finished=[DateTime]::UtcNow.ToString('o')
    $script:k5Evidence.interactions=$script:k5Interactions.ToArray()
    $script:k5Evidence.screenshots=$script:k5Screenshots.ToArray()
    $script:k5Evidence.snapshots=$script:k5Snapshots.ToArray()
    $script:k5Evidence.visualReview='Required separately: screenshots are captured, not automatically inspected'
    $script:k5Evidence | ConvertTo-Json -Depth 65 | Set-Content -LiteralPath (Join-Path $k5Root 'result.json') -Encoding utf8
    Write-Output (Join-Path $k5Root 'result.json')
}
