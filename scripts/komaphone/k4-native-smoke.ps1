# Isolated real-process K4 gallery acceptance. Requires the native app, service, and the
# room-client credential cleanup example; all output stays under repo/log/k4.
# The multiplayer service and each app's automation channel bind only loopback.
param(
    [string]$AppExe = '',
    [string]$ServiceExe = '',
    [string]$CredentialCleanupExe = '',
    [switch]$NativeFilesystemClicks,
    [switch]$ScopedFilesystem,
    [switch]$Diagnostic
)
$ErrorActionPreference = 'Stop'
if ($NativeFilesystemClicks -and $ScopedFilesystem) { throw 'Choose native filesystem clicks or scoped filesystem automation, not both' }
$k4Repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if (-not $AppExe) {
    $AppExe = Join-Path $k4Repo 'log/k4/bin/photocraft.exe'
    if (-not (Test-Path -LiteralPath $AppExe)) { $AppExe = Join-Path $k4Repo 'target/agent-root/debug/photocraft.exe' }
}
if (-not $ServiceExe) { $ServiceExe = Join-Path $k4Repo 'log/k4/bin/photocraft-sync.exe' }
if (-not $CredentialCleanupExe) { $CredentialCleanupExe = Join-Path $k4Repo 'log/k4/bin/cleanup_credentials.exe' }
$AppExe = (Resolve-Path -LiteralPath $AppExe).Path
$ServiceExe = (Resolve-Path -LiteralPath $ServiceExe).Path
$CredentialCleanupExe = (Resolve-Path -LiteralPath $CredentialCleanupExe).Path
$k4OriginalDataRoot = $env:KOMAPHONE_DATA_DIR
$k4OriginalConfigRoot = $env:PHOTOCRAFT_CONFIG_DIR
$k4OriginalTrust = $env:KOMAPHONE_TRUSTED_CERT
$k4OriginalEndpoint = $env:KOMAPHONE_SERVICE_ENDPOINT
$env:KOMAPHONE_TRUSTED_CERT = $null
$env:KOMAPHONE_SERVICE_ENDPOINT = $null
$k4Root = Join-Path (Join-Path $k4Repo 'log/k4') ('native-gallery-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $k4Root | Out-Null
$script:k4Apps = @{}
$script:k4Service = $null
$script:k4ServicePort = 0
$script:k4ServiceGeneration = 0
$script:k4Evidence = [ordered]@{
    validation = $(if ($Diagnostic) { 'diagnostic-only; final refreshed build requires complete rerun' } else { 'acceptance-candidate' })
    appHash = (Get-FileHash -LiteralPath $AppExe -Algorithm SHA256).Hash
    serviceHash = (Get-FileHash -LiteralPath $ServiceExe -Algorithm SHA256).Hash
    filesystemMode = $(if ($ScopedFilesystem) { 'capability-scoped app.galleryFile; rendered controls verified; no native file clicks' } else { 'observed native filesystem clicks' })
}

function New-K4Port {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    return $port
}
function Start-K4Service {
    $script:k4ServiceGeneration++
    $generation = $script:k4ServiceGeneration
    $root = Join-Path $k4Root 'authority'
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $stdout = Join-Path $k4Root "service-$generation.stdout.log"
    $stderr = Join-Path $k4Root "service-$generation.stderr.log"
    $arguments = @('--bind', "127.0.0.1:$script:k4ServicePort", '--data-dir', ('"' + $root + '"'))
    $script:k4Service = Start-Process -FilePath $ServiceExe -ArgumentList $arguments -WindowStyle Hidden -PassThru `
        -WorkingDirectory $k4Repo -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {
        if ($script:k4Service.HasExited) { throw "Service exited: $($script:k4Service.ExitCode); see $stderr" }
        $ready = Get-Content -LiteralPath $stdout -ErrorAction SilentlyContinue | Where-Object { $_ -match '^\{' } | Select-Object -First 1
        if ($ready) {
            $result = $ready | ConvertFrom-Json
            if ($result.ready) {
                $script:k4ServicePort = [int]($result.address -split ':')[-1]
                return "http://127.0.0.1:$script:k4ServicePort"
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Service readiness timed out'
}
function Stop-K4Service {
    if ($script:k4Service -and -not $script:k4Service.HasExited) {
        Stop-Process -Id $script:k4Service.Id -Force
        $script:k4Service.WaitForExit()
    }
    $script:k4Service = $null
}
function Connect-K4Control([hashtable]$app) {
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
function Send-K4([int]$number, [string]$method, [hashtable]$params = @{}) {
    $app = $script:k4Apps[$number]
    if (-not $app -or $app.Process.HasExited) { throw "Client $number is not running" }
    if (-not $app.Client -or ([DateTime]::UtcNow - $app.LastUse).TotalSeconds -gt 20) { Connect-K4Control $app }
    $frame = @{ id = [guid]::NewGuid().ToString('N'); method = $method; params = $params } | ConvertTo-Json -Depth 35 -Compress
    $app.Writer.WriteLine($frame)
    $line = $app.Reader.ReadLine()
    if (-not $line) { throw "Client $number closed control during $method; request may have completed" }
    $reply = $line | ConvertFrom-Json
    $app.LastUse = [DateTime]::UtcNow
    if (-not $reply.ok) { throw "Client $number $method failed: $($reply.error)" }
    return $reply.result
}
function Invoke-K4([int]$number, [string]$command, [hashtable]$params = @{}) {
    Send-K4 $number 'engine.execute' @{ command = $command; params = $params }
}
function Start-K4App([int]$number, [string]$dataRoot = '') {
    $root = Join-Path $k4Root "client-$number"
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $previous = $script:k4Apps[$number]
    $generation = if ($previous) { $previous.Generation + 1 } else { 1 }
    if (-not $dataRoot) { $dataRoot = Join-Path $root 'data' }
    $env:KOMAPHONE_DATA_DIR = $dataRoot
    $env:PHOTOCRAFT_CONFIG_DIR = Join-Path $root 'config'
    $tokenPath = Join-Path $root 'control.token'
    $port = New-K4Port
    $arguments = @('--new-instance', '--control', "$port", '--control-token-file', ('"' + $tokenPath + '"'),
        '--automation-read-root', ('"' + $root + '"'), '--automation-write-root', ('"' + $root + '"'))
    $process = Start-Process -FilePath $AppExe -ArgumentList $arguments -WindowStyle Hidden -PassThru -WorkingDirectory $k4Repo `
        -RedirectStandardOutput (Join-Path $root "app-$generation.stdout.log") -RedirectStandardError (Join-Path $root "app-$generation.stderr.log")
    $app = @{ Process = $process; Root = $root; DataRoot = $dataRoot; TokenPath = $tokenPath; Port = $port; Generation = $generation }
    $script:k4Apps[$number] = $app
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    do {
        if ($process.HasExited) { throw "Client $number exited before control startup: $($process.ExitCode)" }
        try { Connect-K4Control $app; break } catch { Start-Sleep -Milliseconds 100 }
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $app.Client) { throw "Client $number control startup timed out" }
    Send-K4 $number 'ui.set' @{ roomsOpen = $true } | Out-Null
}
function Stop-K4App([int]$number) {
    $app = $script:k4Apps[$number]
    if ($app) {
        if ($app.Client) { $app.Client.Dispose(); $app.Client = $null }
        if ($app.Process -and -not $app.Process.HasExited) { Stop-Process -Id $app.Process.Id -Force; $app.Process.WaitForExit() }
    }
}
function Wait-K4([int]$number, [scriptblock]$condition, [string]$label, [int]$seconds = 40) {
    $deadline = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $state = Invoke-K4 $number 'komaphone.room.status'
        if (& $condition $state) { return $state }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    $state | ConvertTo-Json -Depth 25 | Set-Content -LiteralPath (Join-Path $k4Root "timeout-client-$number.json") -Encoding utf8
    throw "Client $number timed out: $label"
}
function Wait-K4Idle([int]$number) {
    Wait-K4 $number { param($s) -not $s.busy -and $s.status.pending -eq 0 -and -not $s.status.blocked -and $s.status.connected } 'confirmed room request'
}
function Wait-K4Local([int]$number, [long]$revision, [string]$project = '') {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        $state = Invoke-K4 $number 'komaphone.project.status'
        if ($state.error -or $state.active.error) { throw 'Local draft save failed' }
        if ($state.active.local_revision -ge $revision -and $state.active.pending -eq 0 -and
            (-not $project -or $state.active.project -eq $project)) { return $state }
        Start-Sleep -Milliseconds 20
    } while ([DateTime]::UtcNow -lt $deadline)
    $state | ConvertTo-Json -Depth 20 | Set-Content -LiteralPath (Join-Path $k4Root "timeout-project-$number.json") -Encoding utf8
    throw "Client $number local draft ACK timed out: expected '$project' revision >= $revision; active '$($state.active.project)' revision $($state.active.local_revision)"
}
function Save-K4Image([int]$number, [string]$name) {
    Send-K4 $number 'ui.screenshot' @{ path = "$name.png"; focus = $false } | Out-Null
}
function Save-K4Layouts([int]$number, [string]$name) {
    foreach ($size in @(@(1280,720),@(1920,1080))) {
        Send-K4 $number 'ui.resize' @{ width = $size[0]; height = $size[1] } | Out-Null
        $deadline = [DateTime]::UtcNow.AddSeconds(20)
        do {
            Start-Sleep -Milliseconds 100
            $ui = Send-K4 $number 'ui.inspect'
            if ([Math]::Abs($ui.window.width - $size[0]) -le 2 -and [Math]::Abs($ui.window.height - $size[1]) -le 2) { break }
        } while ([DateTime]::UtcNow -lt $deadline)
        if ([Math]::Abs($ui.window.width - $size[0]) -gt 2 -or [Math]::Abs($ui.window.height - $size[1]) -gt 2) { throw "Native resize did not reach $($size[0])x$($size[1])" }
        Send-K4 $number 'ui.set' @{ fit = $true } | Out-Null
        Save-K4Image $number "$name-$($size[0])"
        $script:k4Layouts.Add(@{ client = $number; image = "$name-$($size[0]).png"; requested = $size; actual = $ui.window })
    }
}
function Ready-K4([int]$number) {
    if ($script:k4ReadyRevision) {
        Wait-K4 $number { param($s) $s.status.room.revision -ge $script:k4ReadyRevision } 'preceding lobby change observed' | Out-Null
    }
    Click-K4 $number 'komaphone.room.ready'
    $ready = Wait-K4 $number { param($s) $s.status.pending -eq 0 -and ($s.status.room.members | Where-Object { $_.participant.id -eq $s.status.profile.id }).ready } 'ready accepted'
    $script:k4ReadyRevision = $ready.status.room.revision
}

