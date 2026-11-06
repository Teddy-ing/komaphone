# Isolated real-process K3 acceptance. Requires the native app, service, and the
# room-client credential cleanup example; all output stays under repo/log/k3.
# The multiplayer service and each app's automation channel bind only loopback.
param(
    [string]$AppExe = '',
    [string]$ServiceExe = '',
    [string]$CredentialCleanupExe = '',
    [switch]$Diagnostic
)
$ErrorActionPreference = 'Stop'
$k3Repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if (-not $AppExe) {
    $AppExe = Join-Path $k3Repo 'log/k3/bin/photocraft.exe'
    if (-not (Test-Path -LiteralPath $AppExe)) { $AppExe = Join-Path $k3Repo 'target/agent-root/debug/photocraft.exe' }
}
if (-not $ServiceExe) { $ServiceExe = Join-Path $k3Repo 'log/k3/bin/photocraft-sync.exe' }
if (-not $CredentialCleanupExe) { $CredentialCleanupExe = Join-Path $k3Repo 'log/k3/bin/cleanup_credentials.exe' }
$AppExe = (Resolve-Path -LiteralPath $AppExe).Path
$ServiceExe = (Resolve-Path -LiteralPath $ServiceExe).Path
$CredentialCleanupExe = (Resolve-Path -LiteralPath $CredentialCleanupExe).Path
$k3OriginalDataRoot = $env:KOMAPHONE_DATA_DIR
$k3OriginalConfigRoot = $env:PHOTOCRAFT_CONFIG_DIR
$k3Root = Join-Path (Join-Path $k3Repo 'log/k3') ('native-rooms-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $k3Root | Out-Null
$script:k3Apps = @{}
$script:k3Service = $null
$script:k3ServicePort = 0
$script:k3ServiceGeneration = 0
$script:k3Evidence = [ordered]@{
    validation = $(if ($Diagnostic) { 'diagnostic-only; final refreshed build requires complete rerun' } else { 'acceptance-candidate' })
    appHash = (Get-FileHash -LiteralPath $AppExe -Algorithm SHA256).Hash
    serviceHash = (Get-FileHash -LiteralPath $ServiceExe -Algorithm SHA256).Hash
}

function New-K3Port {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    return $port
}
function Start-K3Service {
    $script:k3ServiceGeneration++
    $generation = $script:k3ServiceGeneration
    $root = Join-Path $k3Root 'authority'
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $stdout = Join-Path $k3Root "service-$generation.stdout.log"
    $stderr = Join-Path $k3Root "service-$generation.stderr.log"
    $arguments = @('--bind', "127.0.0.1:$script:k3ServicePort", '--data-dir', ('"' + $root + '"'))
    $script:k3Service = Start-Process -FilePath $ServiceExe -ArgumentList $arguments -WindowStyle Hidden -PassThru `
        -WorkingDirectory $k3Repo -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {
        if ($script:k3Service.HasExited) { throw "Service exited: $($script:k3Service.ExitCode); see $stderr" }
        $ready = Get-Content -LiteralPath $stdout -ErrorAction SilentlyContinue | Where-Object { $_ -match '^\{' } | Select-Object -First 1
        if ($ready) {
            $result = $ready | ConvertFrom-Json
            if ($result.ready) {
                $script:k3ServicePort = [int]($result.address -split ':')[-1]
                return "http://127.0.0.1:$script:k3ServicePort"
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Service readiness timed out'
}
function Stop-K3Service {
    if ($script:k3Service -and -not $script:k3Service.HasExited) {
        Stop-Process -Id $script:k3Service.Id -Force
        $script:k3Service.WaitForExit()
    }
    $script:k3Service = $null
}
function Connect-K3Control([hashtable]$app) {
    if ($app.Client) { $app.Client.Dispose() }
    $client = [System.Net.Sockets.TcpClient]::new()
    $client.Connect('127.0.0.1', $app.Port)
    $stream = $client.GetStream()
    $stream.ReadTimeout = 20000
    $writer = [System.IO.StreamWriter]::new($stream, [System.Text.UTF8Encoding]::new($false))
    $writer.AutoFlush = $true
    $reader = [System.IO.StreamReader]::new($stream)
    $auth = @{ id = [guid]::NewGuid().ToString('N'); method = 'auth'; params = @{ token = (Get-Content -LiteralPath $app.TokenPath -Raw).Trim() } } | ConvertTo-Json -Compress
    $writer.WriteLine($auth)
    $reply = $reader.ReadLine() | ConvertFrom-Json
    if (-not $reply.ok) { $client.Dispose(); throw 'Isolated control authentication failed' }
    $app.Client = $client
    $app.Writer = $writer
    $app.Reader = $reader
    $app.LastUse = [DateTime]::UtcNow
}
function Send-K3([int]$number, [string]$method, [hashtable]$params = @{}) {
    $app = $script:k3Apps[$number]
    if (-not $app -or $app.Process.HasExited) { throw "Client $number is not running" }
    if (-not $app.Client -or ([DateTime]::UtcNow - $app.LastUse).TotalSeconds -gt 20) { Connect-K3Control $app }
    $frame = @{ id = [guid]::NewGuid().ToString('N'); method = $method; params = $params } | ConvertTo-Json -Depth 35 -Compress
    $app.Writer.WriteLine($frame)
    $line = $app.Reader.ReadLine()
    if (-not $line) { throw "Client $number closed control during $method; request may have completed" }
    $reply = $line | ConvertFrom-Json
    $app.LastUse = [DateTime]::UtcNow
    if (-not $reply.ok) { throw "Client $number $method failed: $($reply.error)" }
    return $reply.result
}
function Invoke-K3([int]$number, [string]$command, [hashtable]$params = @{}) {
    Send-K3 $number 'engine.execute' @{ command = $command; params = $params }
}
function Start-K3App([int]$number) {
    $root = Join-Path $k3Root "client-$number"
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $previous = $script:k3Apps[$number]
    $generation = if ($previous) { $previous.Generation + 1 } else { 1 }
    $env:KOMAPHONE_DATA_DIR = Join-Path $root 'data'
    $env:PHOTOCRAFT_CONFIG_DIR = Join-Path $root 'config'
    $tokenPath = Join-Path $root 'control.token'
    $port = New-K3Port
    $arguments = @('--new-instance', '--control', "$port", '--control-token-file', ('"' + $tokenPath + '"'),
        '--automation-read-root', ('"' + $root + '"'), '--automation-write-root', ('"' + $root + '"'))
    $process = Start-Process -FilePath $AppExe -ArgumentList $arguments -WindowStyle Hidden -PassThru -WorkingDirectory $k3Repo `
        -RedirectStandardOutput (Join-Path $root "app-$generation.stdout.log") -RedirectStandardError (Join-Path $root "app-$generation.stderr.log")
    $app = @{ Process = $process; Root = $root; TokenPath = $tokenPath; Port = $port; Generation = $generation }
    $script:k3Apps[$number] = $app
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    do {
        if ($process.HasExited) { throw "Client $number exited before control startup: $($process.ExitCode)" }
        try { Connect-K3Control $app; break } catch { Start-Sleep -Milliseconds 100 }
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $app.Client) { throw "Client $number control startup timed out" }
    Send-K3 $number 'ui.set' @{ roomsOpen = $true } | Out-Null
}
function Stop-K3App([int]$number) {
    $app = $script:k3Apps[$number]
    if ($app) {
        if ($app.Client) { $app.Client.Dispose(); $app.Client = $null }
        if ($app.Process -and -not $app.Process.HasExited) { Stop-Process -Id $app.Process.Id -Force; $app.Process.WaitForExit() }
    }
}
function Wait-K3([int]$number, [scriptblock]$condition, [string]$label, [int]$seconds = 40) {
    $deadline = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $state = Invoke-K3 $number 'komaphone.room.status'
        if (& $condition $state) { return $state }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    $state | ConvertTo-Json -Depth 25 | Set-Content -LiteralPath (Join-Path $k3Root "timeout-client-$number.json") -Encoding utf8
    throw "Client $number timed out: $label"
}
function Wait-K3Idle([int]$number) {
    Wait-K3 $number { param($s) -not $s.busy -and $s.status.pending -eq 0 -and -not $s.status.blocked -and $s.status.connected } 'confirmed room request'
}
function Wait-K3Local([int]$number, [long]$revision, [string]$project = '') {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        $state = Invoke-K3 $number 'komaphone.project.status'
        if ($state.error -or $state.active.error) { throw 'Local draft save failed' }
        if ($state.active.local_revision -ge $revision -and $state.active.pending -eq 0 -and
            (-not $project -or $state.active.project -eq $project)) { return $state }
        Start-Sleep -Milliseconds 20
    } while ([DateTime]::UtcNow -lt $deadline)
    $state | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $k3Root "timeout-project-$number.json") -Encoding utf8
    throw "Client $number local draft ACK timed out: expected '$project' revision >= $revision; active '$($state.active.project)' revision $($state.active.local_revision)"
}
function Save-K3Image([int]$number, [string]$name) {
    Send-K3 $number 'ui.screenshot' @{ path = "$name.png"; focus = $false } | Out-Null
}
function Save-K3Layouts([int]$number, [string]$name) {
    foreach ($size in @(@(1280,720),@(1920,1080))) {
        Send-K3 $number 'ui.resize' @{ width = $size[0]; height = $size[1] } | Out-Null
        $deadline = [DateTime]::UtcNow.AddSeconds(20)
        do {
            Start-Sleep -Milliseconds 100
            $ui = Send-K3 $number 'ui.inspect'
            if ([Math]::Abs($ui.window.width - $size[0]) -le 2 -and [Math]::Abs($ui.window.height - $size[1]) -le 2) { break }
        } while ([DateTime]::UtcNow -lt $deadline)
        if ([Math]::Abs($ui.window.width - $size[0]) -gt 2 -or [Math]::Abs($ui.window.height - $size[1]) -gt 2) { throw "Native resize did not reach $($size[0])x$($size[1])" }
        Send-K3 $number 'ui.set' @{ fit = $true } | Out-Null
        Save-K3Image $number "$name-$($size[0])"
        $script:k3Layouts.Add(@{ client = $number; image = "$name-$($size[0]).png"; requested = $size; actual = $ui.window })
    }
}
function Ready-K3([int]$number) {
    if ($script:k3ReadyRevision) {
        Wait-K3 $number { param($s) $s.status.room.revision -ge $script:k3ReadyRevision } 'preceding lobby change observed' | Out-Null
    }
    Click-K3 $number 'komaphone.room.ready'
    $ready = Wait-K3 $number { param($s) $s.status.pending -eq 0 -and ($s.status.room.members | Where-Object { $_.participant.id -eq $s.status.profile.id }).ready } 'ready accepted'
    $script:k3ReadyRevision = $ready.status.room.revision
}

