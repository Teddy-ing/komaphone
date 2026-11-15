#Requires -Version 7.0
# Actual native import into an empty isolated service, using accepted synthetic art.
param(
    [Parameter(Mandatory=$true)][string]$AppExe,
    [Parameter(Mandatory=$true)][string]$ServiceExe,
    [Parameter(Mandatory=$true)][string]$CredentialCleanupExe,
    [Parameter(Mandatory=$true)][string]$PythonExe,
    [Parameter(Mandatory=$true)][string]$Archive,
    [string]$BuildEvidence='',
    [string]$SourcePins='',
    [string]$BuildScript=''
)
$ErrorActionPreference='Stop'
$archiveSource=(Resolve-Path -LiteralPath $Archive).Path
$importHarness=$PSCommandPath
. (Join-Path $PSScriptRoot 'k6-native-smoke.ps1') -AppExe $AppExe -ServiceExe $ServiceExe -CredentialCleanupExe $CredentialCleanupExe -BuildEvidence $BuildEvidence -SourcePins $SourcePins -BuildScript $BuildScript -WorldProtocolVersion 2 -FunctionsOnly
$script:k6Evidence.scope='Actual Windows app imports accepted synthetic archive into an empty loopback service with no source canvas; private membership, retained art, later edit, re-import and offline reopen. No hosted or Google acceptance.'
$script:k6Evidence.scenario='K7 portable archive import'
Copy-Item -LiteralPath $importHarness -Destination (Join-Path $k6Root 'k7-import-harness.ps1')
$importVerifier=Join-Path $PSScriptRoot 'verify-k7-import.py'
Copy-Item -LiteralPath $importVerifier -Destination (Join-Path $k6Root 'verify-k7-import.py')
$script:k6Evidence.importHarnessSha256=(Get-FileHash -LiteralPath $importHarness -Algorithm SHA256).Hash
$script:k6Evidence.importVerifierSha256=(Get-FileHash -LiteralPath $importVerifier -Algorithm SHA256).Hash
$script:k6Evidence.restartMethod='Exact owned process path/start-time verified, force-stopped, then reopened with service stopped; crash recovery, not clean-exit acceptance'
function Archive-Pins([string]$directory) {
    @(Get-ChildItem -LiteralPath $directory -File | Sort-Object Name | ForEach-Object {
        @{name=$_.Name;bytes=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash}
    })
}
function Import-Archive([string]$path,[string]$destination) {
    Send-K6 1 'app.worldFile' @{command='komaphone.world.restore';params=@{path=$path;canvas=$destination}}|Out-Null
    $done=Wait-K6 1 {param($s) $s.export -and -not $s.export.busy} 'portable import completed' 180
    Assert-K6 (-not $done.media_error -and @($done.items|Where-Object canvas -eq $destination).Count -eq 1) "Import failed: $($done.media_error)"
    Cmd-K6 1 'komaphone.world.open' @{canvas=$destination}|Out-Null
    Idle-K6 1|Out-Null
    Cmd-K6 1 'komaphone.world.members'|Out-Null
    Wait-K6 1 {param($s) $s.members.Count -eq 1 -and $s.members[0].owner -and $s.members[0].profile -eq $s.account.profile -and $s.requests_pending -eq 0} 'only importer has membership'|Out-Null
}
function Compare-Archive([string]$source,[string]$restored,[string]$name) {
    & $PythonExe (Join-Path $PSScriptRoot 'verify-k7-import.py') $source $restored --output (Join-Path $k6Root "$name.json")
    Assert-K6 ($LASTEXITCODE -eq 0) 'Independent archive comparison failed'
}
try {
    Verify-Build-K6
    $before=Archive-Pins $archiveSource
    $endpoint=Start-Service-K6
    Start-App-K6 1
    Cmd-K6 1 'komaphone.room.connect' @{endpoint=$endpoint;display_name='K7 synthetic archive importer'}|Out-Null
    Wait-K6 1 {param($s) $s.status.paired -and $s.status.connected -and -not $s.busy} 'fresh local service pairing' 60 'komaphone.room.status'|Out-Null
    Wait-K6 1 {param($s) $s.connected -and $s.account} 'authenticated world account'|Out-Null
    Open-World-K6 1
    $initial=Status-K6 1
    Assert-K6 (-not $initial.active -and $initial.items.Count -eq 0) 'Fresh service unexpectedly has a canvas'
    Click-K6 1 'world.section.restore'
    Screenshot-K6 1 'restore-with-no-canvas'|Out-Null
    $input=Join-Path $script:k6Apps[1].Root 'synthetic-input.komworld'
    New-Item -ItemType Directory -Path $input|Out-Null
    foreach($file in Get-ChildItem -LiteralPath $archiveSource -File){Copy-Item -LiteralPath $file.FullName -Destination (Join-Path $input $file.Name)}
    $first=Id-K6
    Import-Archive $input $first
    $firstArchive=Export-K6 'archive' 'first-import.komworld'
    Compare-Archive $input $firstArchive 'first-comparison'
    Screenshot-K6 1 'first-restored-art'|Out-Null
    $firstWorld=(Get-Content -LiteralPath (Join-Path $firstArchive 'world.json') -Raw|ConvertFrom-Json).world
    $copiedSource=$firstWorld.strokes.PSObject.Properties.Value|Where-Object {$_.visible -and -not $_.provenance}|Select-Object -First 1
    Assert-K6 ([bool]$copiedSource) 'Fixture has no visible plain imported stroke'
    $copiedSource=$copiedSource.stroke
    Cmd-K6 1 'komaphone.world.stroke.copy' @{stroke=$copiedSource.id}|Out-Null
    $pasteId=Id-K6
    Cmd-K6 1 'komaphone.world.stroke.paste' @{id=$pasteId;anchor=@{depth=0;x='0';y='0'};offset=@(400.0,0.0);scale=1.0}|Out-Null
    Idle-K6 1|Out-Null
    $copyArchive=Export-K6 'archive' 'copied-import.komworld'
    $copyWorld=(Get-Content -LiteralPath (Join-Path $copyArchive 'world.json') -Raw|ConvertFrom-Json).world
    $copied=$copyWorld.strokes.PSObject.Properties[$pasteId].Value
    Assert-K6 ($copied -and $copied.stroke.owner -eq (Status-K6 1).account.profile -and $copied.provenance.author.label -like 'Archive author *') 'Imported stroke could not be copied with explicit archive credit'
    $prior=(Status-K6 1).active.canonical_revision
    Stroke-K6 1 $first -330|Out-Null
    Idle-K6 1|Out-Null
    Assert-K6 ((Status-K6 1).active.canonical_revision -eq $prior+1) 'New importer edit was not accepted'
    Cmd-K6 1 'komaphone.world.rename' @{canvas=$first;name='Edited restored archive'}|Out-Null
    Idle-K6 1|Out-Null
    $editedArchive=Export-K6 'archive' 'edited-import.komworld'
    $state=Status-K6 1
    Cmd-K6 1 'komaphone.world.delete' @{canvas=$first;expected_epoch=$state.active.info.epoch}|Out-Null
    Wait-K6 1 {param($s) -not $s.active -and $s.requests_pending -eq 0} 'source moved to Trash before reimport'|Out-Null
    $second=Id-K6
    Import-Archive $editedArchive $second
    $secondArchive=Export-K6 'archive' 'second-import.komworld'
    Compare-Archive $editedArchive $secondArchive 'second-comparison'
    $accepted=Scene-K6 1
    Assert-K6 ($accepted.hashIncluded -and $accepted.canonicalHash -match '^[0-9a-fA-F]{64}$') 'Accepted world has no valid canonical hash'
    Stop-App-K6 1
    Stop-Service-K6
    Start-App-K6 1
    Wait-K6 1 {param($s) $s.account} 'offline account scope restored'|Out-Null
    Open-World-K6 1
    Cmd-K6 1 'komaphone.world.open' @{canvas=$second}|Out-Null
    Wait-K6 1 {param($s) $s.active -and $s.active.info.canvas -eq $second -and $s.active.canonical_revision -eq $accepted.revision} 'imported history reopened without service'|Out-Null
    $reopened=Scene-K6 1
    Assert-K6 ($reopened.hashIncluded -and $reopened.canonicalHash -match '^[0-9a-fA-F]{64}$' -and $reopened.canonicalHash -eq $accepted.canonicalHash) 'Offline reopen changed imported history'
    Screenshot-K6 1 'offline-restored-art'|Out-Null
    Assert-K6 ((Json-K6 (Archive-Pins $archiveSource)) -ceq (Json-K6 $before)) 'Historical source archive changed'
    Assert-K6 ((Json-K6 (Archive-Pins $input)) -ceq (Json-K6 $before)) 'Import changed its input archive'
    Record-K6 'portable archive native acceptance' @{source=$archiveSource;first=$first;second=$second;sourceAbsentInitially=$true;sourceTrashedBeforeReimport=$true;offlineHash=$accepted.canonicalHash;originalPins=$before}
    $script:k6Evidence.passed=$true
} catch {
    $script:k6Evidence.error=$_.Exception.Message
    throw
} finally {
    $cleanupErrors=[System.Collections.Generic.List[string]]::new()
    foreach($n in @($script:k6Apps.Keys)){try{Stop-App-K6 $n}catch{$cleanupErrors.Add($_.Exception.Message)}}
    try{Stop-Service-K6}catch{$cleanupErrors.Add($_.Exception.Message)}
    foreach($app in $script:k6Apps.Values){
        $database=Join-Path $app.DataRoot 'rooms.sqlite3'
        if(Test-Path -LiteralPath $database){try{& $CredentialCleanupExe '--discard-isolated-test-credentials' $database|Out-Null;if($LASTEXITCODE -ne 0){throw 'Isolated credential cleanup failed'}}catch{$cleanupErrors.Add($_.Exception.Message)}}
    }
    foreach($key in $script:k6Original.Keys){[Environment]::SetEnvironmentVariable($key,$script:k6Original[$key],'Process')}
    if($cleanupErrors.Count -gt 0){$script:k6Evidence.passed=$false}
    $script:k6Evidence.steps=$script:k6Steps.ToArray();$script:k6Evidence.inputs=$script:k6Inputs.ToArray();$script:k6Evidence.cleanupErrors=$cleanupErrors.ToArray();$script:k6Evidence.finished=[DateTime]::UtcNow.ToString('o')
    $script:k6Evidence|ConvertTo-Json -Depth 70|Set-Content -LiteralPath (Join-Path $k6Root 'result.json') -Encoding utf8
    Write-Output "K7 import evidence: $k6Root"
    if($cleanupErrors.Count -gt 0){throw 'Native fixture cleanup incomplete; inspect cleanupErrors'}
}