function Click-K4([int]$number, [string]$id) {
    if ($id -eq 'komaphone.room.openDraft') {
        # The same command also appears in the assigned-drawing banner. Close the
        # overlapping room window so the recorded banner is actually clickable.
        Send-K4 $number 'ui.set' @{ roomsOpen = $false } | Out-Null
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    $lastRect = ''
    $lastFrame = -1
    $stable = 0
    do {
        $ui = Send-K4 $number 'ui.inspect'
        $rect = $ui.roomControls.$id
        if ($rect -and $rect.Count -eq 4 -and $rect[0] -ge 0 -and $rect[1] -ge 0 -and $rect[2] -le $ui.window.width -and $rect[3] -le $ui.window.height) {
            $rectKey = $rect -join ','
            if ($rectKey -eq $lastRect -and $ui.frame -ne $lastFrame) { $stable++ } else { $stable = 1 }
            $lastRect = $rectKey
            $lastFrame = $ui.frame
            if ($stable -ge 3) {
                $enabled = $null
                if (($id.StartsWith('komaphone.room.') -and $id -ne 'komaphone.room.note') -or $id -eq 'komaphone.animation.copyPrevious' -or $id.StartsWith('komaphone.gallery.')) {
                    $command = @(Send-K4 $number 'engine.commands' | Where-Object id -eq $id)
                    if ($command.Count -ne 1) { throw "Missing engine command metadata for $id" }
                    $enabled = [bool]$command[0].enabled
                }
                if ($id.StartsWith('gallery.') -or $id.StartsWith('komaphone.gallery.')) {
                    $galleryState = Invoke-K4 $number 'komaphone.gallery.status'
                    if ($galleryState.busy) { $enabled = $false }
                }
                $confirmed = Send-K4 $number 'ui.inspect'
                if ($enabled -ne $false -and ($confirmed.roomControls.$id -join ',') -eq $rectKey) {
                    if ($id.StartsWith('gallery.export.') -or $id -in @('komaphone.gallery.import','komaphone.gallery.backup','komaphone.gallery.restore')) {
                        if ($ScopedFilesystem) {
                            $fileCommand = $id
                            $fileParams = @{path=$confirmed.gallery.path}
                            if ($id.StartsWith('gallery.export.')) {
                                $fileCommand = 'komaphone.gallery.export'
                                $fileParams.format = $id.Substring('gallery.export.'.Length)
                                $fileParams.chain = [int]$confirmed.gallery.chain
                                $fileParams.frame = $(if ($galleryState.active.content.room.mode.kind -eq 'exquisite_corpse') { $null } else { [int]$confirmed.gallery.frame })
                                $fileParams.fps = [int]$confirmed.gallery.fps
                                $fileParams.looping = [bool]$confirmed.gallery.looping
                            } elseif ($id -eq 'komaphone.gallery.restore') {
                                $fileParams.destination = $confirmed.gallery.restore_destination
                            }
                            $pending = Send-K4 $number 'app.galleryFile' @{command=$fileCommand;params=$fileParams}
                            if (-not $pending.pending -or $pending.saved) { throw 'Scoped file request did not return an honest pending acknowledgement' }
                        } else {
                        if (-not $NativeFilesystemClicks) { throw 'Filesystem acceptance requires -ScopedFilesystem or -NativeFilesystemClicks; synthetic ambient file input remains denied' }
                        $requestId=[guid]::NewGuid().ToString('N')
                        $ack=Join-Path $k4Root "$requestId.done"
                        @{requestId=$requestId;root=$k4Root;client=$number;process=$script:k4Apps[$number].Process.Id;control=$id;rect=$rect;ack=$ack;path=$confirmed.gallery.path;destination=$confirmed.gallery.restore_destination} | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $k4Repo 'log/k4/native-action.json')
                        Write-Host "Native UI action required: client $number, $id (request $requestId)"
                        $nativeDeadline=[DateTime]::UtcNow.AddMinutes(10)
                        while (-not (Test-Path -LiteralPath $ack) -and [DateTime]::UtcNow -lt $nativeDeadline) {Start-Sleep -Milliseconds 100}
                        if (-not (Test-Path -LiteralPath $ack)) {throw "Native UI action timed out: $id"}
                        }
                    } else {
                        Send-K4 $number 'ui.click' @{ x = ($rect[0] + $rect[2]) / 2; y = ($rect[1] + $rect[3]) / 2 } | Out-Null
                    }
                    $script:k4Clicks.Add(@{ client = $number; control = $id; rect = $rect; frame = $confirmed.frame; enabled = $enabled; stableFrames = $stable; at = [DateTime]::UtcNow.ToString('o') })
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
function Assert-K4Refusal([int]$number, [string]$command, [hashtable]$params = @{}) {
    $rejected = $false
    try { Invoke-K4 $number $command $params | Out-Null } catch {
        # A closed socket or crashed app must never count as an authorization refusal.
        if (-not $_.Exception.Message.StartsWith("Client $number engine.execute failed:")) { throw }
        if ($_.Exception.Message -notmatch '(?i)assign|frozen') { throw "Expected assignment authorization refusal for ${command}, got: $($_.Exception.Message)" }
        $rejected = $true
        $script:k4Refusals.Add(@{ client = $number; command = $command; error = $_.Exception.Message })
    }
    if (-not $rejected) { throw "Client $number forbidden command $command succeeded" }
}
function Wait-K4Media([int]$number, [object]$room) {
    $expected = @()
    if ($room.phase -eq 'drawing' -or $room.phase -eq 'transfer') {
        if ($room.assignment.reference.image) { $expected = @($room.assignment.reference.image.hash) }
    } else {
        $expected = @($room.reveal | Where-Object kind -eq 'artwork' | ForEach-Object { $_.image.hash } | Sort-Object -Unique)
    }
    $deadline = [DateTime]::UtcNow.AddSeconds(90)
    do {
        $media = Invoke-K4 $number 'komaphone.room.mediaStatus'
        if ($media.error) { throw "Authorized media failed: $($media.error)" }
        $allowed = @($media.allowed | ForEach-Object { $_.hash } | Sort-Object -Unique)
        if (($allowed -join ',') -ne (($expected | Sort-Object -Unique) -join ',')) {
            @{ client = $number; expectedRoom = $room; currentRoom = (Invoke-K4 $number 'komaphone.room.status'); media = $media } | ConvertTo-Json -Depth 40 | Set-Content (Join-Path $k4Root 'media-manifest-mismatch.json')
            throw "Client $number media allowed manifest differs from observed phase $($room.phase) revision $($room.revision)"
        }
        if (@($media.cached | Where-Object { $_ -notin $expected }).Count) { throw 'Native cache retained unauthorized artwork' }
        if (@($media.cached).Count -eq $expected.Count) {
            $ui = Send-K4 $number 'ui.inspect'
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
function Note-K4([int]$number, [string]$note) {
    Click-K4 $number 'komaphone.room.note'
    Send-K4 $number 'ui.key' @{ key = 'A'; ctrl = $true } | Out-Null
    Send-K4 $number 'ui.type' @{ text = $note } | Out-Null
    Wait-K4 $number { param($s) $s.status.note_draft.note -eq $note -and -not $s.busy } 'mouse-typed note durably saved' | Out-Null
}
function Slider-K4([int]$number, [string]$id) {
    $ui = Send-K4 $number 'ui.inspect'
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
    Send-K4 $number 'ui.click' @{ x = $rect[0] + 172; y = ($rect[1] + $rect[3]) / 2; count = 2 } | Out-Null
    Start-Sleep -Milliseconds 100
    Send-K4 $number 'ui.key' @{ key = 'A'; ctrl = $true } | Out-Null
    Send-K4 $number 'ui.type' @{ text = $text } | Out-Null
    Send-K4 $number 'ui.key' @{ key = 'Enter' } | Out-Null
    $deadline = [DateTime]::UtcNow.AddSeconds(10)
    do {
        Start-Sleep -Milliseconds 100
        $changed = Send-K4 $number 'ui.inspect'
        $after = $changed.rooms.animation.$property
        if ($after -ne $before) {
            $script:k4Clicks.Add(@{ client = $number; control = $id; rect = $rect; valueBoxOffset = 172; interaction = 'double-click value; Ctrl+A; type; Enter'; typed = $text; before = $before; after = $after; frame = $changed.frame; at = [DateTime]::UtcNow.ToString('o') })
            return
        }
    } while ([DateTime]::UtcNow -lt $deadline)
    throw "Native slider $id did not change from $before"
}
function Draw-K4([int]$number, [int]$round, [bool]$corpse) {
    Send-K4 $number 'ui.set' @{ roomsOpen = $false; tool = 'brush'; fit = $true } | Out-Null
    Invoke-K4 $number 'tools.setBrush' @{ reset = $true; size = 16; hardness = 1; opacity = 1 } | Out-Null
    $color = @('#204080', '#208040', '#804020')[$number - 1]
    Invoke-K4 $number 'tools.setColors' @{ foreground = $color } | Out-Null
    $before = Invoke-K4 $number 'komaphone.project.status'
    $y = 100 + 40 * $round
    $beforePixel = Invoke-K4 $number 'document.pixel' @{ x = 300; y = $y + 30 }
    Send-K4 $number 'ui.pointer' @{ tool = 'brush'; events = @(
        @{ kind = 'down'; x = 100; y = $y },
        @{ kind = 'move'; x = 300; y = $y + 30 },
        @{ kind = 'up'; x = 500; y = $y }
    ) } | Out-Null
    $saved = Wait-K4Local $number ($before.active.local_revision + 1) $before.active.project
    $drawnPixel = Invoke-K4 $number 'document.pixel' @{ x = 300; y = $y + 30 }
    if (($beforePixel | ConvertTo-Json -Compress) -eq ($drawnPixel | ConvertTo-Json -Compress)) { throw 'Actual pointer gesture did not paint its sampled drawing pixel' }
    if ($corpse) {
        # Actual canvas gesture reaches the bottom edge to make the joining strip nonblank.
        $beforeEdge = Invoke-K4 $number 'document.pixel' @{ x = 400; y = 511 }
        Send-K4 $number 'ui.pointer' @{ tool = 'brush'; events = @(
            @{ kind = 'down'; x = 200; y = 511 },
            @{ kind = 'move'; x = 400; y = 511 },
            @{ kind = 'up'; x = 600; y = 511 }
        ) } | Out-Null
        $saved = Wait-K4Local $number ($saved.active.local_revision + 1) $before.active.project
        $drawnEdge = Invoke-K4 $number 'document.pixel' @{ x = 400; y = 511 }
        if (($beforeEdge | ConvertTo-Json -Compress) -eq ($drawnEdge | ConvertTo-Json -Compress)) { throw 'Actual pointer gesture did not paint the bottom seam boundary row' }
    }
    return $saved
}
function Resume-K4([int]$players) {
    Wait-K4 1 { param($s) @($s.status.room.members | Where-Object { -not $_.connected }).Count -eq 0 } 'all participant leases restored' | Out-Null
    $state = Invoke-K4 1 'komaphone.room.status'
    if ($state.status.room.paused) {
        Send-K4 1 'ui.set' @{ roomsOpen = $true } | Out-Null
        if ($Diagnostic) {
            # Provisional build's moderator row is below its scroll clip. This
            # observed native scrollbar-track point moves the row into view.
            Send-K4 1 'ui.click' @{ x = 647; y = 405 } | Out-Null
            Start-Sleep -Milliseconds 200
        }
        Click-K4 1 'komaphone.room.resume'
        Wait-K4 1 { param($s) -not $s.status.room.paused -and -not $s.busy } 'resumed after reconnect' | Out-Null
        Send-K4 1 'ui.set' @{ roomsOpen = $false } | Out-Null
    }
}

function Wait-K4Gallery([int]$number, [scriptblock]$condition, [string]$description, [int]$seconds = 90) {
    $deadline = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $state = Invoke-K4 $number 'komaphone.gallery.status'
        if ($state.error) { throw "Client $number gallery failed during ${description}: $($state.error)" }
        if (& $condition $state) { return $state }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    $state | ConvertTo-Json -Depth 40 | Set-Content -LiteralPath (Join-Path $k4Root "timeout-gallery-$number.json") -Encoding utf8
    throw "Client $number gallery timed out: $description"
}
function Open-K4Gallery([int]$number) {
    Send-K4 $number 'ui.set' @{ roomsOpen = $false } | Out-Null
    $ui = Send-K4 $number 'ui.inspect'
    if (-not $ui.gallery.open) {
        $control = if ($ui.roomControls.'workspace.gallery') { 'workspace.gallery' } else { 'home.gallery' }
        Click-K4 $number $control
    }
    Wait-K4Gallery $number { param($s) -not $s.busy -and $s.available } 'gallery ready' | Out-Null
}
function Text-K4([int]$number, [string]$id, [string]$value) {
    Click-K4 $number $id
    Send-K4 $number 'ui.key' @{ key = 'A'; ctrl = $true } | Out-Null
    Send-K4 $number 'ui.key' @{ key = 'Backspace' } | Out-Null
    if ($value.Length -gt 0) { Send-K4 $number 'ui.type' @{ text = $value } | Out-Null }
    Send-K4 $number 'ui.key' @{ key = 'Enter' } | Out-Null
}
function Number-K4([int]$number, [string]$id, [string]$value) {
    $ui = Send-K4 $number 'ui.inspect'
    $rect = $ui.roomControls.$id
    if (-not $rect -or $rect.Count -ne 4) { throw "Missing numeric control $id" }
    Send-K4 $number 'ui.click' @{ x = ($rect[0]+$rect[2])/2; y = ($rect[1]+$rect[3])/2; count = 2 } | Out-Null
    Start-Sleep -Milliseconds 100
    Send-K4 $number 'ui.key' @{ key = 'A'; ctrl = $true } | Out-Null
    Send-K4 $number 'ui.type' @{ text = $value } | Out-Null
    Send-K4 $number 'ui.key' @{ key = 'Enter' } | Out-Null
    $script:k4Clicks.Add(@{ client=$number; control=$id; rect=$rect; interaction='double-click numeric field; Ctrl+A; type; Enter'; typed=$value; at=[DateTime]::UtcNow.ToString('o') })
}
function Export-K4([int]$number, [string]$format, [string]$name) {
    $path = Join-Path $script:k4Apps[$number].Root $name
    if (Test-Path -LiteralPath $path) { throw 'Acceptance export destination already exists' }
    Wait-K4Gallery $number { param($s) -not $s.busy } 'export ready' | Out-Null
    Text-K4 $number 'gallery.path' $path
    Click-K4 $number "gallery.export.$format"
    $export = Wait-K4Gallery $number { param($s) -not $s.busy -and $s.notice -eq "Export saved: $path" } "$format export published" 180
    if (-not (Test-Path -LiteralPath $path)) { throw "Export acknowledged without output: $name" }
    $files = if ((Get-Item -LiteralPath $path).PSIsContainer) { @(Get-ChildItem -LiteralPath $path -File -Recurse) } else { @(Get-Item -LiteralPath $path) }
    if (-not $files.Count -or @($files | Where-Object Length -eq 0).Count) { throw "Export is empty: $name" }
    $script:k4Exports.Add(@{ client=$number; format=$format; path=$path; notice=$export.notice; files=@($files | ForEach-Object { @{path=$_.FullName;length=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash} }) })
    return $path
}
function Check-K4Image([string]$path, [int]$width, [int]$height, [int]$frames=1) {
    Add-Type -AssemblyName System.Drawing
    $image = [System.Drawing.Image]::FromFile($path)
    try {
        if ($image.Width -ne $width -or $image.Height -ne $height) { throw "Image dimensions disagree: $path" }
        $count = $image.GetFrameCount([System.Drawing.Imaging.FrameDimension]::new($image.FrameDimensionsList[0]))
        if ($count -ne $frames) { throw "Image frame count $count differs from $frames : $path" }
        return @{path=$path;width=$image.Width;height=$image.Height;frames=$count}
    } finally { $image.Dispose() }
}
function Select-K4Item([int]$number, [string]$entry) {
    Open-K4Gallery $number
    $listed = Wait-K4Gallery $number { param($s) -not $s.busy -and @($s.items | Where-Object id -eq $entry).Count -gt 0 } 'saved item listed'
    $item = @($listed.items | Where-Object id -eq $entry)[0]
    # Search is a visible UI control; it keeps the target row within the list viewport.
    Text-K4 $number 'gallery.filter' $item.title
    Wait-K4Gallery $number { param($s) -not $s.busy } 'thumbnail worker ready' | Out-Null
    Click-K4 $number "gallery.item.$entry"
    Wait-K4Gallery $number { param($s) -not $s.busy -and $s.active.id -eq $entry -and $s.image } 'saved item opened'
}
function Review-K4Game([bool]$corpse, [string]$game) {
    $label = if ($corpse) { 'creature' } else { 'animation' }
    Send-K4 1 'ui.set' @{ roomsOpen=$false } | Out-Null
    Click-K4 1 'komaphone.gallery.download'
    $saved = Wait-K4Gallery 1 { param($s) -not $s.busy -and -not $s.downloading -and @($s.items | Where-Object { $_.id -eq $game -and -not $_.draft }).Count -eq 1 } 'full original durably retained' 180
    $selected = Select-K4Item 1 $game
    if ($selected.active.authors.Count -ne 2) { throw 'Gallery lost frozen author attribution' }
    if ($selected.active.content.room.reveal.Count -ne $(if ($corpse) {6} else {8})) { throw 'Gallery lost accepted submissions' }
    Save-K4Layouts 1 "$label-gallery"
    Text-K4 1 'gallery.name' "K4 $label keepsake"
    Click-K4 1 'gallery.rename'
    Wait-K4Gallery 1 { param($s) -not $s.busy -and @($s.items | Where-Object { $_.id -eq $game -and $_.title -eq "K4 $label keepsake" }).Count -eq 1 } 'renamed original' | Out-Null
    Text-K4 1 'gallery.filter' ''
    Click-K4 1 'komaphone.gallery.update'
    Wait-K4Gallery 1 { param($s) -not $s.busy -and @($s.items | Where-Object { $_.id -eq $game -and $_.favorite }).Count -eq 1 } 'favorite persisted' | Out-Null
    Click-K4 1 'gallery.replayMode'
    $replay = Wait-K4Gallery 1 { param($s) -not $s.busy -and $s.replay.moments.Count -ge 2 -and -not $s.replay.baseline_only } 'retained drawing replay ready'
    $before = Send-K4 1 'ui.inspect'
    Click-K4 1 'gallery.replayPlay'
    $deadline = [DateTime]::UtcNow.AddSeconds(25)
    do {
        $playing = Send-K4 1 'ui.inspect'
        if ($playing.gallery.replay_step -gt $before.gallery.replay_step) { break }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    if ($playing.gallery.replay_step -le $before.gallery.replay_step) { throw 'Drawing replay clock did not advance a retained edit' }
    # Use the inspected slider's editable number box (after its 150-point
    # track), because the control transport cannot sustain a mouse drag.
    $rect = $playing.roomControls.'gallery.replayStep'
    if (-not $rect) { throw 'Replay edit slider missing' }
    Send-K4 1 'ui.click' @{x=$rect[0]+172;y=($rect[1]+$rect[3])/2;count=2} | Out-Null
    Start-Sleep -Milliseconds 100
    Send-K4 1 'ui.key' @{key='A';ctrl=$true} | Out-Null
    Send-K4 1 'ui.type' @{text='0'} | Out-Null
    Send-K4 1 'ui.key' @{key='Enter'} | Out-Null
    Start-Sleep -Milliseconds 250
    $scrubbed = Send-K4 1 'ui.inspect'
    if ($scrubbed.gallery.replay_step -ne 0) { throw 'Actual replay scrub did not return to baseline' }
    $script:k4Clicks.Add(@{client=1;control='gallery.replayStep';rect=$rect;interaction='double-click observed value box; Ctrl+A; type 0; Enter';before=$playing.gallery.replay_step;after=$scrubbed.gallery.replay_step})
    Save-K4Image 1 "$label-drawing-replay-baseline"
    Click-K4 1 'gallery.finishedMode'
    if (-not $corpse) {
        Number-K4 1 'gallery.fps' '12'
        Wait-K4Gallery 1 {param($s) -not $s.busy -and $s.active.fps -eq 12} 'animation rate saved' | Out-Null
        $beforeAnimation = Send-K4 1 'ui.inspect'
        Click-K4 1 'gallery.animationPlay'
        $deadline = [DateTime]::UtcNow.AddSeconds(10)
        do { $animation = Send-K4 1 'ui.inspect'; if ($animation.gallery.frame -ne $beforeAnimation.gallery.frame) {break}; Start-Sleep -Milliseconds 100 } while ([DateTime]::UtcNow -lt $deadline)
        if ($animation.gallery.frame -eq $beforeAnimation.gallery.frame) {throw 'Whole animation did not advance independently'}
        Click-K4 1 'gallery.animationPlay'
        Number-K4 1 'gallery.frame' '0'
        Start-Sleep -Milliseconds 200
        if ((Send-K4 1 'ui.inspect').gallery.frame -ne 0) {throw 'Native animation frame scrub failed'}
    }
    $png = Export-K4 1 'png' "$label.png"
    $dimensions = Check-K4Image $png $(if($corpse){1024}else{1536}) $(if($corpse){1536}else{1024})
    $sequence = Export-K4 1 'sequence' "$label-frames"
    $pngs = @(Get-ChildItem -LiteralPath $sequence -File -Filter '*.png')
    if ($pngs.Count -ne $(if($corpse){1}else{4})) { throw 'PNG sequence count differs from finished output' }
    $gif = $null
    if (-not $corpse) {$gif=Export-K4 1 'gif' "$label.gif"; Check-K4Image $gif 1536 1024 4 | Out-Null}
    $archive = Export-K4 1 'archive' "$label.komarchive"
    $archiveHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash
    $source = Invoke-K4 1 'komaphone.gallery.status'
    Click-K4 1 'komaphone.gallery.remix'
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {$draft=Invoke-K4 1 'komaphone.project.status';$ui=Send-K4 1 'ui.inspect';if($draft.active.project -and $draft.active.project -ne $game -and -not $ui.gallery.open){break};Start-Sleep -Milliseconds 100} while([DateTime]::UtcNow -lt $deadline)
    if ($ui.gallery.open -or -not $draft.active.project) {throw 'Remix did not open its new editable draft'}
    $remix = Wait-K4Local 1 1 $draft.active.project
    $remixDocument = Invoke-K4 1 'document.inspect'
    $edited = Draw-K4 1 7 $false
    if ($edited.active.project -ne $remix.active.project) {throw 'Remix edit replaced the project identity'}
    Save-K4Image 1 "$label-remix-edited"
    Select-K4Item 1 $game | Out-Null
    $afterArchive = Export-K4 1 'archive' "$label-after-remix.komarchive"
    if ((Get-FileHash -LiteralPath $afterArchive -Algorithm SHA256).Hash -ne $archiveHash) {throw 'Editing a remix changed the original archive or history'}
    $afterPng=Export-K4 1 'png' "$label-after-remix.png"
    if ((Get-FileHash -LiteralPath $afterPng -Algorithm SHA256).Hash -ne (Get-FileHash -LiteralPath $png -Algorithm SHA256).Hash) {throw 'Editing a remix changed source presentation pixels'}
    $script:k4Games.Add(@{kind=$label;game=$game;gallery=$source;replay=$replay.replay;playbackAdvanced=$true;scrubbedToBaseline=$true;png=$dimensions;archive=$archive;archiveHash=$archiveHash;remixProject=$remix.active.project;remixRevision=$edited.active.local_revision;remixDocument=$remixDocument;originalUnchanged=$true})
}
function Play-K4([bool]$corpse) {
    $mode=if($corpse){@{kind='exquisite_corpse'}}else{@{kind='animation';rotations=2}}
    Write-Output "K4: completing actual two-player $($mode.kind) game"
    Invoke-K4 1 'komaphone.room.options' @{mode=$mode;timer_ms=$null}|Out-Null
    $configured=Wait-K4Idle 1;$script:k4ReadyRevision=$configured.status.room.revision
    foreach($number in 1..2){Send-K4 $number 'ui.set' @{roomsOpen=$true}|Out-Null;Ready-K4 $number}
    Wait-K4 1 {param($s) @($s.status.room.members|Where-Object {-not $_.ready}).Count -eq 0} 'all participants ready'|Out-Null
    Click-K4 1 'komaphone.room.start'
    $rounds=if($corpse){3}else{4}
    for($round=0;$round -lt $rounds;$round++) {
        foreach($number in 1..2) {
            Send-K4 $number 'ui.set' @{roomsOpen=$true}|Out-Null
            $state=Wait-K4 $number {param($s) $s.status.room.phase -eq 'drawing' -and $s.status.room.round -eq $round -and -not $s.busy} 'assigned turn'
            $assignment=$state.status.room.assignment.assignment
            Click-K4 $number 'komaphone.room.openDraft'
            Wait-K4Local $number 1 $assignment.project|Out-Null
            if(-not $corpse){Note-K4 $number "K4 note $number/$round"}
            Draw-K4 $number $round $corpse|Out-Null
            Click-K4 $number 'komaphone.room.submit'
            Wait-K4 $number {param($s) -not $s.busy -and ($s.status.room.assignment.submitted -or $s.status.room.round -gt $round -or $s.status.room.phase -eq 'reveal')} 'submission accepted' 90|Out-Null
        }
    }
    $reveal=Wait-K4 1 {param($s) $s.status.room.phase -eq 'reveal' -and $s.status.room.reveal.Count -eq 2*$rounds} 'complete reveal' 90
    Review-K4Game $corpse $reveal.status.room.reveal[0].assignment.id.game
    Click-K4 1 'gallery.draw'
    Send-K4 1 'ui.set' @{roomsOpen=$true}|Out-Null
    Click-K4 1 'komaphone.room.next'
    Wait-K4 1 {param($s) $s.status.room.phase -eq 'lobby' -and -not $s.busy} 'new lobby retained old original'|Out-Null
}

$script:k4Clicks = [System.Collections.Generic.List[object]]::new()
$script:k4Games = [System.Collections.Generic.List[object]]::new()
$script:k4Layouts = [System.Collections.Generic.List[object]]::new()
$script:k4Refusals = [System.Collections.Generic.List[object]]::new()
$script:k4Exports = [System.Collections.Generic.List[object]]::new()
try {
    $endpoint = Start-K4Service
    foreach ($number in 1..2) {
        Start-K4App $number
        Send-K4 $number 'ui.resize' @{width=1920;height=1080}|Out-Null
        Invoke-K4 $number 'komaphone.room.connect' @{endpoint=$endpoint;display_name="K4 Player $number"}|Out-Null
        Wait-K4 $number {param($s) $s.status.paired -and $s.status.connected -and -not $s.busy} 'paired isolated profile'|Out-Null
    }
    Invoke-K4 1 'komaphone.room.create' @{mode=@{kind='animation';rotations=2};timer_ms=$null}|Out-Null
    $created=Wait-K4 1 {param($s) $s.status.room -and -not $s.busy} 'room created'
    Invoke-K4 1 'komaphone.room.invite' @{expires_in_ms=3600000;uses=1}|Out-Null
    Wait-K4 1 {param($s) $s.status.invitation_id -and -not $s.busy} 'invitation committed'|Out-Null
    $invitation=Invoke-K4 1 'komaphone.room.invitation'
    Invoke-K4 2 'komaphone.room.join' @{room=$created.status.room.id;invitation=$invitation.invitation}|Out-Null
    $invitation=$null
    Wait-K4 1 {param($s) $s.status.room.members.Count -eq 2} 'two-player lobby'|Out-Null
    Play-K4 $false
    Play-K4 $true

    # The peer obtains a prior game's complete original after both Next Game actions.
    Open-K4Gallery 2
    Click-K4 2 'komaphone.gallery.history'
    $history=Wait-K4Gallery 2 {param($s) -not $s.busy -and $s.remote.Count -eq 2} 'historical game listing'
    $oldGame=$script:k4Games[0].game
    Click-K4 2 "gallery.remote.$oldGame"
    $peer=Wait-K4Gallery 2 {param($s) -not $s.busy -and -not $s.downloading -and @($s.items|Where-Object { $_.id -eq $oldGame -and -not $_.draft }).Count -eq 1} 'past-game original saved' 180
    $script:k4Evidence.historicalPeer=@{history=$history.remote;original=$oldGame;retained=$true}

    Stop-K4App 1
    Start-K4App 1
    Send-K4 1 'ui.resize' @{width=1920;height=1080}|Out-Null
    Open-K4Gallery 1
    foreach($game in $script:k4Games) {
        $reopened=Select-K4Item 1 $game.game
        if ($reopened.active.authors.Count -ne 2) {throw 'Restart lost original authors'}
        $resaved=Export-K4 1 'archive' "$($game.kind)-after-restart.komarchive"
        if((Get-FileHash -LiteralPath $resaved -Algorithm SHA256).Hash -ne $game.archiveHash){throw 'Restart changed the saved original'}
        Invoke-K4 1 'komaphone.project.open' @{project=$game.remixProject}|Out-Null
        Wait-K4Local 1 $game.remixRevision $game.remixProject|Out-Null
        $game.restartRecovered=$true
    }
    $latest=Select-K4Item 1 $script:k4Games[1].game
    Save-K4Image 1 'gallery-restarted-original'

    # Import runs in an unpaired third app with an empty data root. It cannot
    # reach service content; its offline result must come from the archive.
    Start-K4App 3
    Send-K4 3 'ui.resize' @{width=1920;height=1080}|Out-Null
    Open-K4Gallery 3
    Click-K4 3 'gallery.storage'
    $freshImports=[System.Collections.Generic.List[object]]::new()
    foreach($game in $script:k4Games) {
        $copied=Join-Path $script:k4Apps[3].Root "$($game.kind).komarchive"
        Copy-Item -LiteralPath $game.archive -Destination $copied
        Text-K4 3 'gallery.storagePath' $copied
        Click-K4 3 'komaphone.gallery.import'
        $imported=Wait-K4Gallery 3 {param($s) -not $s.busy -and $s.active.id -eq $game.game} 'fresh-root archive imported' 180
        if($imported.active.authors.Count -ne 2 -or $imported.active.content.room.reveal.Count -ne $game.gallery.active.content.room.reveal.Count){throw 'Portable archive lost credits or original submissions'}
        if(@($imported.items | Where-Object { $_.id -eq $game.game -and -not $_.draft }).Count -ne 1 -or $imported.usageBytes -le 0){throw 'Fresh archive import did not publish its gallery list row and storage usage'}
        Click-K4 3 'gallery.storage'
        $reexport=Export-K4 3 'archive' "$($game.kind)-fresh-roundtrip.komarchive"
        if((Get-FileHash -LiteralPath $reexport -Algorithm SHA256).Hash -ne $game.archiveHash){throw 'Fresh-root archive roundtrip changed original/history'}
        $freshImports.Add(@{game=$game.game;archiveHash=$game.archiveHash;freshUnpairedRoot=$script:k4Apps[3].Root;identicalRoundtrip=$true})
        Save-K4Image 3 "$($game.kind)-fresh-import"
        Click-K4 3 'gallery.storage'
    }
    $script:k4Evidence.freshImports=$freshImports.ToArray()

    # Full backup includes local remix drafts as well as retained gallery games.
    Open-K4Gallery 1
    Click-K4 1 'gallery.storage'
    $backup=Join-Path $script:k4Apps[1].Root 'full-backup'
    Text-K4 1 'gallery.storagePath' $backup
    Click-K4 1 'komaphone.gallery.backup'
    Wait-K4Gallery 1 {param($s) -not $s.busy -and $s.notice -eq "Backup saved: $backup"} 'full backup completed' 180|Out-Null
    $restored=Join-Path $script:k4Apps[1].Root 'restored-data'
    Text-K4 1 'gallery.restoreDestination' $restored
    Click-K4 1 'komaphone.gallery.restore'
    Wait-K4Gallery 1 {param($s) -not $s.busy -and $s.notice.StartsWith('Restored a separate data directory:')} 'full backup restored into fresh root' 180|Out-Null
    if(-not(Test-Path -LiteralPath $restored)){throw 'Restore acknowledgement has no fresh data folder'}
    Stop-K4App 3
    Start-K4App 3 $restored
    Send-K4 3 'ui.resize' @{width=1920;height=1080}|Out-Null
    foreach($game in $script:k4Games) {
        Select-K4Item 3 $game.game|Out-Null
        Invoke-K4 3 'komaphone.project.open' @{project=$game.remixProject}|Out-Null
        Wait-K4Local 3 $game.remixRevision $game.remixProject|Out-Null
    }
    Save-K4Image 3 'fresh-backup-restoration'
    $script:k4Evidence.backup=@{backup=$backup;restored=$restored;freshDestination=$true;restoredGamesAndRemixDraftsOpened=$true;files=@(Get-ChildItem -LiteralPath $backup -File -Recurse|ForEach-Object{@{path=$_.FullName;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash;length=$_.Length}})}
    $script:k4Evidence.games=$script:k4Games.ToArray()
    $script:k4Evidence.nativeClicks=$script:k4Clicks.ToArray()
    $script:k4Evidence.layouts=$script:k4Layouts.ToArray()
    $script:k4Evidence.exports=$script:k4Exports.ToArray()
    $script:k4Evidence.passed=$true
    $script:k4Evidence | ConvertTo-Json -Depth 70 | Set-Content -LiteralPath (Join-Path $k4Root 'result.json') -Encoding utf8
    Write-Output "PASS: actual two-player Animation and Corpse, durable gallery, replay, exports, independent remix, restart, historical peer download, fresh-root archive import, and backup/restore. Artifacts: $k4Root"
} catch {
    $failure=$_
    $failure|Out-String|Set-Content -LiteralPath (Join-Path $k4Root 'failure.log') -Encoding utf8
    $script:k4Evidence.games=$script:k4Games.ToArray()
    $script:k4Evidence.nativeClicks=$script:k4Clicks.ToArray()
    $script:k4Evidence.layouts=$script:k4Layouts.ToArray()
    $script:k4Evidence.exports=$script:k4Exports.ToArray()
    $script:k4Evidence.passed=$false
    $script:k4Evidence|ConvertTo-Json -Depth 70|Set-Content -LiteralPath (Join-Path $k4Root 'result.json') -Encoding utf8
    foreach($number in @(1,2,3)) {
        try {
            Send-K4 $number 'ui.inspect'|ConvertTo-Json -Depth 40|Set-Content -LiteralPath (Join-Path $k4Root "failure-ui-$number.json") -Encoding utf8
            Invoke-K4 $number 'komaphone.gallery.status'|ConvertTo-Json -Depth 50|Set-Content -LiteralPath (Join-Path $k4Root "failure-gallery-$number.json") -Encoding utf8
            Invoke-K4 $number 'komaphone.project.status'|ConvertTo-Json -Depth 40|Set-Content -LiteralPath (Join-Path $k4Root "failure-project-$number.json") -Encoding utf8
            Save-K4Image $number 'failure'
        } catch {$_|Out-String|Set-Content -LiteralPath (Join-Path $k4Root "failure-capture-$number.log") -Encoding utf8}
    }
    throw $failure
} finally {
    foreach($number in @(1,2,3)){Stop-K4App $number}
    Stop-K4Service
    $cleanupFailures=[System.Collections.Generic.List[int]]::new()
    $cleanupResults=[System.Collections.Generic.List[object]]::new()
    foreach($number in @(1,2,3)) {
        $dataRoot=if($script:k4Apps[$number]){$script:k4Apps[$number].DataRoot}else{Join-Path $k4Root "client-$number/data"}
        $database=Join-Path $dataRoot 'rooms.sqlite3'
        if(Test-Path -LiteralPath $database) {
            try {
                & $CredentialCleanupExe --discard-isolated-test-credentials $database *> (Join-Path $k4Root "client-$number/credentials-cleanup.log")
                $cleanupResults.Add(@{client=$number;exitCode=$LASTEXITCODE;log="client-$number/credentials-cleanup.log"})
                if($LASTEXITCODE -ne 0){$cleanupFailures.Add($number)}
            } catch {$cleanupFailures.Add($number);$cleanupResults.Add(@{client=$number;error=$_.ToString()})}
        }
    }
    $env:KOMAPHONE_DATA_DIR=$k4OriginalDataRoot
    $env:PHOTOCRAFT_CONFIG_DIR=$k4OriginalConfigRoot
    $env:KOMAPHONE_TRUSTED_CERT=$k4OriginalTrust
    $env:KOMAPHONE_SERVICE_ENDPOINT=$k4OriginalEndpoint
    $cleanupResults.ToArray()|ConvertTo-Json -Depth 5|Set-Content -LiteralPath (Join-Path $k4Root 'cleanup.json') -Encoding utf8
    if($cleanupFailures.Count) {
        $script:k4Evidence.passed=$false;$script:k4Evidence.cleanupFailed=$cleanupFailures.ToArray()
        $script:k4Evidence|ConvertTo-Json -Depth 70|Set-Content -LiteralPath (Join-Path $k4Root 'result.json') -Encoding utf8
        throw "Isolated credential cleanup failed for client(s) $($cleanupFailures -join ', '); see cleanup.json"
    }
}