function Click-K3([int]$number, [string]$id) {
    if ($Diagnostic -and $id -eq 'komaphone.room.openDraft') {
        # The same command also appears in the assigned-drawing banner. Close the
        # overlapping room window so the recorded banner is actually clickable.
        Send-K3 $number 'ui.set' @{ roomsOpen = $false } | Out-Null
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    $lastRect = ''
    $lastFrame = -1
    $stable = 0
    do {
        $ui = Send-K3 $number 'ui.inspect'
        $rect = $ui.roomControls.$id
        if ($rect -and $rect.Count -eq 4 -and $rect[0] -ge 0 -and $rect[1] -ge 0 -and $rect[2] -le $ui.window.width -and $rect[3] -le $ui.window.height) {
            $rectKey = $rect -join ','
            if ($rectKey -eq $lastRect -and $ui.frame -ne $lastFrame) { $stable++ } else { $stable = 1 }
            $lastRect = $rectKey
            $lastFrame = $ui.frame
            if ($stable -ge 3) {
                $enabled = $null
                if (($id.StartsWith('komaphone.room.') -and $id -ne 'komaphone.room.note') -or $id -eq 'komaphone.animation.copyPrevious') {
                    $command = @(Send-K3 $number 'engine.commands' | Where-Object id -eq $id)
                    if ($command.Count -ne 1) { throw "Missing engine command metadata for $id" }
                    $enabled = [bool]$command[0].enabled
                }
                $confirmed = Send-K3 $number 'ui.inspect'
                if ($enabled -ne $false -and ($confirmed.roomControls.$id -join ',') -eq $rectKey) {
                    Send-K3 $number 'ui.click' @{ x = ($rect[0] + $rect[2]) / 2; y = ($rect[1] + $rect[3]) / 2 } | Out-Null
                    $script:k3Clicks.Add(@{ client = $number; control = $id; rect = $rect; frame = $confirmed.frame; enabled = $enabled; stableFrames = $stable; at = [DateTime]::UtcNow.ToString('o') })
                    Start-Sleep -Milliseconds 200
                    return
                }
                $stable = 0
            }
        } else {
            $stable = 0
            $lastRect = ''
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Client $number native control '$id' has no visible rendered rectangle"
}
function Assert-K3Refusal([int]$number, [string]$command, [hashtable]$params = @{}) {
    $rejected = $false
    try { Invoke-K3 $number $command $params | Out-Null } catch {
        # A closed socket or crashed app must never count as an authorization refusal.
        if (-not $_.Exception.Message.StartsWith("Client $number engine.execute failed:")) { throw }
        if ($_.Exception.Message -notmatch '(?i)assign|frozen') { throw "Expected assignment authorization refusal for ${command}, got: $($_.Exception.Message)" }
        $rejected = $true
        $script:k3Refusals.Add(@{ client = $number; command = $command; error = $_.Exception.Message })
    }
    if (-not $rejected) { throw "Client $number forbidden command $command succeeded" }
}
function Wait-K3Media([int]$number, [object]$room) {
    $expected = @()
    if ($room.phase -eq 'drawing' -or $room.phase -eq 'transfer') {
        if ($room.assignment.reference.image) { $expected = @($room.assignment.reference.image.hash) }
    } else {
        $expected = @($room.reveal | Where-Object kind -eq 'artwork' | ForEach-Object { $_.image.hash } | Sort-Object -Unique)
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    do {
        $media = Invoke-K3 $number 'komaphone.room.mediaStatus'
        if ($media.error) { throw "Authorized media failed: $($media.error)" }
        $allowed = @($media.allowed | ForEach-Object { $_.hash } | Sort-Object -Unique)
        if (($allowed -join ',') -ne (($expected | Sort-Object -Unique) -join ',')) {
            @{ client = $number; expectedRoom = $room; currentRoom = (Invoke-K3 $number 'komaphone.room.status'); media = $media } | ConvertTo-Json -Depth 40 | Set-Content (Join-Path $k3Root 'media-manifest-mismatch.json')
            throw "Client $number media allowed manifest differs from observed phase $($room.phase) revision $($room.revision)"
        }
        if (@($media.cached | Where-Object { $_ -notin $expected }).Count) { throw 'Native cache retained unauthorized artwork' }
        if (@($media.cached).Count -eq $expected.Count) {
            $ui = Send-K3 $number 'ui.inspect'
            $textureHashes = @($ui.roomImageCache)
            if ((($textureHashes | Sort-Object -Unique) -join ',') -eq (($expected | Sort-Object -Unique) -join ',')) {
                $media | Add-Member -NotePropertyName 'uiTextureHashes' -NotePropertyValue $textureHashes
                return $media
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Authorized native image cache did not finish downloading'
}
function Note-K3([int]$number, [string]$note) {
    Click-K3 $number 'komaphone.room.note'
    Send-K3 $number 'ui.key' @{ key = 'A'; ctrl = $true } | Out-Null
    Send-K3 $number 'ui.type' @{ text = $note } | Out-Null
    Wait-K3 $number { param($s) $s.status.note_draft.note -eq $note -and -not $s.busy } 'mouse-typed note durably saved' | Out-Null
}
function Slider-K3([int]$number, [string]$id) {
    $ui = Send-K3 $number 'ui.inspect'
    $rect = $ui.roomControls.$id
    if (-not $rect -or $rect.Count -ne 4) { throw "Slider $id unavailable" }
    # The observed value box begins 158 points after the 150-point track. The
    # control transport's same-frame press/release cannot sustain a slider drag;
    # use the actual editable value box, never a view-state or engine setter.
    switch ($id) {
        'komaphone.animation.onionOpacity' { $property = 'onion_opacity'; $text = $(if ($ui.rooms.animation.onion_opacity -lt 0.5) { '0.65' } else { '0.4' }) }
        'komaphone.animation.speed' { $property = 'fps'; $text = $(if ($ui.rooms.animation.fps -lt 12) { '18' } else { '6' }) }
        'komaphone.animation.frame' { $property = 'frame'; $text = $(if ($ui.rooms.animation.frame -eq 0) { '2' } else { '0' }) }
        default { throw "No observed native slider track for $id" }
    }
    $before = $ui.rooms.animation.$property
    Send-K3 $number 'ui.click' @{ x = $rect[0] + 172; y = ($rect[1] + $rect[3]) / 2; count = 2 } | Out-Null
    Start-Sleep -Milliseconds 100
    Send-K3 $number 'ui.key' @{ key = 'A'; ctrl = $true } | Out-Null
    Send-K3 $number 'ui.type' @{ text = $text } | Out-Null
    Send-K3 $number 'ui.key' @{ key = 'Enter' } | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 100
        $changed = Send-K3 $number 'ui.inspect'
        $after = $changed.rooms.animation.$property
        if ($after -ne $before) {
            $script:k3Clicks.Add(@{ client = $number; control = $id; rect = $rect; valueBoxOffset = 172; interaction = 'double-click value; Ctrl+A; type; Enter'; typed = $text; before = $before; after = $after; frame = $changed.frame; at = [DateTime]::UtcNow.ToString('o') })
            return
        }
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Native slider $id did not change from $before"
}
function Draw-K3([int]$number, [int]$round, [bool]$corpse) {
    Send-K3 $number 'ui.set' @{ roomsOpen = $false; tool = 'brush'; fit = $true } | Out-Null
    Invoke-K3 $number 'tools.setBrush' @{ reset = $true; size = 16; hardness = 1; opacity = 1 } | Out-Null
    $color = @('#204080', '#208040', '#804020')[$number - 1]
    Invoke-K3 $number 'tools.setColors' @{ foreground = $color } | Out-Null
    $before = Invoke-K3 $number 'komaphone.project.status'
    $y = 100 + 40 * $round
    $beforePixel = Invoke-K3 $number 'document.pixel' @{ x = 300; y = $y + 30 }
    Send-K3 $number 'ui.pointer' @{ tool = 'brush'; events = @(
        @{ kind = 'down'; x = 100; y = $y },
        @{ kind = 'move'; x = 300; y = $y + 30 },
        @{ kind = 'up'; x = 500; y = $y }
    ) } | Out-Null
    $saved = Wait-K3Local $number ($before.active.local_revision + 1) $before.active.project
    $drawnPixel = Invoke-K3 $number 'document.pixel' @{ x = 300; y = $y + 30 }
    if (($beforePixel | ConvertTo-Json -Compress) -eq ($drawnPixel | ConvertTo-Json -Compress)) { throw 'Actual pointer gesture did not paint its sampled drawing pixel' }
    if ($corpse) {
        # Actual canvas gesture reaches the bottom edge to make the joining strip nonblank.
        $beforeEdge = Invoke-K3 $number 'document.pixel' @{ x = 400; y = 511 }
        Send-K3 $number 'ui.pointer' @{ tool = 'brush'; events = @(
            @{ kind = 'down'; x = 200; y = 511 },
            @{ kind = 'move'; x = 400; y = 511 },
            @{ kind = 'up'; x = 600; y = 511 }
        ) } | Out-Null
        $saved = Wait-K3Local $number ($saved.active.local_revision + 1) $before.active.project
        $drawnEdge = Invoke-K3 $number 'document.pixel' @{ x = 400; y = 511 }
        if (($beforeEdge | ConvertTo-Json -Compress) -eq ($drawnEdge | ConvertTo-Json -Compress)) { throw 'Actual pointer gesture did not paint the bottom seam boundary row' }
    }
    return $saved
}
function Resume-K3([int]$players) {
    Wait-K3 1 { param($s) @($s.status.room.members | Where-Object { -not $_.connected }).Count -eq 0 } 'all participant leases restored' | Out-Null
    $state = Invoke-K3 1 'komaphone.room.status'
    if ($state.status.room.paused) {
        Send-K3 1 'ui.set' @{ roomsOpen = $true } | Out-Null
        if ($Diagnostic) {
            # Provisional build's moderator row is below its scroll clip. This
            # observed native scrollbar-track point moves the row into view.
            Send-K3 1 'ui.click' @{ x = 647; y = 405 } | Out-Null
            Start-Sleep -Milliseconds 200
        }
        Click-K3 1 'komaphone.room.resume'
        Wait-K3 1 { param($s) -not $s.status.room.paused -and -not $s.busy } 'resumed after reconnect' | Out-Null
        Send-K3 1 'ui.set' @{ roomsOpen = $false } | Out-Null
    }
}
function Play-K3([int]$players, [bool]$corpse) {
    $label = if ($corpse) { "corpse-$players" } else { "animation-$players" }
    Write-Output "K3 ${label}: configuring actual native clients"
    $mode = if ($corpse) { @{ kind = 'exquisite_corpse' } } else { @{ kind = 'animation'; rotations = 2 } }
    Invoke-K3 1 'komaphone.room.options' @{ mode = $mode; timer_ms = $null } | Out-Null
    $configured = Wait-K3Idle 1
    $script:k3ReadyRevision = $configured.status.room.revision
    foreach ($number in 1..$players) {
        Send-K3 $number 'ui.set' @{ roomsOpen = $true } | Out-Null
        Ready-K3 $number
    }
    Wait-K3 1 { param($s) @($s.status.room.members | Where-Object { -not $_.ready }).Count -eq 0 } 'all Ready clicks accepted' | Out-Null
    Save-K3Image 1 "$label-lobby"
    Click-K3 1 'komaphone.room.start'
    Wait-K3 1 { param($s) $s.status.room.phase -eq 'drawing' -and -not $s.busy } 'native Start click accepted' | Out-Null
    $rounds = if ($corpse) { 3 } else { 2 * $players }
    $frames = [System.Collections.Generic.List[object]]::new()
    for ($round = 0; $round -lt $rounds; $round++) {
        Write-Output "K3 ${label}: drawing/submitting round $($round + 1) of $rounds"
        foreach ($number in 1..$players) {
            Send-K3 $number 'ui.set' @{ roomsOpen = $true } | Out-Null
            $state = Wait-K3 $number { param($s) $s.status.room.phase -eq 'drawing' -and $s.status.room.round -eq $round -and -not $s.busy } 'current round observed'
            $assignment = $state.status.room.assignment.assignment
            if (($assignment.id.chain + $round) % $players -ne $number - 1 -or $assignment.player -ne $state.status.profile.id) { throw 'Cyclic assignment mismatch' }
            if ($state.status.room.reveal.Count) { throw 'Premature reveal in native projection' }
            if ($round -eq 0 -and $state.status.room.assignment.reference) { throw 'First round delivered reference content' }
            if ($round -gt 0) {
                $kind = if ($corpse) { 'seam' } else { 'animation' }
                if ($state.status.room.assignment.reference.kind -ne $kind) { throw 'Wrong phase-filtered reference' }
            }
            Click-K3 $number 'komaphone.room.openDraft'
            $opened = Wait-K3Local $number 1 $assignment.project
            $media = Wait-K3Media $number $state.status.room
            if (@($media.notes).Count) { throw 'Pre-reveal native note cache is populated' }
            $dimensions = Invoke-K3 $number 'document.inspect'
            $expectedWidth = if ($corpse) { 1024 } else { 1536 }
            $expectedHeight = if ($corpse) { 512 } else { 1024 }
            if ($dimensions.width -ne $expectedWidth -or $dimensions.height -ne $expectedHeight) { throw 'Assigned canvas dimensions are incorrect' }
            Assert-K3Refusal $number 'image.canvasSize' @{ width = 64; height = 64 }
            $note = if ($corpse) { '' } else { "K3 note $number/$round" }
            if (-not $corpse) {
                Note-K3 $number $note
                if ($round -gt 0) {
                    $prior = @($frames | Where-Object { $_.assignment.id.chain -eq $assignment.id.chain -and $_.assignment.id.round -eq $round - 1 })
                    if ($prior.Count -ne 1 -or $state.status.room.assignment.reference.note -ne $prior[0].note) { throw 'Previous-artist note did not follow its chain' }
                    $blank = Invoke-K3 $number 'document.pixel' @{ x = 300; y = 100 + 40 * ($round - 1) + 30 }
                    if ($round -eq 1) {
                        $beforeOnion = Send-K3 $number 'ui.inspect'
                        if (-not $beforeOnion.rooms.animation.onion_skin) { Click-K3 $number 'komaphone.animation.onion' }
                        Slider-K3 $number 'komaphone.animation.onionOpacity'
                        $onion = Send-K3 $number 'ui.inspect'
                        if (-not $onion.rooms.animation.onion_skin -or $onion.rooms.animation.onion_opacity -lt 0 -or $onion.rooms.animation.onion_opacity -gt 1) { throw 'Onion skin control did not update' }
                        $unchanged = Invoke-K3 $number 'document.pixel' @{ x = 300; y = 130 }
                        if (($blank | ConvertTo-Json -Compress) -ne ($unchanged | ConvertTo-Json -Compress)) { throw 'Onion overlay entered document pixels' }
                        $still = Invoke-K3 $number 'komaphone.project.status'
                        if ($still.active.local_revision -ne $opened.active.local_revision) { throw 'Read-only onion/reference changed the local drawing revision' }
                        Click-K3 $number 'komaphone.animation.copyPrevious'
                        Wait-K3Local $number ($opened.active.local_revision + 1) $assignment.project | Out-Null
                        $copied = Invoke-K3 $number 'document.pixel' @{ x = 300; y = 130 }
                        if (($copied | ConvertTo-Json -Compress) -ne ($prior[0].pixel | ConvertTo-Json -Compress)) { throw 'Explicit previous-frame copy differs from accepted pixels' }
                        Save-K3Image $number "$label-copy-onion-$number"
                    }
                }
            }
            $saved = Draw-K3 $number $round $corpse
            $pixel = Invoke-K3 $number 'document.pixel' @{ x = 300; y = 100 + 40 * $round + 30 }
            if ($players -eq 2 -and -not $corpse -and $round -eq 1 -and $number -eq 2) {
                $old = @($frames | Where-Object { $_.assignment.player -eq $state.status.profile.id -and $_.assignment.id.round -eq 0 })[0]
                Stop-K3App 2
                Start-K3App 2
                $scoped = Wait-K3 2 { param($s) $s.status.connected -and $s.status.game_drafts.projects.Count -eq 2 } 'own assignment scope restored after restart'
                if ($old.assignment.project -notin $scoped.status.game_drafts.projects -or $assignment.project -notin $scoped.status.game_drafts.projects) { throw 'Restart lost current game project ownership scope' }
                Invoke-K3 2 'komaphone.project.open' @{ project = $old.assignment.project } | Out-Null
                Wait-K3Local 2 $old.revision $old.assignment.project | Out-Null
                Assert-K3Refusal 2 'paint.stroke' @{ points = @(@(10,10),@(20,20)); size = 10 }
                $oldPixel = Invoke-K3 2 'document.pixel' @{ x = 300; y = 130 }
                if (($oldPixel | ConvertTo-Json -Compress) -ne ($old.pixel | ConvertTo-Json -Compress)) { throw 'Previous accepted local drawing changed during scope test' }
                Click-K3 2 'komaphone.room.openDraft'
                Wait-K3Local 2 $saved.active.local_revision $assignment.project | Out-Null
                Resume-K3 $players
                $script:k3Evidence.previousTurnScope = @{ projects = $scoped.status.game_drafts.projects; oldReadable = $true; oldMutationRejected = $true; reopened = $true }
            }
            if ($players -eq 2 -and -not $corpse -and $round -eq 0 -and $number -eq 2) {
                # Kill after an acknowledged canvas edit, then restore the exact assigned project.
                Stop-K3App 2
                Start-K3App 2
                $restored = Wait-K3 2 { param($s) $s.status.connected -and $s.status.room.assignment } 'editing reconnect'
                if ($restored.status.room.assignment.assignment.project -ne $assignment.project -or $restored.status.profile.id -ne $state.status.profile.id) { throw 'Editing reconnect changed assignment or identity' }
                if ($restored.status.note_draft.note -ne $note) { throw 'Editing reconnect lost acknowledged handoff note' }
                Click-K3 2 'komaphone.room.openDraft'
                Wait-K3Local 2 $saved.active.local_revision $assignment.project | Out-Null
                $recovered = Invoke-K3 2 'document.pixel' @{ x = 300; y = 130 }
                if (($recovered | ConvertTo-Json -Compress) -ne ($pixel | ConvertTo-Json -Compress)) { throw 'Editing reconnect lost saved pixels' }
                $script:k3Evidence.editingReconnect = @{ project = $assignment.project; revision = $saved.active.local_revision; identicalPixel = $true }
                Resume-K3 $players
            }
            if ($players -eq 2 -and -not $corpse -and $round -eq 1 -and $number -eq 1) {
                # Persist an actual submission offline, kill the submitting client, then retry.
                Stop-K3Service
                Wait-K3 1 { param($s) -not $s.status.connected } 'service outage before submission' | Out-Null
                Click-K3 1 'komaphone.room.submit'
                Wait-K3 1 { param($s) $s.status.pending -gt 0 } 'submission persisted in offline outbox' 90 | Out-Null
                Stop-K3App 1
                Start-K3Service | Out-Null
                Start-K3App 1
                Wait-K3 1 { param($s) $s.status.room.assignment.submitted -and $s.status.pending -eq 0 } 'queued submission accepted after process restart' 90 | Out-Null
                Invoke-K3 1 'komaphone.project.open' @{ project = $assignment.project } | Out-Null
                Wait-K3Local 1 $saved.active.local_revision $assignment.project | Out-Null
                Resume-K3 $players
                $script:k3Evidence.uploadReconnect = @{ project = $assignment.project; revision = $saved.active.local_revision; accepted = $true }
            } else {
                Click-K3 $number 'komaphone.room.submit'
            }
            $accepted = Wait-K3 $number { param($s) -not $s.busy -and ($s.status.room.assignment.submitted -or $s.status.room.round -gt $round -or $s.status.room.phase -eq 'reveal') } 'native Submit click durably accepted' 90
            if ($accepted.status.room.phase -eq 'drawing' -and $accepted.status.room.round -eq $round) {
                Assert-K3Refusal $number 'paint.stroke' @{ points = @(@(10,10),@(20,20)); size = 10 }
            }
            $frames.Add(@{ assignment = $assignment; revision = $saved.active.local_revision; pixel = $pixel; note = $note; cache = $media })
            if ($round -eq 1 -and $number -eq 1) {
                Send-K3 $number 'ui.set' @{ roomsOpen = $false } | Out-Null
                Save-K3Image $number "$label-reference"
                if ($players -eq 2) { Save-K3Layouts $number "$label-reference" }
            }
        }
    }
    $revealed = Wait-K3 1 { param($s) $s.status.room.phase -eq 'reveal' -and $s.status.room.reveal.Count -eq $players * $rounds } 'full native game reveal' 90
    # The host and peer poll independently. Assert the peer's grants only after
    # that peer has observed the same authoritative reveal; a host observation
    # cannot make the peer's previous-round reference immediately obsolete.
    $revealGame = $revealed.status.room.reveal[0].assignment.id.game
    $revealPeers = @{}
    foreach ($number in 1..$players) {
        $peerReveal = Wait-K3 $number { param($s) $s.status.room.id -eq $revealed.status.room.id -and $s.status.room.phase -eq 'reveal' -and $s.status.room.epoch -eq $revealed.status.room.epoch -and $s.status.room.revision -ge $revealed.status.room.revision -and $s.status.game_drafts.game -eq $revealGame -and $s.status.room.reveal.Count -eq $players * $rounds } 'peer observed same game and full native reveal' 90
        if (($peerReveal.status.room.reveal | ConvertTo-Json -Depth 30 -Compress) -ne ($revealed.status.room.reveal | ConvertTo-Json -Depth 30 -Compress)) { throw 'Native peers disagree about authoritative reveal' }
        $peerMedia = Wait-K3Media $number $peerReveal.status.room
        if (@($peerMedia.notes).Count) { throw 'Reveal downloaded notes before explicit viewer action' }
        $revealPeers["$number"] = @{ revision = $peerReveal.status.room.revision; epoch = $peerReveal.status.room.epoch; game = $revealGame; media = $peerMedia }
    }
    Send-K3 1 'ui.set' @{ roomsOpen = $false } | Out-Null
    Send-K3 2 'ui.set' @{ roomsOpen = $false } | Out-Null
    $revealMedia = $revealPeers['2'].media
    if (@($revealMedia.notes).Count) { throw 'Reveal downloaded notes before explicit viewer action' }
    foreach ($entry in $revealed.status.room.reveal) {
        if ($entry.kind -ne 'artwork') { throw 'Full game unexpectedly contains a missing drawing' }
        $frame = @($frames | Where-Object { $_.assignment.project -eq $entry.assignment.project })
        if ($frame.Count -ne 1 -or $entry.revision -ne $frame[0].revision) { throw 'Reveal lost exact accepted project/revision' }
    }
    if ($corpse) {
        Click-K3 2 'corpse.chain.1'
        Click-K3 2 'corpse.reveal.head'
        Click-K3 2 'corpse.reveal.torso'
        Click-K3 2 'corpse.reveal.full'
        $viewed = Send-K3 2 'ui.inspect'
        if ($viewed.rooms.corpse_view.chain -ne 1 -or $viewed.rooms.corpse_view.visible_parts -ne 3) { throw 'Part-by-part/full creature reveal controls did not apply' }
        $position = @{ chain = 1; frame = 0; visible_parts = 3 }
    } else {
        Click-K3 2 'komaphone.animation.chain.1'
        $initialView = Send-K3 2 'ui.inspect'
        Click-K3 2 'komaphone.animation.play'
        $playDeadline = [DateTime]::UtcNow.AddSeconds(5)
        do {
            $playing = Send-K3 2 'ui.inspect'
            if ($playing.rooms.animation.playing -and $playing.rooms.animation.frame -ne $initialView.rooms.animation.frame) { break }
            Start-Sleep -Milliseconds 80
        } while ([DateTime]::UtcNow -lt $playDeadline)
        if (-not $playing.rooms.animation.playing -or $playing.rooms.animation.frame -eq $initialView.rooms.animation.frame) { throw 'Whole-animation playback did not advance actual frames' }
        Click-K3 2 'komaphone.animation.play'
        Click-K3 2 'komaphone.animation.loop'
        Slider-K3 2 'komaphone.animation.speed'
        Slider-K3 2 'komaphone.animation.frame'
        $viewed = Send-K3 2 'ui.inspect'
        if ($viewed.rooms.animation.playing -or $viewed.rooms.animation.frame -lt 0 -or $viewed.rooms.animation.frame -ge $rounds -or $viewed.rooms.animation.fps -lt 1 -or $viewed.rooms.animation.fps -gt 24) { throw 'Pause/scrub/speed controls did not apply' }
        Click-K3 2 'komaphone.animation.notes'
        $noteDeadline = [DateTime]::UtcNow.AddSeconds(30)
        do {
            $notes = Invoke-K3 2 'komaphone.room.mediaStatus'
            if (@($notes.notes).Count -eq $players * $rounds) { break }
            Start-Sleep -Milliseconds 100
        } while ([DateTime]::UtcNow -lt $noteDeadline)
        if (@($notes.notes).Count -ne $players * $rounds) { throw 'Explicit notes action did not fetch every handoff note' }
        foreach ($frame in $frames) {
            $matches = 0
            foreach ($record in $notes.notes) {
                if ($record[0].chain -eq $frame.assignment.id.chain -and $record[0].round -eq $frame.assignment.id.round) {
                    if ($record[1] -ne $frame.note) { throw 'Revealed notes lost their assignment metadata' }
                    $matches++
                }
            }
            if ($matches -ne 1) { throw 'Revealed notes omitted or duplicated an assignment' }
        }
        $position = @{ chain = $viewed.rooms.animation.chain; frame = $viewed.rooms.animation.frame; fps = $viewed.rooms.animation.fps; looping = $viewed.rooms.animation.looping }
    }
    Wait-K3 2 { param($s) -not $s.busy -and $s.status.reveal_position.chain -eq $position.chain -and $s.status.reveal_position.frame -eq $position.frame } 'reveal viewer position durably saved' | Out-Null
    Save-K3Image 1 "$label-reveal"
    Save-K3Image 2 "$label-reveal-controls"
    if ($players -eq 2) { Save-K3Layouts 2 "$label-reveal" }
    # A real process restart during reveal must restore the complete room and its phase.
    $before = $revealed.status.room.reveal | ConvertTo-Json -Depth 30 -Compress
    Stop-K3App 2
    Start-K3App 2
    $reopened = Wait-K3 2 { param($s) $s.status.connected -and $s.status.room.id -eq $revealed.status.room.id -and $s.status.room.phase -eq 'reveal' -and $s.status.room.epoch -eq $revealed.status.room.epoch -and $s.status.room.revision -ge $revealed.status.room.revision -and $s.status.game_drafts.game -eq $revealGame } 'same game reveal reconnect' 90
    if (($reopened.status.room.reveal | ConvertTo-Json -Depth 30 -Compress) -ne $before) { throw 'Reveal reconnect changed artwork or credits' }
    Send-K3 2 'ui.set' @{ roomsOpen = $false } | Out-Null
    Wait-K3Media 2 $reopened.status.room | Out-Null
    $restoredView = Send-K3 2 'ui.inspect'
    if ($corpse) {
        if ($restoredView.rooms.corpse_view.chain -ne $position.chain -or $restoredView.rooms.corpse_view.visible_parts -ne $position.visible_parts) { throw 'Creature reveal navigation did not survive process restart' }
    } elseif ($restoredView.rooms.animation.chain -ne $position.chain -or $restoredView.rooms.animation.frame -ne $position.frame -or $restoredView.rooms.animation.fps -ne $position.fps -or $restoredView.rooms.animation.looping -ne $position.looping) { throw 'Animation reveal navigation did not survive process restart' }
    Save-K3Image 2 "$label-reveal-reopened"
    $script:k3Games.Add(@{ label = $label; players = $players; rounds = $rounds; frames = $frames.ToArray(); reveal = $revealed.status.room.reveal; revealMedia = $revealMedia; revealPeers = $revealPeers; position = $position; revealReconnect = $true })
    Write-Output "K3 ${label}: complete with reveal/restart and authorized cache evidence"
    Send-K3 1 'ui.set' @{ roomsOpen = $false } | Out-Null
    Click-K3 1 'komaphone.room.next'
    Wait-K3 1 { param($s) $s.status.room.phase -eq 'lobby' -and -not $s.busy } 'next game lobby' | Out-Null
}

function Transfer-K3 {
    Write-Output 'K3: testing timed Transfer reconnect and incomplete archive'
    Invoke-K3 1 'komaphone.room.options' @{ mode = @{ kind = 'animation'; rotations = 1 }; timer_ms = 10000 } | Out-Null
    $configured = Wait-K3Idle 1
    $script:k3ReadyRevision = $configured.status.room.revision
    foreach ($number in 1..3) { Send-K3 $number 'ui.set' @{ roomsOpen = $true } | Out-Null; Ready-K3 $number }
    Wait-K3 1 { param($s) @($s.status.room.members | Where-Object { -not $_.ready }).Count -eq 0 } 'all timed-game Ready clicks accepted' | Out-Null
    Click-K3 1 'komaphone.room.start'
    $started = Wait-K3 1 { param($s) $s.status.room.phase -eq 'drawing' -and -not $s.busy } 'timed game started'
    $project = $started.status.room.assignment.assignment.project
    Click-K3 1 'komaphone.room.openDraft'
    Wait-K3Local 1 1 $project | Out-Null
    $saved = Draw-K3 1 0 $false
    # No Submit: the service deadline must freeze and accept the last local save.
    $transfer = Wait-K3 1 { param($s) $s.status.room.phase -eq 'transfer' -and $s.status.room.assignment.submitted -and $s.status.pending -eq 0 } 'automatic deadline transfer' 45
    $pin = $transfer.status.freeze_pin
    if ($pin.project -ne $project -or $pin.revision -ne $saved.active.local_revision) { throw 'Deadline froze a different saved drawing' }
    Stop-K3Service
    Stop-K3App 1
    Start-K3Service | Out-Null
    Start-K3App 1
    $restored = Wait-K3 1 { param($s) $s.status.connected -and $s.status.room.phase -eq 'transfer' -and $s.status.room.assignment.submitted } 'Transfer phase restored after both endpoints restarted' 90
    if ($restored.status.room.epoch -ne $transfer.status.room.epoch -or
        ($restored.status.freeze_pin | ConvertTo-Json -Compress) -ne ($pin | ConvertTo-Json -Compress) -or
        $restored.status.room.assignment.assignment.project -ne $project) { throw 'Transfer reconnect changed epoch/assignment/freeze pin' }
    Save-K3Image 1 'transfer-reconnect'
    $script:k3Evidence.transferReconnect = @{ project = $project; epoch = $restored.status.room.epoch; freezePin = $pin; accepted = $true; paused = $restored.status.room.paused }
    # Explicitly retain an absent participant as a placeholder. Resume must be
    # available after Skip even though that member remains disconnected.
    $missing = (Invoke-K3 3 'komaphone.room.status').status.profile.id
    Stop-K3App 3
    $offline = Wait-K3 1 { param($s) $s.status.room.paused -and @($s.status.room.members | Where-Object { $_.participant.id -eq $missing -and -not $_.connected }).Count -eq 1 -and @($s.status.room.members | Where-Object { $_.participant.id -ne $missing -and -not $_.connected }).Count -eq 0 } 'one missing participant with live host and friend' 45
    Send-K3 1 'ui.set' @{ roomsOpen = $true } | Out-Null
    Click-K3 1 'komaphone.room.skip'
    $skipped = Wait-K3 1 { param($s) -not $s.busy -and $s.status.pending -eq 0 -and $s.status.room.revision -gt $offline.status.room.revision } 'actual host Skip accepted'
    Click-K3 1 'komaphone.room.resume'
    $resumed = Wait-K3 1 { param($s) -not $s.busy -and -not $s.status.room.paused -and $s.status.room.phase -eq 'transfer' } 'actual host Resume accepted after offline Skip'
    if (@($resumed.status.room.members | Where-Object { $_.participant.id -eq $missing -and -not $_.connected }).Count -ne 1) { throw 'Skip/Resume fixture unexpectedly reconnected the absent participant' }
    $script:k3Evidence.skipResume = @{ participant = $missing; skippedRevision = $skipped.status.room.revision; resumedRevision = $resumed.status.room.revision; stillDisconnected = $true }
    Save-K3Image 1 'skipped-offline-resumed'
    Click-K3 1 'komaphone.room.end'
    $archived = Wait-K3 1 { param($s) $s.status.room.phase -eq 'archived' -and -not $s.busy } 'incomplete timed fixture archived'
    $script:k3Evidence.incompleteArchive = $archived.status.room.reveal
    if (@($archived.status.room.reveal | Where-Object kind -eq 'artwork').Count -ne 1 -or @($archived.status.room.reveal | Where-Object kind -eq 'missing').Count -ne 8) { throw 'Incomplete timed archive lost exact artwork/missing entries' }
    $placeholder = @($archived.status.room.reveal | Where-Object { $_.kind -eq 'missing' -and $_.assignment.player -eq $missing -and $_.assignment.id.round -eq 0 })
    if ($placeholder.Count -ne 1 -or $placeholder[0].reason -notmatch '(?i)skipped') { throw 'Explicit offline placeholder was not preserved in the incomplete archive' }
    Save-K3Image 1 'timed-incomplete-archive'
}

$script:k3Clicks = [System.Collections.Generic.List[object]]::new()
$script:k3Games = [System.Collections.Generic.List[object]]::new()
$script:k3Layouts = [System.Collections.Generic.List[object]]::new()
$script:k3Refusals = [System.Collections.Generic.List[object]]::new()
try {
    $endpoint = Start-K3Service
    foreach ($number in 1..3) {
        Start-K3App $number
        Invoke-K3 $number 'komaphone.room.connect' @{ endpoint = $endpoint; display_name = "K3 Player $number" } | Out-Null
        Wait-K3 $number { param($s) $s.status.paired -and $s.status.connected -and -not $s.busy } 'paired isolated profile' | Out-Null
    }
    Invoke-K3 1 'komaphone.room.create' @{ mode = @{ kind = 'animation'; rotations = 2 }; timer_ms = $null } | Out-Null
    $created = Wait-K3 1 { param($s) $s.status.room -and -not $s.busy } 'room created'
    $room = $created.status.room.id
    Invoke-K3 1 'komaphone.room.invite' @{ expires_in_ms = 3600000; uses = 2 } | Out-Null
    Wait-K3 1 { param($s) $s.status.invitation_id -and -not $s.busy } 'invitation committed' | Out-Null
    $invitation = Invoke-K3 1 'komaphone.room.invitation'
    Invoke-K3 2 'komaphone.room.join' @{ room = $room; invitation = $invitation.invitation } | Out-Null
    Wait-K3 1 { param($s) $s.status.room.members.Count -eq 2 } 'two-player lobby' | Out-Null
    Play-K3 2 $false
    Play-K3 2 $true
    Invoke-K3 3 'komaphone.room.join' @{ room = $room; invitation = $invitation.invitation } | Out-Null
    $invitation = $null
    Wait-K3 1 { param($s) $s.status.room.members.Count -eq 3 } 'three-player lobby' | Out-Null
    Play-K3 3 $false
    Play-K3 3 $true
    Transfer-K3
    $script:k3Evidence.games = $script:k3Games.ToArray()
    $script:k3Evidence.nativeClicks = $script:k3Clicks.ToArray()
    $script:k3Evidence.layouts = $script:k3Layouts.ToArray()
    $script:k3Evidence.commandRefusals = $script:k3Refusals.ToArray()
    $script:k3Evidence.passed = $true
    $script:k3Evidence | ConvertTo-Json -Depth 45 | Set-Content -LiteralPath (Join-Path $k3Root 'result.json') -Encoding utf8
    Write-Output "PASS: four complete native games, mouse Ready/Start/Open/drawing/Submit, cyclic assignments, durable reconnect while editing/upload/reveal. Artifacts: $k3Root"
} catch {
    $failure = $_
    $failure | Out-String | Set-Content -LiteralPath (Join-Path $k3Root 'failure.log') -Encoding utf8
    $script:k3Evidence.games = $script:k3Games.ToArray()
    $script:k3Evidence.nativeClicks = $script:k3Clicks.ToArray()
    $script:k3Evidence.layouts = $script:k3Layouts.ToArray()
    $script:k3Evidence.commandRefusals = $script:k3Refusals.ToArray()
    $script:k3Evidence.passed = $false
    $script:k3Evidence | ConvertTo-Json -Depth 45 | Set-Content -LiteralPath (Join-Path $k3Root 'result.json') -Encoding utf8
    foreach ($number in @(1,2,3)) {
        try {
            Send-K3 $number 'ui.inspect' | ConvertTo-Json -Depth 35 | Set-Content -LiteralPath (Join-Path $k3Root "failure-ui-$number.json") -Encoding utf8
            Invoke-K3 $number 'komaphone.room.status' | ConvertTo-Json -Depth 35 | Set-Content -LiteralPath (Join-Path $k3Root "failure-room-$number.json") -Encoding utf8
            Invoke-K3 $number 'komaphone.project.status' | ConvertTo-Json -Depth 35 | Set-Content -LiteralPath (Join-Path $k3Root "failure-project-$number.json") -Encoding utf8
            Save-K3Image $number 'failure'
        } catch { $_ | Out-String | Set-Content -LiteralPath (Join-Path $k3Root "failure-capture-$number.log") -Encoding utf8 }
    }
    throw $failure
} finally {
    foreach ($number in @(1,2,3)) { Stop-K3App $number }
    Stop-K3Service
    $cleanupFailures = [System.Collections.Generic.List[int]]::new()
    $cleanupResults = [System.Collections.Generic.List[object]]::new()
    foreach ($number in @(1,2,3)) {
        $database = Join-Path $k3Root "client-$number/data/rooms.sqlite3"
        if ((Test-Path -LiteralPath $database) -and (Test-Path -LiteralPath $CredentialCleanupExe)) {
            try {
                & $CredentialCleanupExe --discard-isolated-test-credentials $database *> (Join-Path $k3Root "client-$number/credentials-cleanup.log")
                $cleanupResults.Add(@{ client = $number; exitCode = $LASTEXITCODE; log = "client-$number/credentials-cleanup.log" })
                if ($LASTEXITCODE -ne 0) { $cleanupFailures.Add($number) }
            } catch { $cleanupFailures.Add($number); $cleanupResults.Add(@{ client = $number; error = $_.ToString() }) }
        }
    }
    $env:KOMAPHONE_DATA_DIR = $k3OriginalDataRoot
    $env:PHOTOCRAFT_CONFIG_DIR = $k3OriginalConfigRoot
    $cleanupResults.ToArray() | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $k3Root 'cleanup.json') -Encoding utf8
    if ($cleanupFailures.Count) {
        $script:k3Evidence.passed = $false
        $script:k3Evidence.cleanupFailed = $cleanupFailures.ToArray()
        $script:k3Evidence | ConvertTo-Json -Depth 45 | Set-Content -LiteralPath (Join-Path $k3Root 'result.json') -Encoding utf8
        throw "Isolated credential cleanup failed for client(s) $($cleanupFailures -join ', '); see cleanup.json"
    }
}
