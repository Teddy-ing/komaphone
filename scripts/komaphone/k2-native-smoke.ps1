# Isolated real-process K2 acceptance. Requires the native app, service, and the
# room-client credential cleanup example; all output stays under repo/log/k2.
# The multiplayer service and each app's automation channel bind only loopback.
param(
    [string]$AppExe = '',
    [string]$ServiceExe = '',
    [string]$CredentialCleanupExe = '',
    [switch]$SkipTimedGame
)
$ErrorActionPreference = 'Stop'
$k2Repo = (Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
if (-not $AppExe) {
    $AppExe = Join-Path $k2Repo 'log/k2/bin/photocraft.exe'
    if (-not (Test-Path -LiteralPath $AppExe)) { $AppExe = Join-Path $k2Repo 'target/agent-root/debug/photocraft.exe' }
}
if (-not $ServiceExe) { $ServiceExe = Join-Path $k2Repo 'log/k2/bin/photocraft-sync.exe' }
if (-not $CredentialCleanupExe) { $CredentialCleanupExe = Join-Path $k2Repo 'log/k2/bin/cleanup_credentials.exe' }
$AppExe = (Resolve-Path -LiteralPath $AppExe).Path
$ServiceExe = (Resolve-Path -LiteralPath $ServiceExe).Path
$CredentialCleanupExe = (Resolve-Path -LiteralPath $CredentialCleanupExe).Path
$k2OriginalDataRoot = $env:KOMAPHONE_DATA_DIR
$k2OriginalConfigRoot = $env:PHOTOCRAFT_CONFIG_DIR
$k2Root = Join-Path (Join-Path $k2Repo 'log/k2') ('native-rooms-' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $k2Root | Out-Null
$script:k2Apps = @{}
$script:k2Service = $null
$script:k2ServicePort = 0
$script:k2ServiceGeneration = 0
$script:k2Evidence = [ordered]@{
    appHash = (Get-FileHash -LiteralPath $AppExe -Algorithm SHA256).Hash
    serviceHash = (Get-FileHash -LiteralPath $ServiceExe -Algorithm SHA256).Hash
}

function New-K2Port {
    $listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $listener.Start()
    $port = $listener.LocalEndpoint.Port
    $listener.Stop()
    return $port
}
function Start-K2Service {
    $script:k2ServiceGeneration++
    $generation = $script:k2ServiceGeneration
    $root = Join-Path $k2Root 'authority'
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $stdout = Join-Path $k2Root "service-$generation.stdout.log"
    $stderr = Join-Path $k2Root "service-$generation.stderr.log"
    $arguments = @('--bind', "127.0.0.1:$script:k2ServicePort", '--data-dir', ('"' + $root + '"'))
    $script:k2Service = Start-Process -FilePath $ServiceExe -ArgumentList $arguments -WindowStyle Hidden -PassThru `
        -WorkingDirectory $k2Repo -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $deadline = [DateTime]::UtcNow.AddSeconds(45)
    do {
        if ($script:k2Service.HasExited) { throw "Service exited: $($script:k2Service.ExitCode); see $stderr" }
        $ready = Get-Content -LiteralPath $stdout -ErrorAction SilentlyContinue | Where-Object { $_ -match '^\{' } | Select-Object -First 1
        if ($ready) {
            $result = $ready | ConvertFrom-Json
            if ($result.ready) {
                $script:k2ServicePort = [int]($result.address -split ':')[-1]
                return "http://127.0.0.1:$script:k2ServicePort"
            }
        }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Service readiness timed out'
}
function Stop-K2Service {
    if ($script:k2Service -and -not $script:k2Service.HasExited) {
        Stop-Process -Id $script:k2Service.Id -Force
        $script:k2Service.WaitForExit()
    }
    $script:k2Service = $null
}
function Connect-K2Control([hashtable]$app) {
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
function Send-K2([int]$number, [string]$method, [hashtable]$params = @{}) {
    $app = $script:k2Apps[$number]
    if (-not $app -or $app.Process.HasExited) { throw "Client $number is not running" }
    if (-not $app.Client -or ([DateTime]::UtcNow - $app.LastUse).TotalSeconds -gt 20) { Connect-K2Control $app }
    $frame = @{ id = [guid]::NewGuid().ToString('N'); method = $method; params = $params } | ConvertTo-Json -Depth 35 -Compress
    $app.Writer.WriteLine($frame)
    $line = $app.Reader.ReadLine()
    if (-not $line) { throw "Client $number closed control during $method; request may have completed" }
    $reply = $line | ConvertFrom-Json
    $app.LastUse = [DateTime]::UtcNow
    if (-not $reply.ok) { throw "Client $number $method failed: $($reply.error)" }
    return $reply.result
}
function Invoke-K2([int]$number, [string]$command, [hashtable]$params = @{}) {
    Send-K2 $number 'engine.execute' @{ command = $command; params = $params }
}
function Start-K2App([int]$number) {
    $root = Join-Path $k2Root "client-$number"
    New-Item -ItemType Directory -Force -Path $root | Out-Null
    $previous = $script:k2Apps[$number]
    $generation = if ($previous) { $previous.Generation + 1 } else { 1 }
    $env:KOMAPHONE_DATA_DIR = Join-Path $root 'data'
    $env:PHOTOCRAFT_CONFIG_DIR = Join-Path $root 'config'
    $tokenPath = Join-Path $root 'control.token'
    $port = New-K2Port
    $arguments = @('--new-instance', '--control', "$port", '--control-token-file', ('"' + $tokenPath + '"'),
        '--automation-read-root', ('"' + $root + '"'), '--automation-write-root', ('"' + $root + '"'))
    $process = Start-Process -FilePath $AppExe -ArgumentList $arguments -WindowStyle Hidden -PassThru -WorkingDirectory $k2Repo `
        -RedirectStandardOutput (Join-Path $root "app-$generation.stdout.log") -RedirectStandardError (Join-Path $root "app-$generation.stderr.log")
    $app = @{ Process = $process; Root = $root; TokenPath = $tokenPath; Port = $port; Generation = $generation }
    $script:k2Apps[$number] = $app
    $deadline = [DateTime]::UtcNow.AddSeconds(60)
    do {
        if ($process.HasExited) { throw "Client $number exited before control startup: $($process.ExitCode)" }
        try { Connect-K2Control $app; break } catch { Start-Sleep -Milliseconds 100 }
    } while ([DateTime]::UtcNow -lt $deadline)
    if (-not $app.Client) { throw "Client $number control startup timed out" }
    Send-K2 $number 'ui.set' @{ roomsOpen = $true } | Out-Null
}
function Stop-K2App([int]$number) {
    $app = $script:k2Apps[$number]
    if ($app) {
        if ($app.Client) { $app.Client.Dispose(); $app.Client = $null }
        if ($app.Process -and -not $app.Process.HasExited) { Stop-Process -Id $app.Process.Id -Force; $app.Process.WaitForExit() }
    }
}
function Wait-K2([int]$number, [scriptblock]$condition, [string]$label, [int]$seconds = 40) {
    $deadline = [DateTime]::UtcNow.AddSeconds($seconds)
    do {
        $state = Invoke-K2 $number 'komaphone.room.status'
        if (& $condition $state) { return $state }
        Start-Sleep -Milliseconds 100
    } while ([DateTime]::UtcNow -lt $deadline)
    $state | ConvertTo-Json -Depth 25 | Set-Content -LiteralPath (Join-Path $k2Root "timeout-client-$number.json") -Encoding utf8
    throw "Client $number timed out: $label"
}
function Wait-K2Idle([int]$number) {
    Wait-K2 $number { param($s) -not $s.busy -and $s.status.pending -eq 0 -and -not $s.status.blocked -and $s.status.connected } 'confirmed room request'
}
function Wait-K2Local([int]$number, [long]$revision, [string]$project = '') {
    $deadline = [DateTime]::UtcNow.AddSeconds(30)
    do {
        $state = Invoke-K2 $number 'komaphone.project.status'
        if ($state.error -or $state.active.error) { throw 'Local draft save failed' }
        if ($state.active.local_revision -ge $revision -and $state.active.pending -eq 0 -and
            (-not $project -or $state.active.project -eq $project)) { return $state }
        Start-Sleep -Milliseconds 20
    } while ([DateTime]::UtcNow -lt $deadline)
    throw 'Local draft durability acknowledgment timed out'
}
function Save-K2Image([int]$number, [string]$name) {
    Send-K2 $number 'ui.screenshot' @{ path = "$name.png"; focus = $false } | Out-Null
}
function Ready-K2([int]$number) {
    if ($script:k2ReadyRevision) {
        Wait-K2 $number { param($s) $s.status.room.revision -ge $script:k2ReadyRevision } 'preceding lobby change observed' | Out-Null
    }
    Invoke-K2 $number 'komaphone.room.ready' @{ ready = $true } | Out-Null
    $ready = Wait-K2 $number { param($s) $s.status.pending -eq 0 -and ($s.status.room.members | Where-Object { $_.participant.id -eq $s.status.profile.id }).ready } 'ready accepted'
    $script:k2ReadyRevision = $ready.status.room.revision
}

try {
    $endpoint = Start-K2Service
    foreach ($number in 1..3) {
        Start-K2App $number
        Invoke-K2 $number 'komaphone.room.connect' @{ endpoint = $endpoint; display_name = "K2 Player $number" } | Out-Null
        Wait-K2 $number { param($s) $s.status.paired -and $s.status.connected -and $s.status.pending -eq 0 } 'paired' | Out-Null
    }
    Invoke-K2 1 'komaphone.room.create' @{ mode = @{ kind = 'animation'; rotations = 1 }; timer_ms = $null } | Out-Null
    $roomHost = Wait-K2 1 { param($s) $s.status.room -and $s.status.pending -eq 0 } 'room created'
    $room = $roomHost.status.room.id
    Invoke-K2 1 'komaphone.room.invite' @{ expires_in_ms = 3600000; uses = 2 } | Out-Null
    Wait-K2 1 { param($s) $s.status.invitation_id -and $s.status.pending -eq 0 } 'invitation committed' | Out-Null
    # The explicit authorized capability command is kept in memory only; never log this response.
    $invitation = Invoke-K2 1 'komaphone.room.invitation'
    Invoke-K2 2 'komaphone.room.join' @{ room = $room; invitation = $invitation.invitation } | Out-Null
    Wait-K2 2 { param($s) $s.status.room.members.Count -eq 2 -and $s.status.pending -eq 0 } 'second participant joined' | Out-Null
    Wait-K2 1 { param($s) $s.status.room.members.Count -eq 2 } 'host observed second participant' | Out-Null
    Invoke-K2 1 'komaphone.room.options' @{ mode = @{ kind = 'animation'; rotations = 1 }; timer_ms = $null } | Out-Null
    Wait-K2Idle 1 | Out-Null
    Save-K2Image 1 'lobby-two'
    # Click the actual native egui Ready button using its rendered control rectangle.
    $ui = Send-K2 1 'ui.inspect'
    $rect = $ui.roomControls.'komaphone.room.ready'
    if (-not $rect -or $rect.Count -ne 4) { throw 'Native Ready control rectangle is unavailable' }
    Send-K2 1 'ui.click' @{ x = ($rect[0] + $rect[2]) / 2; y = ($rect[1] + $rect[3]) / 2 } | Out-Null
    $mouseReady = Wait-K2 1 { param($s) ($s.status.room.members | Where-Object { $_.participant.id -eq $s.status.profile.id }).ready } 'mouse Ready accepted'
    $script:k2ReadyRevision = $mouseReady.status.room.revision
    Ready-K2 2
    Wait-K2 1 { param($s) -not ($s.status.room.members | Where-Object { -not $_.ready }) } 'all two-player ready states received' | Out-Null
    Invoke-K2 1 'komaphone.room.start' | Out-Null
    $two = Wait-K2 1 { param($s) $s.status.room.phase -eq 'drawing' -and $s.status.pending -eq 0 } 'two-player game start'
    $script:k2Evidence.twoPlayerStarted = $two.status
    Invoke-K2 1 'komaphone.room.openDraft' | Out-Null
    Wait-K2Local 1 1 $two.status.room.assignment.assignment.project | Out-Null
    Invoke-K2 1 'layer.new.layer' @{ name = 'Accepted animation frame' } | Out-Null
    Invoke-K2 1 'paint.stroke' @{ points = @(@(120,220),@(900,220)); size = 28; hardness = 1; color = '#204080' } | Out-Null
    Wait-K2Local 1 3 | Out-Null
    Invoke-K2 1 'komaphone.room.submit' @{ note = 'Synthetic K2 animation handoff' } | Out-Null
    $submitted = Wait-K2 1 { param($s) $s.status.room.assignment.submitted -and $s.status.pending -eq 0 } 'real locally durable animation frame accepted' 90
    $script:k2Evidence.nativeAnimationSubmission = $submitted.status
    Save-K2Image 1 'accepted-animation-frame'
    Invoke-K2 3 'komaphone.room.join' @{ room = $room; invitation = $invitation.invitation } | Out-Null
    $invitation = $null
    $late = Wait-K2 3 { param($s) $s.status.room.waiting.Count -eq 1 -and $s.status.pending -eq 0 } 'late arrival waiting'
    if ($late.status.room.assignment -or $late.status.room.reveal.Count) { throw 'Waiting participant received game content' }
    $script:k2Evidence.waitingParticipant = $late.status
    Stop-K2App 1
    Wait-K2 2 { param($s) $s.status.room.paused -and ($s.status.room.members | Where-Object { $_.participant.id -eq $s.status.room.moderator -and -not $_.connected }) } 'host lease expired' | Out-Null
    Invoke-K2 2 'komaphone.room.claim' | Out-Null
    $claimed = Wait-K2 2 { param($s) $s.status.room.moderator -eq $s.status.profile.id -and $s.status.pending -eq 0 } 'moderation claimed'
    $script:k2Evidence.hostLossClaim = $claimed.status
    Save-K2Image 2 'host-loss-paused'
    Start-K2App 1
    $rejoined = Wait-K2 1 { param($s) $s.status.connected -and $s.status.room.assignment } 'original host reconnected'
    if ($rejoined.status.profile.id -ne $two.status.profile.id -or
        ($rejoined.status.room.assignment.assignment | ConvertTo-Json -Compress) -ne ($two.status.room.assignment.assignment | ConvertTo-Json -Compress)) { throw 'Client restart changed identity or assignment' }
    Wait-K2 2 { param($s) -not ($s.status.room.members | Where-Object { -not $_.connected }) } 'original host lease restored' | Out-Null
    Invoke-K2 2 'komaphone.room.resume' | Out-Null
    Wait-K2 2 { param($s) -not $s.status.room.paused -and $s.status.pending -eq 0 } 'resumed after host recovery' | Out-Null
    $beforeOutage = Invoke-K2 2 'komaphone.room.status'
    $offlineAssignedProject = $beforeOutage.status.room.assignment.assignment.project
    Invoke-K2 2 'komaphone.room.openDraft' | Out-Null
    Wait-K2Local 2 1 $offlineAssignedProject | Out-Null
    Stop-K2Service
    Wait-K2 2 { param($s) -not $s.status.connected } 'service unavailable' | Out-Null
    Invoke-K2 2 'komaphone.room.pause' | Out-Null
    $pending = Wait-K2 2 { param($s) $s.status.pending -gt 0 } 'offline room outbox persisted'
    # A queued metadata request must not freeze this actual assigned drawing.
    Invoke-K2 2 'layer.new.layer' @{ name = 'Offline preserved assigned layer' } | Out-Null
    Invoke-K2 2 'paint.stroke' @{ points = @(@(8,20),@(56,20)); size = 8; hardness = 1; color = '#204080' } | Out-Null
    $draft = Wait-K2Local 2 3 $offlineAssignedProject
    if ($draft.active.project -ne $offlineAssignedProject) { throw 'Offline edits did not belong to the active assigned project' }
    $draftPixel = Invoke-K2 2 'document.pixel' @{ x = 32; y = 20 }
    $script:k2Evidence.offlinePending = $pending.status
    Save-K2Image 2 'service-unavailable'
    Stop-K2App 2
    Start-K2App 2
    $pendingRestart = Wait-K2 2 { param($s) $s.status.pending -gt 0 -and -not $s.status.connected } 'offline outbox survived app kill'
    Invoke-K2 2 'komaphone.project.open' @{ project = $draft.active.project } | Out-Null
    Wait-K2Local 2 3 $draft.active.project | Out-Null
    $recoveredPixel = Invoke-K2 2 'document.pixel' @{ x = 32; y = 20 }
    $recoveredDocument = Invoke-K2 2 'document.inspect'
    if (($draftPixel | ConvertTo-Json -Compress) -ne ($recoveredPixel | ConvertTo-Json -Compress) -or $recoveredDocument.layers.Count -ne 2) { throw 'Offline drawing did not survive app restart' }
    $script:k2Evidence.offlineRestartPending = $pendingRestart.status
    $script:k2Evidence.offlineDraft = @{ project = $draft.active.project; assignedProject = $offlineAssignedProject; matchesAssignedProject = ($draft.active.project -eq $offlineAssignedProject); pixel = $recoveredPixel; layers = $recoveredDocument.layers.Count }
    $restartedEndpoint = Start-K2Service
    if ($restartedEndpoint -ne $endpoint) { throw 'Service restarted on different endpoint' }
    $after = Wait-K2 2 { param($s) $s.status.blocked -or ($s.status.connected -and $s.status.pending -eq 0) } 'outbox replay after service restart'
    if ($after.status.blocked) {
        $script:k2Evidence.explicitRejectedVariant = $after.status
        Invoke-K2 2 'komaphone.room.retainRejected' | Out-Null
    }
    foreach ($number in 1..3) { Wait-K2 $number { param($s) $s.status.connected -and $s.status.room -and -not $s.status.blocked } 'reconnected after service restart' | Out-Null }
    $acceptedAfterRestart = Invoke-K2 1 'komaphone.room.status'
    if (-not $acceptedAfterRestart.status.room.assignment.submitted) { throw 'Service restart lost the acknowledged native animation submission' }
    $script:k2Evidence.acceptedAfterServiceRestart = $acceptedAfterRestart.status
    Wait-K2 2 { param($s) -not ($s.status.room.members | Where-Object { -not $_.connected }) -and -not ($s.status.room.waiting | Where-Object { -not $_.connected }) } 'all frozen and waiting participants connected' | Out-Null
    Invoke-K2 2 'komaphone.room.resume' | Out-Null
    Wait-K2 2 { param($s) -not $s.status.room.paused -and $s.status.pending -eq 0 } 'explicit resume' | Out-Null
    $skipReason = 'K2 explicit moderator skip preserves the offline assigned draft'
    Invoke-K2 2 'komaphone.room.skip' @{ participant = $beforeOutage.status.profile.id; reason = $skipReason } | Out-Null
    $skipped = Wait-K2 2 { param($s) $s.status.room.round -eq 1 -and $s.status.pending -eq 0 } 'explicit skip completed first round exactly once'
    $script:k2Evidence.explicitSkip = $skipped.status
    Invoke-K2 2 'komaphone.room.end' @{ reason = 'K2 smoke kept an explicitly incomplete game' } | Out-Null
    $archived = Wait-K2 2 { param($s) $s.status.room.phase -eq 'archived' -and $s.status.pending -eq 0 } 'incomplete game archived'
    $missing = @($archived.status.room.reveal | Where-Object { $_.kind -eq 'missing' -and $_.assignment.project -eq $offlineAssignedProject -and $_.reason -eq $skipReason })
    $retainedArtwork = @($archived.status.room.reveal | Where-Object { $_.kind -eq 'artwork' -and $_.assignment.project -eq $two.status.room.assignment.assignment.project -and $_.revision -eq 3 })
    if ($missing.Count -ne 1 -or $retainedArtwork.Count -ne 1 -or -not $retainedArtwork[0].replay.manifest) { throw 'Incomplete archive lost the exact skipped project/reason or accepted host artwork/replay' }
    $script:k2Evidence.incompleteArchive = $archived.status
    Save-K2Image 2 'incomplete-skip-archive'
    Invoke-K2 2 'komaphone.room.next' | Out-Null
    $threeLobby = Wait-K2 2 { param($s) $s.status.room.phase -eq 'lobby' -and $s.status.room.members.Count -eq 3 -and $s.status.pending -eq 0 } 'waiting participant promoted'
    Wait-K2 3 { param($s) $s.status.room.phase -eq 'lobby' -and $s.status.room.revision -ge $threeLobby.status.room.revision } 'third player observed promoted lobby' | Out-Null
    Invoke-K2 3 'komaphone.room.leave' | Out-Null
    $left = Wait-K2 3 { param($s) $s.status.pending -eq 0 -and -not $s.status.polling } 'native membership leave committed'
    Wait-K2 2 { param($s) $s.status.room.members.Count -eq 2 } 'host observed native leave' | Out-Null
    Invoke-K2 2 'komaphone.room.invite' @{ expires_in_ms = 3600000; uses = 1 } | Out-Null
    Wait-K2 2 { param($s) $s.status.invitation_id -and $s.status.pending -eq 0 } 'return invitation committed' | Out-Null
    $returnInvitation = Invoke-K2 2 'komaphone.room.invitation'
    Invoke-K2 3 'komaphone.room.join' @{ room = $room; invitation = $returnInvitation.invitation } | Out-Null
    $returnInvitation = $null
    $returned = Wait-K2 3 { param($s) $s.status.room.members.Count -eq 3 -and $s.status.pending -eq 0 -and $s.status.polling } 'native membership rejoin committed'
    if ($returned.status.profile.id -ne $late.status.profile.id) { throw 'Native leave and reinvitation changed stable identity' }
    $script:k2Evidence.nativeLeave = $left.status
    $script:k2Evidence.nativeRejoin = $returned.status
    Wait-K2 2 { param($s) $s.status.room.members.Count -eq 3 } 'host observed rejoined third player' | Out-Null
    Invoke-K2 2 'komaphone.room.options' @{ mode = @{ kind = 'exquisite_corpse' }; timer_ms = $null } | Out-Null
    $configured = Wait-K2Idle 2
    $script:k2ReadyRevision = $configured.status.room.revision
    foreach ($number in 1..3) { Ready-K2 $number }
    Wait-K2 2 { param($s) -not ($s.status.room.members | Where-Object { -not $_.ready }) } 'all three-player ready states received' | Out-Null
    Save-K2Image 2 'lobby-three'
    Invoke-K2 2 'komaphone.room.start' | Out-Null
    $three = Wait-K2 2 { param($s) $s.status.room.phase -eq 'drawing' -and $s.status.room.members.Count -eq 3 -and $s.status.pending -eq 0 } 'three-player game started'
    $script:k2Evidence.threePlayerStarted = $three.status
    Invoke-K2 2 'komaphone.room.openDraft' | Out-Null
    Wait-K2Local 2 1 $three.status.room.assignment.assignment.project | Out-Null
    Invoke-K2 2 'layer.new.layer' @{ name = 'Accepted corpse head' } | Out-Null
    Invoke-K2 2 'paint.stroke' @{ points = @(@(140,500),@(860,500)); size = 20; hardness = 1; color = '#208040' } | Out-Null
    Wait-K2Local 2 3 | Out-Null
    Invoke-K2 2 'komaphone.room.submit' @{ note = '' } | Out-Null
    $corpseAccepted = Wait-K2 2 { param($s) $s.status.room.assignment.submitted -and $s.status.pending -eq 0 } 'real locally durable corpse band and seam accepted' 90
    $script:k2Evidence.nativeCorpseSubmission = $corpseAccepted.status
    Save-K2Image 2 'three-player-active'
    if (-not $SkipTimedGame) {
        Invoke-K2 2 'komaphone.room.end' @{ reason = 'K2 timer acceptance follows this retained incomplete game' } | Out-Null
        Wait-K2 2 { param($s) $s.status.room.phase -eq 'archived' -and $s.status.pending -eq 0 } 'second game archived' | Out-Null
        Invoke-K2 2 'komaphone.room.next' | Out-Null
        Wait-K2 2 { param($s) $s.status.room.phase -eq 'lobby' -and $s.status.pending -eq 0 } 'timed lobby' | Out-Null
        Invoke-K2 2 'komaphone.room.options' @{ mode = @{ kind = 'animation'; rotations = 1 }; timer_ms = 6000 } | Out-Null
        $configured = Wait-K2Idle 2
        $script:k2ReadyRevision = $configured.status.room.revision
        foreach ($number in 1..3) { Ready-K2 $number }
        Wait-K2 2 { param($s) -not ($s.status.room.members | Where-Object { -not $_.ready }) } 'all timed-game ready states received' | Out-Null
        Invoke-K2 2 'komaphone.room.start' | Out-Null
        $timed = Wait-K2 2 { param($s) $s.status.room.phase -eq 'drawing' -and $s.status.pending -eq 0 } 'timed game started'
        $timedProject = $timed.status.room.assignment.assignment.project
        Invoke-K2 2 'komaphone.room.openDraft' | Out-Null
        Wait-K2Local 2 1 $timedProject | Out-Null
        Invoke-K2 2 'layer.new.layer' @{ name = 'Automatically submitted saved frame' } | Out-Null
        Invoke-K2 2 'paint.stroke' @{ points = @(@(120,220),@(900,220)); size = 28; hardness = 1; color = '#804020' } | Out-Null
        $timedDraft = Wait-K2Local 2 3 $timedProject
        Invoke-K2 2 'komaphone.project.new' @{ width = 32; height = 32; name = 'Another active tab at deadline' } | Out-Null
        $otherDraft = Wait-K2Local 2 1
        if ($otherDraft.active.project -eq $timedProject) { throw 'Timed test did not switch to an unrelated active draft' }
        # No Submit command: the service deadline must pin and submit the other saved tab.
        $automatic = Wait-K2 2 { param($s) $s.status.room.phase -eq 'transfer' -and $s.status.room.assignment.submitted -and $s.status.pending -eq 0 } 'automatic timed submission from inactive tab' 35
        if ($automatic.status.freeze_pin.project -ne $timedProject -or $automatic.status.freeze_pin.revision -ne $timedDraft.active.local_revision) { throw 'Timed submission did not retain the exact acknowledged freeze pin' }
        $stillOther = Invoke-K2 2 'komaphone.project.status'
        if ($stillOther.active.project -ne $otherDraft.active.project) { throw 'Timed submission changed the active private drawing tab' }
        Invoke-K2 2 'layer.new.layer' @{ name = 'Unrelated drawing stays editable' } | Out-Null
        Wait-K2Local 2 2 $otherDraft.active.project | Out-Null
        $script:k2Evidence.automaticTimedSubmission = $automatic.status
        $script:k2Evidence.timedPrivateDraft = $otherDraft.active.project
        Save-K2Image 2 'automatic-timed-submission'
    }
    $script:k2Evidence.timedGateExercised = -not $SkipTimedGame
    $script:k2Evidence.passed = $true
    $script:k2Evidence | ConvertTo-Json -Depth 35 | Set-Content -LiteralPath (Join-Path $k2Root 'result.json') -Encoding utf8
    Write-Output "PASS: native two/three clients, mouse Ready, accepted frame/band submissions, waiting privacy, host claim, service/client kill, durable offline draft and pending outbox; timed gate exercised: $(-not $SkipTimedGame). Artifacts: $k2Root"
} catch {
    $_ | Out-String | Set-Content -LiteralPath (Join-Path $k2Root 'failure.log') -Encoding utf8
    throw
} finally {
    foreach ($number in @(1,2,3)) { Stop-K2App $number }
    Stop-K2Service
    foreach ($number in @(1,2,3)) {
        $database = Join-Path $k2Root "client-$number/data/rooms.sqlite3"
        if ((Test-Path -LiteralPath $database) -and (Test-Path -LiteralPath $CredentialCleanupExe)) {
            try {
                & $CredentialCleanupExe --discard-isolated-test-credentials $database *> (Join-Path $k2Root "client-$number/credentials-cleanup.log")
                if ($LASTEXITCODE -ne 0) { Write-Warning "Client $number isolated credential cleanup needs retry; see credentials-cleanup.log" }
            } catch { Write-Warning "Client $number isolated credential cleanup needs retry" }
        }
    }
    $env:KOMAPHONE_DATA_DIR = $k2OriginalDataRoot
    $env:PHOTOCRAFT_CONFIG_DIR = $k2OriginalConfigRoot
}
