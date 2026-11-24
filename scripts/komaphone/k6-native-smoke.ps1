#Requires -Version 7.0
# Real Windows clients and an isolated loopback service. No Google/cloud acceptance claim.
# Secrets stay in memory or the app's scoped credential store, never result.json.
param(
    [Parameter(Mandatory=$true)][string]$AppExe,
    [Parameter(Mandatory=$true)][string]$ServiceExe,
    [Parameter(Mandatory=$true)][string]$CredentialCleanupExe,
    [string]$BuildEvidence='',
    [string]$SourcePins='',
    [string]$BuildScript='',
    [ValidateRange(1,65535)][int]$WorldProtocolVersion=1,
    [string]$ImportVerifierPython='',
    [switch]$Diagnostic,
    [switch]$Stress,
    [switch]$Probe,
    [switch]$Presence,
    [switch]$PresencePngDiagnostic,
    [switch]$FunctionsOnly
)
$ErrorActionPreference='Stop'
if(([int][bool]$Stress+[int][bool]$Probe+[int][bool]$Presence) -gt 1){throw 'Choose one of Stress, Probe or Presence'}
if($PresencePngDiagnostic -and -not $Presence){throw 'PresencePngDiagnostic requires Presence'}
$k6Repo=(Resolve-Path (Join-Path $PSScriptRoot '../..')).Path
$AppExe=(Resolve-Path -LiteralPath $AppExe).Path
$ServiceExe=(Resolve-Path -LiteralPath $ServiceExe).Path
$CredentialCleanupExe=(Resolve-Path -LiteralPath $CredentialCleanupExe).Path
$k6Root=Join-Path $k6Repo ('log/k6/native/run-'+[guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $k6Root | Out-Null
Copy-Item -LiteralPath $PSCommandPath -Destination (Join-Path $k6Root 'harness.ps1')
$script:k6Apps=@{}; $script:k6Service=$null; $script:k6Port=0; $script:k6Generation=0
$script:k6Proxy=$null;$script:k6HttpEndpoint=$null
$script:k6ClipboardCaptured=$false;$script:k6OwnedClipboard=$null;$script:k6OriginalClipboard=$null
$script:k6Original=@{}
foreach($key in @('KOMAPHONE_DATA_DIR','PHOTOCRAFT_CONFIG_DIR','KOMAPHONE_SERVICE_ENDPOINT','KOMAPHONE_TRUSTED_CERT')){
    $script:k6Original[$key]=[Environment]::GetEnvironmentVariable($key,'Process')
}
$script:k6Steps=[System.Collections.Generic.List[object]]::new()
$script:k6Inputs=[System.Collections.Generic.List[object]]::new()
$script:k6FirstMismatch=@{};$script:k6LastCommand=@{}
$script:k6Evidence=[ordered]@{
    passed=$false; mode=$(if($Diagnostic){'diagnostic'}else{'acceptance-candidate'})
    appSha256=(Get-FileHash -LiteralPath $AppExe -Algorithm SHA256).Hash
    serviceSha256=(Get-FileHash -LiteralPath $ServiceExe -Algorithm SHA256).Hash
    harnessSha256=(Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
    worldProtocolVersion=$WorldProtocolVersion
    root=$k6Root; started=[DateTime]::UtcNow.ToString('o'); googleTested=$false; cloudDeployed=$false
    scope='Two actual native clients plus six headless synthetic members; isolated local plaintext loopback service; real egui shared-canvas pointer input; capability-scoped exports'
    scenario=$(if($Presence){'visible previews at100ms RTT'}elseif($Probe){'two-native response probe'}elseif($Stress){'131072-point shared capacity'}else{'regular shared acceptance'})
}
function Json-K6($value){ConvertTo-Json -InputObject $value -Depth 70 -Compress}
function Preserve-Clipboard-K6{
    Assert-K6 ([System.Threading.Thread]::CurrentThread.GetApartmentState() -eq 'STA') 'Run this Windows fixture with pwsh -STA so all clipboard formats can be preserved'
    Add-Type -AssemblyName System.Windows.Forms
    $snapshot=[System.Windows.Forms.DataObject]::new();$current=[System.Windows.Forms.Clipboard]::GetDataObject()
    if($current){foreach($format in $current.GetFormats($false)){
        $value=$current.GetData($format,$false)
        if($null -ne $value){$snapshot.SetData($format,$false,$value)}
    }}
    $script:k6OriginalClipboard=$snapshot;$script:k6ClipboardCaptured=$true
}
function Id-K6{[guid]::NewGuid().ToString('N')}
function Record-K6([string]$label,$value){$script:k6Steps.Add(@{label=$label;value=$value;at=[DateTime]::UtcNow.ToString('o')});Write-Host ('K6: '+$label)}
function Assert-K6([bool]$condition,[string]$message){if(-not $condition){throw $message}}
function Verify-Build-K6{
    if(-not $BuildEvidence -or -not $SourcePins -or -not $BuildScript){
        Assert-K6 ($Diagnostic -or $Probe) 'Full acceptance requires BuildEvidence, SourcePins and BuildScript for the frozen native binary'
        $script:k6Evidence.buildBinding='Diagnostic/probe only; final build/source evidence not supplied';return
    }
    $buildPath=(Resolve-Path -LiteralPath $BuildEvidence).Path;$sourcePath=(Resolve-Path -LiteralPath $SourcePins).Path;$scriptPath=(Resolve-Path -LiteralPath $BuildScript).Path
    $build=Get-Content -LiteralPath $buildPath -Raw|ConvertFrom-Json;$pins=@(Get-Content -LiteralPath $sourcePath -Raw|ConvertFrom-Json)
    Assert-K6 ($build.exit -eq 0 -and @($build.changed_sources).Count -eq 0) 'The supplied native build did not pass with frozen sources'
    Assert-K6 ($pins.Count -gt 0 -and $build.command -like 'cargo build *') 'The supplied build/source evidence is incomplete'
    $changed=@(foreach($pin in $pins){$path=Join-Path $k6Repo $pin.path;if(-not (Test-Path -LiteralPath $path) -or (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $pin.sha256){$pin.path}})
    $script:k6Evidence.buildBinding=@{
        build=@{path=$buildPath;sha256=(Get-FileHash -LiteralPath $buildPath -Algorithm SHA256).Hash;command=$build.command;seconds=$build.seconds}
        source=@{path=$sourcePath;sha256=(Get-FileHash -LiteralPath $sourcePath -Algorithm SHA256).Hash;files=$pins.Count;changedSinceBuild=$changed}
        script=@{path=$scriptPath;sha256=(Get-FileHash -LiteralPath $scriptPath -Algorithm SHA256).Hash}
        profile='Cargo dev profile; exact profile and environment definitions are pinned by Cargo.toml and the recorded build script'
        cargoTomlSha256=(Get-FileHash -LiteralPath (Join-Path $k6Repo 'Cargo.toml') -Algorithm SHA256).Hash
    }
    Assert-K6 ($changed.Count -eq 0) 'Native acceptance sources changed after the supplied frozen build'
}
function Port-K6{
    $listener=[System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback,0)
    $listener.Start();$port=$listener.LocalEndpoint.Port;$listener.Stop();$port
}
function Stop-Owned-K6($owned,[string]$expected){
    if($owned -and -not $owned.Process.HasExited){
        $actual=Get-Process -Id $owned.Process.Id -ErrorAction Stop
        Assert-K6 ($actual.StartTime.ToUniversalTime().Ticks -eq $owned.StartTicks -and $actual.Path -eq $expected) 'Refusing to stop a process whose identity changed'
        Stop-Process -Id $actual.Id -Force;$owned.Process.WaitForExit()
    }
}
function Start-Service-K6{
    $script:k6Generation++;$directory=Join-Path $k6Root 'authority'
    New-Item -ItemType Directory -Force -Path $directory|Out-Null
    Assert-K6 ((Get-FileHash -LiteralPath $ServiceExe -Algorithm SHA256).Hash -eq $script:k6Evidence.serviceSha256) 'Service binary changed during acceptance'
    $stdout=Join-Path $k6Root "service-$script:k6Generation.stdout.log"
    $process=Start-Process -FilePath $ServiceExe -ArgumentList @('--bind',"127.0.0.1:$script:k6Port",'--data-dir',('"'+$directory+'"')) -WindowStyle Hidden -PassThru -WorkingDirectory $k6Repo -RedirectStandardOutput $stdout -RedirectStandardError (Join-Path $k6Root "service-$script:k6Generation.stderr.log")
    $script:k6Service=@{Process=$process;StartTicks=$process.StartTime.ToUniversalTime().Ticks}
    $deadline=[DateTime]::UtcNow.AddSeconds(45)
    do{
        Assert-K6 (-not $process.HasExited) 'Service exited during startup'
        $line=Get-Content -LiteralPath $stdout -ErrorAction SilentlyContinue|Where-Object{$_ -match '^\{'}|Select-Object -First 1
        if($line){$ready=$line|ConvertFrom-Json;if($ready.ready){$script:k6Port=[int]($ready.address -split ':')[-1];return "http://127.0.0.1:$script:k6Port"}}
        Start-Sleep -Milliseconds 100
    }while([DateTime]::UtcNow -lt $deadline)
    throw 'Service startup timed out'
}
function Stop-Service-K6{Stop-Owned-K6 $script:k6Service $ServiceExe;$script:k6Service=$null}
function Connect-K6($app){
    if($app.Client){$app.Client.Dispose();$app.Client=$null}
    $client=[System.Net.Sockets.TcpClient]::new()
    try{
        $client.Connect('127.0.0.1',$app.Port);$stream=$client.GetStream();$stream.ReadTimeout=60000
        $writer=[System.IO.StreamWriter]::new($stream,[System.Text.UTF8Encoding]::new($false));$writer.AutoFlush=$true
        $reader=[System.IO.StreamReader]::new($stream)
        $writer.WriteLine((Json-K6 @{id=(Id-K6);method='auth';params=@{token=(Get-Content -LiteralPath $app.Token -Raw).Trim()}}))
        $reply=$reader.ReadLine()|ConvertFrom-Json
        Assert-K6 $reply.ok 'Isolated control authentication failed'
        $app.Client=$client;$app.Writer=$writer;$app.Reader=$reader;$app.LastUse=[DateTime]::UtcNow
    }catch{$client.Dispose();throw}
}
function Send-K6([int]$number,[string]$method,[hashtable]$params=@{}){
    $app=$script:k6Apps[$number];Assert-K6 ($app -and -not $app.Process.HasExited) "Client $number unavailable"
    if(-not $app.Client -or ([DateTime]::UtcNow-$app.LastUse).TotalSeconds -gt 20){Connect-K6 $app}
    $app.Writer.WriteLine((Json-K6 @{id=(Id-K6);method=$method;params=$params}))
    $line=$app.Reader.ReadLine();Assert-K6 ([bool]$line) "Client $number control closed during $method; do not blindly retry mutations"
    $reply=$line|ConvertFrom-Json;$app.LastUse=[DateTime]::UtcNow
    Assert-K6 $reply.ok "Client $number $method refused: $($reply.error)"
    $reply.result
}
function Cmd-K6([int]$number,[string]$command,[hashtable]$params=@{}){
    $value=Send-K6 $number 'engine.execute' @{command=$command;params=$params}
    if($command -eq 'komaphone.world.status'){
        if($value.error -eq 'Service response does not match its canvas request' -and -not $script:k6FirstMismatch.ContainsKey($number)){
            $script:k6FirstMismatch[$number]=$true
            Record-K6 'first unexpected service response mismatch' @{client=$number;previousCommand=$script:k6LastCommand[$number];error=$value.error;canvas=$value.active.info.canvas;canonicalRevision=$value.active.canonical_revision;displayRevision=$value.active.display_revision;pending=$value.active.pending;requestsPending=$value.requests_pending;sequenceAvailable=$false}
        }
    }else{$script:k6LastCommand[$number]=$command}
    $value
}
function Status-K6([int]$number){Cmd-K6 $number 'komaphone.world.status'}
function Wait-K6([int]$number,[scriptblock]$condition,[string]$label,[int]$seconds=60,[string]$command='komaphone.world.status'){
    $deadline=[DateTime]::UtcNow.AddSeconds($seconds)
    do{$state=Cmd-K6 $number $command;if(& $condition $state){return $state};Start-Sleep -Milliseconds 80}while([DateTime]::UtcNow -lt $deadline)
    # World status intentionally excludes invitation capabilities. Room status is not persisted here.
    if($command -eq 'komaphone.world.status'){Record-K6 "timeout: $label" $state}
    if($command -eq 'komaphone.room.status'){Record-K6 "timeout: $label" @{client=$number;busy=$state.busy;pending=$state.status.pending;blocked=$state.status.blocked;connected=$state.status.connected;error=$state.status.error;room=$state.status.room.id;revision=$state.status.room.revision;phase=$state.status.room.phase;round=$state.status.room.round;ready=@($state.status.room.members|ForEach-Object{@{profile=$_.participant.id;ready=$_.ready}})}}
    throw "Client $number timed out: $label"
}
function Idle-K6([int]$number){Wait-K6 $number {param($s) $s.active -and -not $s.busy -and $s.requests_pending -eq 0 -and $s.active.pending -eq 0 -and $s.active.canonical_revision -eq $s.active.display_revision} 'accepted canvas edits and completed service requests'}
function Start-App-K6([int]$number){
    Assert-K6 ((Get-FileHash -LiteralPath $AppExe -Algorithm SHA256).Hash -eq $script:k6Evidence.appSha256) 'App binary changed during acceptance'
    $root=Join-Path $k6Root "client-$number";New-Item -ItemType Directory -Force -Path $root|Out-Null
    $old=$script:k6Apps[$number];$generation=if($old){$old.Generation+1}else{1}
    $env:KOMAPHONE_DATA_DIR=Join-Path $root 'data';$env:PHOTOCRAFT_CONFIG_DIR=Join-Path $root 'config'
    $env:KOMAPHONE_SERVICE_ENDPOINT=$null;$env:KOMAPHONE_TRUSTED_CERT=$null
    $port=Port-K6;$token=Join-Path $root 'control.token'
    $arguments=@('--new-instance','--control',"$port",'--control-token-file',('"'+$token+'"'),'--automation-read-root',('"'+$root+'"'),'--automation-write-root',('"'+$root+'"'))
    $process=Start-Process -FilePath $AppExe -ArgumentList $arguments -WindowStyle Hidden -PassThru -WorkingDirectory $k6Repo -RedirectStandardOutput (Join-Path $root "app-$generation.stdout.log") -RedirectStandardError (Join-Path $root "app-$generation.stderr.log")
    $app=@{Process=$process;StartTicks=$process.StartTime.ToUniversalTime().Ticks;Root=$root;DataRoot=$env:KOMAPHONE_DATA_DIR;Generation=$generation;Port=$port;Token=$token;Client=$null}
    $script:k6Apps[$number]=$app;$deadline=[DateTime]::UtcNow.AddSeconds(75)
    do{Assert-K6 (-not $process.HasExited) "Client $number exited during startup";try{Connect-K6 $app;break}catch{Start-Sleep -Milliseconds 100}}while([DateTime]::UtcNow -lt $deadline)
    Assert-K6 ([bool]$app.Client) "Client $number startup timed out"
    Send-K6 $number 'ui.resize' @{width=1920;height=1080}|Out-Null
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    do{$ui=Send-K6 $number 'ui.inspect';if([Math]::Abs($ui.window.width-1920) -le 2 -and [Math]::Abs($ui.window.height-1080) -le 2){return};Start-Sleep -Milliseconds 80}while([DateTime]::UtcNow -lt $deadline)
    throw "Client $number did not reach its requested1920x1080 native viewport"
}
function Stop-App-K6([int]$number){$app=$script:k6Apps[$number];if($app.Client){$app.Client.Dispose();$app.Client=$null};Stop-Owned-K6 $app $AppExe}
function Click-K6([int]$number,[string]$id){
    $deadline=[DateTime]::UtcNow.AddSeconds(20);$prior='';$frame=-1;$stable=0
    $scrollSteps=0;$nextScroll=[DateTime]::UtcNow.AddMilliseconds(500)
    do{
        $ui=Send-K6 $number 'ui.inspect';$rect=$ui.roomControls.$id
        $minY=if($id -in @('world.return','world.local') -or $id -like 'workspace.*'){0}else{35}
        if($rect -and $rect.Count -eq 4 -and $rect[0] -ge 0 -and $rect[1] -ge $minY -and $rect[2] -le $ui.window.width -and $rect[3] -le $ui.window.height-3){
            $key=$rect -join ',';if($key -eq $prior -and $frame -ne $ui.frame){$stable++}else{$stable=1};$prior=$key;$frame=$ui.frame
            if($stable -ge 2){Send-K6 $number 'ui.click' @{x=($rect[0]+$rect[2])/2;y=($rect[1]+$rect[3])/2}|Out-Null;$script:k6Inputs.Add(@{client=$number;kind='click';id=$id;rect=$rect;frame=$ui.frame});return}
        }elseif($rect -and $ui.world.open -and $ui.worldScene -and ($id -like 'world.*' -or $id -like 'komaphone.world.*')){
            $delta=if($rect[1] -lt 35){400}else{-400}
            Send-K6 $number 'ui.worldInput' @{kind='wheel';x=150;y=400;deltaY=$delta;unit='point'}|Out-Null;$stable=0
        }elseif(-not $rect -and $ui.world.open -and $ui.roomControls.'world.drawing' -and ($id -like 'world.*' -or $id -like 'komaphone.world.*') -and $scrollSteps -lt 24 -and [DateTime]::UtcNow -ge $nextScroll){
            # Clipped controls deliberately have no inspector rectangle. Discover them
            # by bounded real sidebar scrolling, then click only an observed stable rect.
            $drawing=$ui.roomControls.'world.drawing'
            Assert-K6 ($drawing[0] -gt 150 -and $ui.window.height -gt 450) 'Sidebar discovery point must remain outside the drawing surface'
            $delta=if($scrollSteps -lt 8 -or $scrollSteps -ge 16){-400}else{400}
            Send-K6 $number 'ui.worldInput' @{kind='wheel';x=150;y=400;deltaY=$delta;unit='point'}|Out-Null
            $script:k6Inputs.Add(@{client=$number;kind='sidebar-discovery-wheel';target=$id;deltaY=$delta;frame=$ui.frame})
            $scrollSteps++;$nextScroll=[DateTime]::UtcNow.AddMilliseconds(250);$stable=0
        }
        Start-Sleep -Milliseconds 70
    }while([DateTime]::UtcNow -lt $deadline)
    Record-K6 'control visibility timeout' @{client=$number;id=$id;window=$ui.window;controls=$ui.roomControls;world=@{open=$ui.world.open;privateSourceFields='Intentionally omitted by UI serialization; inspect command status instead'};sidebarDiscoverySteps=$scrollSteps;status=(Status-K6 $number)}
    # Accepted capability fields are cleared by Accept-K6 before later captures.
    try{Screenshot-K6 $number 'control-visibility-timeout'|Out-Null}catch{Record-K6 'timeout screenshot unavailable' @{client=$number}}
    throw "Client $number control not visibly clickable: $id"
}
function Open-World-K6([int]$number){
    $ui=Send-K6 $number 'ui.inspect'
    if(-not $ui.world.open){$id=if($ui.roomControls.'home.worlds'){'home.worlds'}else{'workspace.worlds'};Click-K6 $number $id}
}
function Invitation-K6([int]$owner,[string]$recipient){
    $previous=(Status-K6 $owner).invitation_id
    Cmd-K6 $owner 'komaphone.world.invite' @{recipient=$recipient}|Out-Null
    $issued=Wait-K6 $owner {param($s) $s.invitation_id -and $s.invitation_id -ne $previous -and $s.requests_pending -eq 0} 'new invitation committed'
    $ui=Send-K6 $owner 'ui.inspect';if(-not $ui.roomControls.'world.invitation.copy'){Click-K6 $owner 'world.section.people'}
    # If the user copied something during this run, preserve that newest external value.
    if($null -eq $script:k6OwnedClipboard -or (Get-Clipboard -Raw) -cne $script:k6OwnedClipboard){Preserve-Clipboard-K6}
    Click-K6 $owner 'world.invitation.copy'
    # Preserve the user's clipboard only in memory; read capabilities after this owned Copy.
    $deadline=[DateTime]::UtcNow.AddSeconds(5)
    do{Start-Sleep -Milliseconds 50;$code=Get-Clipboard -Raw;try{$value=$code|ConvertFrom-Json;if($value.canvas -and $value.id -eq $issued.invitation_id -and $value.secret){$script:k6OwnedClipboard=$code;return $value}}catch{}}while([DateTime]::UtcNow -lt $deadline)
    throw 'Owned invitation Copy did not publish the complete capability'
}
function Accept-K6([int]$number,$invite){
    Open-World-K6 $number;$ui=Send-K6 $number 'ui.inspect'
    if(-not $ui.roomControls.'world.invitation'){Click-K6 $number 'world.section.accept'}
    Click-K6 $number 'world.invitation';Send-K6 $number 'ui.key' @{key='A';ctrl=$true}|Out-Null
    Send-K6 $number 'ui.type' @{text=(Json-K6 $invite)}|Out-Null;Click-K6 $number 'world.invitation.accept'
    Wait-K6 $number {param($s) -not $s.busy -and @($s.items|Where-Object canvas -eq $invite.canvas).Count -eq 1} 'invitation accepted'|Out-Null
    Click-K6 $number 'world.invitation';Send-K6 $number 'ui.key' @{key='A';ctrl=$true}|Out-Null;Send-K6 $number 'ui.key' @{key='Backspace'}|Out-Null
    Click-K6 $number 'world.section.accept'
    Cmd-K6 $number 'komaphone.world.open' @{canvas=$invite.canvas}|Out-Null;Idle-K6 $number
}
function Scene-K6([int]$number){(Send-K6 $number 'ui.inspect').worldScene}
function Converged-K6([string]$canvas,[int]$seconds=60){
    $deadline=[DateTime]::UtcNow.AddSeconds($seconds)
    do{$a=Status-K6 1;$b=Status-K6 2;if($a.active.info.canvas -eq $canvas -and $b.active.info.canvas -eq $canvas -and $a.active.pending -eq 0 -and $b.active.pending -eq 0){$x=Scene-K6 1;$y=Scene-K6 2;if($x.canonicalHash -and $x.canonicalHash -eq $y.canonicalHash){return @{canvas=$canvas;hash=$x.canonicalHash;revision=$x.revision;first=$x;second=$y}}};Start-Sleep -Milliseconds 100}while([DateTime]::UtcNow -lt $deadline)
    throw 'Native canonical scenes did not converge'
}
function Stroke-K6([int]$number,[string]$canvas,[double]$y,[string]$strokeId=''){
    $params=@{canvas=$canvas;anchor=@{depth=0;x='0';y='0'};points=@(@(-100.0,$y),@(100.0,$y));width=8.0;color=@{rgba=@(0.12,0.35,0.7,1.0)};erase=$false}
    if($strokeId){$params.id=$strokeId}
    Cmd-K6 $number 'komaphone.world.stroke' $params
}
function Pointer-K6([int]$number){
    $before=Idle-K6 $number;$ui=Send-K6 $number 'ui.inspect';$rect=$ui.roomControls.'world.drawing'
    Assert-K6 ([bool]$rect) 'Shared drawing rectangle missing';$x=($rect[0]+$rect[2])/2;$y=($rect[1]+$rect[3])/2
    $offsets=@(@(-150,100),@(-150,-60),@(0,-170),@(150,-60),@(150,100),@(-150,100))
    for($i=0;$i -lt $offsets.Count;$i++){$kind=if($i -eq 0){'down'}else{'move'};$p=@{kind=$kind;x=$x+$offsets[$i][0];y=$y+$offsets[$i][1]};Send-K6 $number 'ui.worldInput' $p|Out-Null;$script:k6Inputs.Add(@{client=$number;kind='world-input';params=$p})}
    $last=$offsets[-1];Send-K6 $number 'ui.worldInput' @{kind='up';x=$x+$last[0];y=$y+$last[1]}|Out-Null
    Wait-K6 $number {param($s) $s.active.stroke_count -eq $before.active.stroke_count+1 -and $s.active.pending -eq 0} 'retained native pointer stroke'|Out-Null
    $scene=Scene-K6 $number;$added=@($scene.strokes|Where-Object createdRevision -gt $before.active.canonical_revision)
    Assert-K6 ($added.Count -eq 1 -and $added[0].samples -eq $offsets.Count) 'Pointer sample integrity failed; physical pointer input may have interleaved'
    Record-K6 'actual egui pointer samples' @{samples=$offsets.Count;stroke=$added[0];source='ui.worldInput down/move/up through real frames'}
}
function Http-K6([string]$path,$body,[string]$token='',[int]$expected=200){
    $headers=@{};if($token){$headers.Authorization='Bearer '+$token}
    $base=if($script:k6HttpEndpoint){$script:k6HttpEndpoint}else{"http://127.0.0.1:$script:k6Port"}
    $response=Invoke-WebRequest -Uri ($base+$path) -Method Post -ContentType 'application/json' -Headers $headers -Body (Json-K6 $body) -TimeoutSec 30 -SkipHttpErrorCheck
    Assert-K6 ($response.StatusCode -eq $expected) "Synthetic endpoint $path returned unexpected status $($response.StatusCode)"
    $response.Content|ConvertFrom-Json
}
function World-K6($actor,$action,[int]$expected=200){Http-K6 '/v1/canvas' @{version=$WorldProtocolVersion;action=$action} $actor.token $expected}
function Headless-K6([int]$number){
    $base=if($script:k6HttpEndpoint){$script:k6HttpEndpoint}else{"http://127.0.0.1:$script:k6Port"}
    $health=(Invoke-WebRequest -Uri ($base+'/health') -TimeoutSec 10).Content|ConvertFrom-Json
    $rpcVersion=if($health.protocol_version){[int]$health.protocol_version}else{1}
    $actor=@{profile=(Id-K6);device=(Id-K6);token=((Id-K6)+(Id-K6))}
    Http-K6 '/v1/rpc' @{protocol_version=$rpcVersion;request_id=(Id-K6);operation=@{kind='pair';participant=@{id=$actor.profile;device=$actor.device;display_name="K6 synthetic member $number"};credential=$actor.token}}|Out-Null
    $actor
}
function Export-K6([string]$format,[string]$name,$extra=@{}){
    Wait-K6 1 {param($s) $s.active -and -not $s.busy -and $s.active.pending -eq 0} 'durable viewer and accepted artwork before export'|Out-Null
    $path=Join-Path $script:k6Apps[1].Root $name;$params=@{format=$format;path=$path};foreach($key in $extra.Keys){$params[$key]=$extra[$key]}
    Send-K6 1 'app.worldFile' @{command='komaphone.world.export';params=$params}|Out-Null
    $done=Wait-K6 1 {param($s) $s.export -and -not $s.export.busy} "completed $format export" 120
    Assert-K6 (-not $done.media_error -and (Test-Path -LiteralPath $path)) "Export $format failed: $($done.media_error)"
    $files=if((Get-Item -LiteralPath $path).PSIsContainer){@(Get-ChildItem -LiteralPath $path -File -Recurse)}else{@(Get-Item -LiteralPath $path)}
    Assert-K6 ($files.Count -gt 0) 'Export contained no files'
    $hashes=@($files|ForEach-Object{@{path=$_.FullName;bytes=$_.Length;sha256=(Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash}})
    Record-K6 "export $format" @{status=$done.export;files=$hashes};$path
}
function Image-K6([string]$path,[int]$width,[int]$height){
    Add-Type -AssemblyName System.Drawing
    $image=[System.Drawing.Image]::FromFile($path)
    try{
        Assert-K6 ($image.Width -eq $width -and $image.Height -eq $height) 'Export dimensions do not match the requested view'
        $frames=$image.GetFrameCount([System.Drawing.Imaging.FrameDimension]::new($image.FrameDimensionsList[0]))
        @{width=$image.Width;height=$image.Height;frames=$frames;decoded=$true}
    }finally{$image.Dispose()}
}
function Copied-World-K6($world,[string]$destination){
    # Mirror only the namespace mapping; retain provenance, author/device IDs and all history.
    $copy=(Json-K6 $world)|ConvertFrom-Json;$copy.canvas=$destination
    foreach($property in $copy.strokes.PSObject.Properties){$property.Value.stroke.canvas=$destination}
    foreach($property in $copy.media.PSObject.Properties){$property.Value.media.canvas=$destination}
    foreach($property in $copy.tours.PSObject.Properties){$property.Value.canvas=$destination;foreach($stop in $property.Value.stops){$stop.camera.canvas=$destination}}
    foreach($event in $copy.events){
        $event.op.canvas=$destination
        switch($event.op.action.kind){
            'add_stroke'{$event.op.action.stroke.canvas=$destination}
            'copy_stroke'{$event.op.action.stroke.canvas=$destination}
            'place_media'{$event.op.action.media.canvas=$destination}
            'put_tour'{$event.op.action.tour.canvas=$destination;foreach($stop in $event.op.action.tour.stops){$stop.camera.canvas=$destination}}
        }
    }
    $copy
}
function Screenshot-K6([int]$number,[string]$name,[bool]$settle=$true){
    $params=@{path="$name.png";focus=$false};if(-not $settle){$params.settle=$false}
    Send-K6 $number 'ui.screenshot' $params|Out-Null;Join-Path $script:k6Apps[$number].Root "$name.png"
}
function Thumbnail-K6([int]$number,[string]$canvas,[string]$name){
    Add-Type -AssemblyName System.Drawing
    $deadline=[DateTime]::UtcNow.AddSeconds(45)
    do{
        $ui=Send-K6 $number 'ui.inspect';$rect=$ui.roomControls."world.thumbnail.$canvas"
        if($rect -and $rect[1] -ge 35 -and $rect[3] -le $ui.window.height-3){
            $path=Screenshot-K6 $number $name;$bitmap=[Drawing.Bitmap]::FromFile($path)
            try{
                $white=0;$dark=0;$total=0
                for($y=[int]$rect[1]+3;$y -lt [int]$rect[3]-3;$y++){for($x=[int]$rect[0]+3;$x -lt [int]$rect[2]-3;$x++){
                    $pixel=$bitmap.GetPixel([int]($x*$bitmap.Width/$ui.window.width),[int]($y*$bitmap.Height/$ui.window.height));$total++
                    if($pixel.R -gt 230 -and $pixel.G -gt 230 -and $pixel.B -gt 230){$white++}
                    if($pixel.R -lt 190 -or $pixel.G -lt 190 -or $pixel.B -lt 190){$dark++}
                }}
                if($total -gt 0 -and $white/$total -gt 0.5 -and $dark -ge 3){
                    Record-K6 'actual cached artwork thumbnail' @{client=$number;canvas=$canvas;rect=$rect;whitePixels=$white;artPixels=$dark;path=$path;sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash};return
                }
            }finally{$bitmap.Dispose()}
        }
        Start-Sleep -Milliseconds 100
    }while([DateTime]::UtcNow -lt $deadline)
    throw "Client $number did not render a real artwork thumbnail for the cached canvas"
}
function Pixel-Difference-K6([string]$before,[string]$after){
    Add-Type -AssemblyName System.Drawing
    $a=[Drawing.Bitmap]::FromFile($before);$b=[Drawing.Bitmap]::FromFile($after)
    try{
        Assert-K6 ($a.Width -eq $b.Width -and $a.Height -eq $b.Height) 'Pixel comparison dimensions changed'
        $changed=0
        for($y=0;$y -lt $a.Height;$y+=2){for($x=0;$x -lt $a.Width;$x+=2){if($a.GetPixel($x,$y).ToArgb() -ne $b.GetPixel($x,$y).ToArgb()){$changed++}}}
        Assert-K6 ($changed -ge 10) 'Native remix edit did not change at least ten decoded destination pixel samples'
        @{sampleStride=2;changedSamples=$changed;width=$a.Width;height=$a.Height}
    }finally{$a.Dispose();$b.Dispose()}
}
function Stroke-Copy-Trash-K6([string]$sourceCanvas,$reader){
    $owner=(Status-K6 2).account.profile;$copier=(Status-K6 1).account.profile
    $source=Scene-K6 2;$sourceStroke=@($source.strokes|Where-Object visible)[0]
    Assert-K6 ($sourceStroke.owner -eq $owner) 'Stroke-copy fixture source has the wrong artist'
    $invite=Invitation-K6 2 $copier;Accept-K6 1 $invite|Out-Null;$invite=$null
    $ui=Send-K6 1 'ui.inspect';if(-not $ui.roomControls.'world.people.refresh'){Click-K6 1 'world.section.people'}
    Click-K6 1 'world.people.refresh';Wait-K6 1 {param($s) @($s.people|Where-Object profile -eq $owner).Count -eq 1 -and $s.requests_pending -eq 0} 'original stroke artist available for credit'|Out-Null
    # Gallery-Source-K6 already opened this persistent section in the same client.
    # A clipped control has no rectangle; toggling its section would close it.
    $copyId="world.stroke.copy.$($sourceStroke.id)"
    Click-K6 1 $copyId
    $created=Cmd-K6 1 'komaphone.world.create' @{name='Independent copied stroke'};$destination=$created.canvas;Idle-K6 1|Out-Null
    Click-K6 1 'komaphone.world.stroke.paste'
    Wait-K6 1 {param($s) $s.active.info.canvas -eq $destination -and $s.active.stroke_count -eq 1 -and $s.active.pending -eq 0} 'native UI pasted another artist stroke into own layer'|Out-Null
    $copied=Scene-K6 1
    Assert-K6 ($copied.strokes[0].owner -eq $copier -and (Scene-K6 2).canonicalHash -eq $source.canonicalHash) 'Copying a stroke changed source art or failed to create own-author art'
    $archive=Export-K6 'archive' 'copied-stroke.komworld';$manifest=Get-Content -LiteralPath (Join-Path $archive 'world.json') -Raw|ConvertFrom-Json
    $entry=@($manifest.world.strokes.PSObject.Properties)[0].Value
    Assert-K6 ($entry.provenance.canvas -eq $sourceCanvas -and $entry.provenance.stroke -eq $sourceStroke.id -and $entry.provenance.author.profile -eq $owner -and $entry.stroke.owner -eq $copier) 'Copied stroke archive lost immutable original attribution or editable owner'
    $png=Export-K6 'png' 'copied-stroke.png' @{width=640;height=480};Image-K6 $png 640 480|Out-Null
    Cmd-K6 2 'komaphone.world.undo'|Out-Null;Idle-K6 2|Out-Null;$hiddenSource=Scene-K6 2
    Assert-K6 ((Scene-K6 1).canonicalHash -eq $copied.canonicalHash) 'Changing the original stroke altered its copied canvas'
    Cmd-K6 1 'komaphone.world.undo'|Out-Null;Idle-K6 1|Out-Null
    Assert-K6 (-not (Scene-K6 1).strokes[0].visible -and (Scene-K6 2).canonicalHash -eq $hiddenSource.canonicalHash) 'Copied stroke was not independently undoable by its new owner'
    Record-K6 'native stroke copy and paste retain credit and independent own-author editing' @{source=$sourceCanvas;destination=$destination;originalStroke=$sourceStroke.id;originalArtist=$owner;newOwner=$copier;sourceHashAtCopy=$source.canonicalHash;destinationHashAtCopy=$copied.canonicalHash;sourceAfterHide=$hiddenSource.canonicalHash}
    # Three restored members, with both actual native clients and one independent HTTP reader.
    Stroke-K6 2 $sourceCanvas 30|Out-Null;Idle-K6 2|Out-Null
    Cmd-K6 1 'komaphone.world.open' @{canvas=$sourceCanvas}|Out-Null;Idle-K6 1|Out-Null
    $invite=Invitation-K6 2 $reader.profile;$accepted=World-K6 $reader @{kind='accept';canvas=$sourceCanvas;id=$invite.id;secret=$invite.secret};$invite=$null;$oldReaderEpoch=$accepted.info.epoch
    $before=Converged-K6 $sourceCanvas;$oldOwnerEpoch=(Status-K6 2).active.info.epoch;$oldMemberEpoch=(Status-K6 1).active.info.epoch
    Click-K6 2 'komaphone.world.delete'
    Wait-K6 2 {param($s) @($s.trash|Where-Object canvas -eq $sourceCanvas).Count -eq 1 -and -not $s.busy} 'owner sees recoverable deleted canvas'|Out-Null
    World-K6 $reader @{kind='info';canvas=$sourceCanvas} 403|Out-Null
    Cmd-K6 1 'komaphone.world.list'|Out-Null
    Wait-K6 1 {param($s) $s.requests_pending -eq 0 -and @($s.items|Where-Object canvas -eq $sourceCanvas).Count -eq 0} 'deleted canvas disappears from other member directory'|Out-Null
    Stop-Service-K6;Start-Service-K6|Out-Null
    $ui=Send-K6 2 'ui.inspect';if(-not $ui.roomControls.'komaphone.world.trash'){Click-K6 2 'world.section.trash'}
    Click-K6 2 'komaphone.world.trash';Wait-K6 2 {param($s) @($s.trash|Where-Object canvas -eq $sourceCanvas).Count -eq 1 -and $s.requests_pending -eq 0} 'trash survived authority restart'|Out-Null
    Click-K6 2 "world.undelete.$sourceCanvas"
    Wait-K6 2 {param($s) $s.active.info.canvas -eq $sourceCanvas -and $s.active.info.epoch -gt $oldOwnerEpoch -and $s.active.pending -eq 0} 'native owner restored original canvas'|Out-Null
    Cmd-K6 1 'komaphone.world.list'|Out-Null;Wait-K6 1 {param($s) @($s.items|Where-Object canvas -eq $sourceCanvas).Count -eq 1 -and $s.requests_pending -eq 0} 'prior native membership restored without invitation'|Out-Null
    Cmd-K6 1 'komaphone.world.open' @{canvas=$sourceCanvas}|Out-Null;Idle-K6 1|Out-Null
    $after=Converged-K6 $sourceCanvas;$restoredReader=World-K6 $reader @{kind='info';canvas=$sourceCanvas}
    Assert-K6 ($after.hash -eq $before.hash -and $after.revision -eq $before.revision -and (Status-K6 1).active.info.epoch -gt $oldMemberEpoch -and $restoredReader.info.epoch -gt $oldReaderEpoch) 'Trash restoration lost exact history or failed to refresh every member epoch'
    Cmd-K6 2 'komaphone.world.members'|Out-Null;Wait-K6 2 {param($s) $s.members.Count -eq 3 -and $s.requests_pending -eq 0} 'all prior members restored'|Out-Null
    Record-K6 'native trash restore after authority restart preserved exact canvas and prior members' @{canvas=$sourceCanvas;before=$before.hash;after=$after.hash;revision=$after.revision;members=3;oldOwnerEpoch=$oldOwnerEpoch;newOwnerEpoch=(Status-K6 2).active.info.epoch;oldMemberEpoch=$oldMemberEpoch;newMemberEpoch=(Status-K6 1).active.info.epoch;oldReaderEpoch=$oldReaderEpoch;newReaderEpoch=$restoredReader.info.epoch}
}
function Rendered-K6([int]$number,[long]$revision){
    $deadline=[DateTime]::UtcNow.AddSeconds(90)
    do{
        $ui=Send-K6 $number 'ui.inspect'
        if(-not $ui.worldRender.preparing -and $ui.worldRender.rendered_revision -eq $revision -and $ui.worldRender.stats.vertices -gt 0 -and $ui.worldRender.stats.pending_assets -eq 0){return $ui}
        Start-Sleep -Milliseconds 80
    }while([DateTime]::UtcNow -lt $deadline)
    throw "Client $number did not present expected artwork revision $revision"
}
function House-Pixels-K6([string]$path,$ui){
    Add-Type -AssemblyName System.Drawing
    $bitmap=[System.Drawing.Bitmap]::FromFile($path)
    try{
        $rect=$ui.roomControls.'world.drawing';$centerX=($rect[0]+$rect[2])/2;$centerY=($rect[1]+$rect[3])/2
        $scaleX=$bitmap.Width/$ui.window.width;$scaleY=$bitmap.Height/$ui.window.height
        $samples=[System.Collections.Generic.List[object]]::new()
        # Midpoints of all five real-pointer house edges, away from UI and cursors.
        foreach($offset in @(@(-150,20),@(-75,-115),@(75,-115),@(150,20),@(0,100))){
            $x=[int][Math]::Round(($centerX+$offset[0])*$scaleX);$y=[int][Math]::Round(($centerY+$offset[1])*$scaleY);$dark=$false
            for($dy=-3;$dy -le 3;$dy++){for($dx=-3;$dx -le 3;$dx++){
                Assert-K6 ($x+$dx -ge 0 -and $x+$dx -lt $bitmap.Width -and $y+$dy -ge 0 -and $y+$dy -lt $bitmap.Height) 'House screenshot sample lies outside the native image'
                $pixel=$bitmap.GetPixel($x+$dx,$y+$dy)
                if($pixel.R -lt 100 -and $pixel.G -lt 100 -and $pixel.B -lt 100){$dark=$true}
            }}
            $samples.Add(@{x=$x;y=$y;darkArtwork=$dark})
            Assert-K6 $dark 'Deep bookmark screenshot is missing one of the five original house edges'
        }
        Record-K6 'deep bookmark rendered original five-edge artwork' @{path=$path;sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash;render=$ui.worldRender;samples=$samples.ToArray();width=$bitmap.Width;height=$bitmap.Height}
    }finally{$bitmap.Dispose()}
}
function Processes-K6{
    @{at=[DateTime]::UtcNow.ToString('o');scope='Known build/app process names and cumulative CPU/memory only; no command lines or credentials; no host exclusivity'
      processes=@(Get-Process|Where-Object{$_.ProcessName -match '^(cargo|rustc|xtask|cl|link|lld|cmake|ninja|msbuild)$|^(photocraft|snapshot|engine_it)'}|ForEach-Object{@{name=$_.ProcessName;id=$_.Id;cpuSeconds=$_.CPU;workingSetBytes=$_.WorkingSet64}})}
}
function Measure-Frames-K6([string]$workload='1920x1080; two actual native clients, eight-member scene with terminal Animation media; 8 warmup then 64 measured rounds of real middle-drag +/-4px and wheel +/-4points'){
    $before=Idle-K6 1;$beforeScene=Scene-K6 1;$ui=Send-K6 1 'ui.inspect';$rect=$ui.roomControls.'world.drawing'
    Assert-K6 ($beforeScene.canonicalHash -is [string] -and $beforeScene.canonicalHash -match '^[0-9a-fA-F]{64}$') 'Before measurement requires a full non-null canonical hash'
    Assert-K6 (-not [string]::IsNullOrWhiteSpace($beforeScene.canvas) -and $beforeScene.canvas -ceq $before.active.info.canvas -and $beforeScene.revision -eq $before.active.canonical_revision) 'Before measurement scene must match the active canvas and accepted revision'
    Assert-K6 ([bool]$rect) 'Shared drawing viewport unavailable for frame measurement'
    Assert-K6 ([bool]$ui.perf.timings.gpuInfo.adapter -and [bool]$ui.perf.timings.gpuInfo.backend -and [bool]$ui.perf.timings.gpuInfo.deviceType) 'Native hardware adapter/backend/device metadata is missing'
    $x=($rect[0]+$rect[2])/2;$y=($rect[1]+$rect[3])/2;$samples=@{};$afterFrame=-1
    $processesBefore=Processes-K6;$script:k6Apps[1].Process.Refresh();$startWorkingSet=$script:k6Apps[1].Process.WorkingSet64
    if(-not $Diagnostic){Assert-K6 (@($processesBefore.processes|Where-Object {$_.name -match '^(cargo|rustc|xtask|cl|link|lld|cmake|ninja|msbuild)$|^engine_it'}).Count -eq 0) 'Native timing acceptance requires known build and test processes to finish first'}
    for($round=0;$round -lt 72;$round++){
        $direction=if($round%2 -eq 0){1}else{-1}
        foreach($input in @(@{kind='down';x=$x;y=$y;button='middle'},@{kind='move';x=$x+4*$direction;y=$y},@{kind='up';x=$x+4*$direction;y=$y;button='middle'},@{kind='wheel';x=$x;y=$y;deltaY=4*$direction;unit='point'})){Send-K6 1 'ui.worldInput' $input|Out-Null}
        $ui=Send-K6 1 'ui.inspect' @{worldHash=$false}
        Assert-K6 ($ui.worldScene.hashIncluded -eq $false -and $null -eq $ui.worldScene.canonicalHash) 'Measured inspection must explicitly omit the full-world hash; full hashes are checked outside collection'
        Assert-K6 ([bool]$ui.world.frameSamples) 'Shared frame instrumentation unavailable'
        if($round -lt 8){$afterFrame=($ui.world.frameSamples|Measure-Object frame -Maximum).Maximum}else{
            foreach($sample in $ui.world.frameSamples){if($sample.frame -gt $afterFrame){
                Assert-K6 ($null -ne $sample.cpu_ms -and -not [double]::IsNaN($sample.cpu_ms) -and -not [double]::IsInfinity($sample.cpu_ms) -and $sample.cpu_ms -ge 0) 'Invalid CPU frame sample'
                $samples["$($sample.frame)"]=$sample
            }}
        }
    }
    $raw=@($samples.Values|Sort-Object frame);$cpu=@($raw.cpu_ms|Sort-Object)
    $intervals=@($raw|Where-Object{$null -ne $_.interval_ms}|ForEach-Object{
        Assert-K6 (-not [double]::IsNaN($_.interval_ms) -and -not [double]::IsInfinity($_.interval_ms) -and $_.interval_ms -gt 0) 'Invalid frame interval';[double]$_.interval_ms
    }|Sort-Object)
    Assert-K6 ($raw.Count -ge 100 -and $intervals.Count -ge 100) 'Too few actual frame samples'
    $p95=$intervals[[Math]::Max(0,[Math]::Ceiling($intervals.Count*0.95)-1)]
    $script:k6Apps[1].Process.Refresh();$after=Idle-K6 1;$afterScene=Scene-K6 1
    Assert-K6 ($afterScene.canonicalHash -is [string] -and $afterScene.canonicalHash -match '^[0-9a-fA-F]{64}$') 'After measurement requires a full non-null canonical hash'
    Assert-K6 ($afterScene.canvas -ceq $beforeScene.canvas -and $after.active.info.canvas -ceq $before.active.info.canvas -and $afterScene.revision -eq $beforeScene.revision -and $afterScene.revision -eq $after.active.canonical_revision) 'Measurement switched the canvas or inspected a different accepted revision'
    Assert-K6 ($after.active.canonical_revision -eq $before.active.canonical_revision -and $afterScene.canonicalHash -ceq $beforeScene.canonicalHash) 'Navigation changed accepted artwork or its exact canonical hash'
    $script:k6Evidence.performance=@{
        scope='Consecutive native frame-start timestamps including presentation/scheduling/automation gaps; not input-to-display latency'
        cpuScope='Previous eframe CPU frame duration; excludes surface acquisition, queue.submit/present call duration and GPU execution'
        renderScope='worldRender.render_ms is UI-thread geometry/encoding/submission work; it excludes background preparation and is not GPU execution time'
        workload=$workload
        inspectionMethod='Candidate4+: ui.inspect worldHash:false only during measured/warmup collection, explicitly hashIncluded:false/canonicalHash:null. Full canonical hashes before and after remain required. Candidate3 raw full-hash timing failure is retained separately; no overhead subtraction or threshold change.'
        canonicalHashBefore=$beforeScene.canonicalHash;canonicalHashAfter=$afterScene.canonicalHash
        canonicalCanvasBefore=$beforeScene.canvas;canonicalCanvasAfter=$afterScene.canvas;canonicalRevisionBefore=$beforeScene.revision;canonicalRevisionAfter=$afterScene.revision
        samples=$raw;sampleCount=$raw.Count;intervalSampleCount=$intervals.Count;target_p95_ms=33.0;p95_ms=$p95;p50_ms=$intervals[[Math]::Max(0,[Math]::Ceiling($intervals.Count*0.5)-1)];max_ms=$intervals[-1]
        cpu_p50_ms=$cpu[[Math]::Max(0,[Math]::Ceiling($cpu.Count*0.5)-1)];cpu_p95_ms=$cpu[[Math]::Max(0,[Math]::Ceiling($cpu.Count*0.95)-1)];cpu_max_ms=$cpu[-1]
        render=$ui.worldRender;adapter=$ui.perf.timings.gpuInfo;window=$ui.window;scene=$before.active.info
        startWorkingSetBytes=$startWorkingSet;endWorkingSetBytes=$script:k6Apps[1].Process.WorkingSet64;peakWorkingSetBytes=$script:k6Apps[1].Process.PeakWorkingSet64
        processesBefore=$processesBefore;processesAfter=(Processes-K6);passed=($p95 -le 33.0)
    }
    if($p95 -gt 33.0){
        if(-not $Diagnostic){throw "Shared native frame interval p95 ${p95}ms exceeds33ms"}
        $script:k6Evidence.performanceLimitation="Diagnostic p95 ${p95}ms exceeds33ms; functional checks continue but performance acceptance remains pending"
    }
}
function Stress-Scene-K6([string]$canvas,[string]$expectedHash=''){
    $scene=Converged-K6 $canvas 180
    foreach($side in @($scene.first,$scene.second)){
        Assert-K6 ($side.strokeCount -eq 16 -and @($side.strokes).Count -eq 16 -and @($side.strokes|Where-Object {$_.samples -ne 8192 -or -not $_.visible}).Count -eq 0) 'Shared capacity did not retain all 16 strokes and 131072 original points'
        Assert-K6 (@($side.strokes.id|Sort-Object -Unique).Count -eq 16) 'Shared capacity stroke identities are not distinct'
        Assert-K6 (@($side.strokes.owner|Sort-Object -Unique).Count -eq 2) 'Both actual native authors must contribute to shared capacity'
    }
    if($expectedHash){Assert-K6 ($scene.hash -eq $expectedHash) 'Shared capacity canonical hash changed across recovery'}
    foreach($number in 1..2){
        $deadline=[DateTime]::UtcNow.AddSeconds(90)
        do{
            $ui=Send-K6 $number 'ui.inspect'
            if($ui.worldRender.stats.vertices -gt 0 -and $ui.worldRender.stats.layers -eq 2 -and $ui.worldRender.stats.pending_assets -eq 0 -and -not $ui.worldRender.preparing -and $ui.worldRender.rendered_revision -eq $scene.revision){break}
            Start-Sleep -Milliseconds 80
        }while([DateTime]::UtcNow -lt $deadline)
        Assert-K6 ($ui.worldRender.stats.vertices -gt 0 -and $ui.worldRender.stats.layers -eq 2 -and $ui.worldRender.rendered_revision -eq $scene.revision -and -not $ui.worldRender.preparing) "Client $number did not render the exact shared capacity revision in two author layers"
        Record-K6 "shared capacity renderer $number" @{render=$ui.worldRender;scene=$scene.hash;revision=$scene.revision;camera=(Status-K6 $number).active.viewer.camera}
    }
    $scene
}
function Stress-K6([string]$canvas){
    $script:k6Evidence.scope='Two actual native writers plus one headless read verifier; isolated loopback authority; K5 capacity curve geometry through shared commands; real egui pan and wheel; native canonical catch-up and restart'
    $script:k6Evidence.fixture=@{
        curves=16;samplesPerCurve=8192;totalRetainedSamples=131072;width=5.0
        source='scripts/komaphone/stroke-capacity-smoke.ps1'
        formula='For c=0..15 and i=0..8191: t=i/8191; x=round(-700+1400*t,6); y=round(180*sin(2*pi*t)+40*sin(6*pi*t)-30+4*c,6).'
        window=@(1920,1080);camera='root region and offset zero, zoom1';timingTargetP95Ms=33.0
    }
    foreach($number in 1..2){Cmd-K6 $number 'komaphone.world.home'|Out-Null;Idle-K6 $number|Out-Null}
    $seeding=[Diagnostics.Stopwatch]::StartNew()
    for($curve=0;$curve -lt 16;$curve++){
        $points=[System.Collections.Generic.List[object]]::new()
        for($sample=0;$sample -lt 8192;$sample++){
            $t=[double]$sample/8191.0
            $points.Add(@([Math]::Round(-700.0+1400.0*$t,6),[Math]::Round(180.0*[Math]::Sin(2.0*[Math]::PI*$t)+40.0*[Math]::Sin(6.0*[Math]::PI*$t)-30.0+4.0*$curve,6)))
        }
        $number=if($curve -eq 0){2}else{1}
        $readyClock=[Diagnostics.Stopwatch]::StartNew()
        Cmd-K6 $number 'komaphone.world.stroke' @{canvas=$canvas;anchor=@{depth=0;x='0';y='0'};points=$points.ToArray();width=5.0;color=@{rgba=@(0.05,0.09,0.13,1.0)};erase=$false}|Out-Null
        $accepted=Idle-K6 $number
        Assert-K6 ($accepted.active.stroke_count -eq $curve+1) 'A shared capacity seed stroke was lost'
        $readyUi=Rendered-K6 $number $accepted.active.canonical_revision;$readyClock.Stop()
        Record-K6 'shared capacity seed' @{curve=$curve;client=$number;revision=$accepted.active.canonical_revision;retainedPoints=8192*($curve+1);commandToRenderReadyMs=$readyClock.Elapsed.TotalMilliseconds;readinessScope='Control stroke request through exact accepted revision observed ready; includes local persistence, network, background preparation, UI scheduling and polling; not CPU-only preparation or input-to-photon';render=$readyUi.worldRender}
        if($curve -eq 0){Converged-K6 $canvas|Out-Null;Stop-App-K6 2}
    }
    $seeding.Stop();$script:k6Evidence.seedSeconds=$seeding.Elapsed.TotalSeconds
    $writerFinal=Scene-K6 1;Stop-App-K6 1;$writerStoppedAt=[DateTime]::UtcNow.ToString('o')
    Assert-K6 $script:k6Apps[1].Process.HasExited 'Capacity author A must exit before peer B comes online'
    $peerStartedAt=[DateTime]::UtcNow.ToString('o');$catchupClock=[Diagnostics.Stopwatch]::StartNew()
    Start-App-K6 2;Wait-K6 2 {param($s) $s.connected -and $s.account} 'capacity peer reconnect'|Out-Null
    Open-World-K6 2;Cmd-K6 2 'komaphone.world.open' @{canvas=$canvas}|Out-Null
    Wait-K6 2 {param($s) $s.active.stroke_count -eq 16 -and $s.active.pending -eq 0} 'native peer caught up all fifteen large stroke events' 180|Out-Null
    $peerFinal=Scene-K6 2;$peerReady=Rendered-K6 2 $writerFinal.revision;$catchupClock.Stop()
    Assert-K6 ($script:k6Apps[1].Process.HasExited -and $peerFinal.canonicalHash -eq $writerFinal.canonicalHash) 'Peer B failed to recover author A accepted work while A remained closed'
    Record-K6 'nonoverlapping native sessions recover all large edits' @{authorStoppedAt=$writerStoppedAt;peerStartedAt=$peerStartedAt;authorRemainedClosed=$true;acceptedHash=$writerFinal.canonicalHash;peerHash=$peerFinal.canonicalHash;revision=$peerFinal.revision;peerStartToRenderReadyMs=$catchupClock.Elapsed.TotalMilliseconds;scope='Peer process startup, cached identity, network catch-up, background preparation, UI scheduling and polling; not CPU preparation or input-to-photon';render=$peerReady.worldRender}
    Start-App-K6 1;Wait-K6 1 {param($s) $s.connected -and $s.account} 'capacity author returns after peer caught up'|Out-Null
    Open-World-K6 1;Cmd-K6 1 'komaphone.world.open' @{canvas=$canvas}|Out-Null;Idle-K6 1|Out-Null
    $before=Stress-Scene-K6 $canvas;Record-K6 'both native capacity canonical scenes' $before
    # A real authorized reader exercises the service's byte-bounded Events pages.
    $reader=Headless-K6 3;$invite=Invitation-K6 1 $reader.profile
    World-K6 $reader @{kind='accept';canvas=$canvas;id=$invite.id;secret=$invite.secret}|Out-Null;$invite=$null
    $after=0;$pages=[System.Collections.Generic.List[object]]::new();$seen=[System.Collections.Generic.HashSet[string]]::new();$sampleCount=0
    do{
        $page=World-K6 $reader @{kind='events';canvas=$canvas;after=$after;through=$before.revision;limit=128}
        Assert-K6 ($page.kind -eq 'events' -and $page.through -eq $before.revision -and $page.events.Count -gt 0) 'Invalid capacity Events page'
        foreach($event in $page.events){
            Assert-K6 ($event.revision -eq $after+1 -and $seen.Add($event.op.id)) 'Capacity Events pages skipped or repeated an operation'
            Assert-K6 ($event.op.action.kind -eq 'add_stroke' -and $event.op.action.stroke.points.Count -eq 8192) 'Capacity Events page dropped original stroke points'
            $sampleCount+=$event.op.action.stroke.points.Count;$after=$event.revision
        }
        $pages.Add(@{events=$page.events.Count;after=$after;through=$page.through;next=$page.next;reserializedJsonBytes=[Text.Encoding]::UTF8.GetByteCount((Json-K6 $page))})
        Assert-K6 ($null -eq $page.next -or $page.next -eq $after) 'Capacity Events continuation cursor is incorrect'
    }while($null -ne $page.next)
    Assert-K6 ($pages.Count -ge 2 -and $after -eq $before.revision -and $seen.Count -eq 16 -and $sampleCount -eq 131072) 'Shared capacity did not exercise multiple complete byte-bounded Events pages'
    Record-K6 'large event pages retain every original sample' @{pages=$pages.ToArray();uniqueOperations=$seen.Count;retainedSamples=$sampleCount;nativePeerMissedEvents=15}
    foreach($number in 1..2){Idle-K6 $number|Out-Null;Stop-App-K6 $number};Stop-Service-K6
    Start-Service-K6|Out-Null
    foreach($number in 1..2){Start-App-K6 $number;Wait-K6 $number {param($s) $s.connected -and $s.account} 'capacity identity restart'|Out-Null;Open-World-K6 $number;Cmd-K6 $number 'komaphone.world.open' @{canvas=$canvas}|Out-Null;Idle-K6 $number|Out-Null}
    Record-K6 'capacity hash survives both clients and authority restart' (Stress-Scene-K6 $canvas $before.hash)
    Screenshot-K6 1 'shared-capacity-before-timing'|Out-Null
    Measure-Frames-K6 '1920x1080;16 smooth curves,8192 original samples each (131072 total),two actual native author layers;K5 capacity geometry;8 warmup then64 measured real middle-drag +/-4px and wheel +/-4point rounds'
    Stress-Scene-K6 $canvas $before.hash|Out-Null
    $image=Export-K6 'png' 'shared-capacity.png' @{width=1280;height=720};Record-K6 'decoded shared capacity PNG' (Image-K6 $image 1280 720)
    if(-not $Diagnostic){Assert-K6 ($script:k6FirstMismatch.Count -eq 0) 'Unexpected service-response mismatches prevent native acceptance'}
    $script:k6Evidence.functionalPassed=$true;$script:k6Evidence.passed=[bool]$script:k6Evidence.performance.passed
}
function Navigation-K6([string]$canvas){
    Cmd-K6 1 'komaphone.world.home'|Out-Null;Idle-K6 1|Out-Null
    Cmd-K6 1 'komaphone.world.bookmark' @{name='Synthetic home'}|Out-Null;Idle-K6 1|Out-Null
    $far=@{canvas=$canvas;region=@{depth=0;x='12345';y='-23456'};offset=@('0','0');zoom=1.0}
    Cmd-K6 1 'komaphone.world.camera' @{camera=$far}|Out-Null;Idle-K6 1|Out-Null
    $ui=Send-K6 1 'ui.inspect';$rect=$ui.roomControls.'world.drawing';$x=($rect[0]+$rect[2])/2;$y=($rect[1]+$rect[3])/2
    foreach($input in @(@{kind='down';x=$x;y=$y;button='middle'},@{kind='move';x=$x-80;y=$y+45},@{kind='up';x=$x-80;y=$y+45;button='middle'})){Send-K6 1 'ui.worldInput' $input|Out-Null}
    $panned=Idle-K6 1;$far=$panned.active.viewer.camera
    Assert-K6 ($far.offset[0] -ne '0' -or $far.offset[1] -ne '0') 'Actual shared middle-drag did not change its exact lateral offsets'
    Cmd-K6 1 'komaphone.world.bookmark' @{name='Synthetic far lateral'}|Out-Null;Idle-K6 1|Out-Null
    $deep=@{canvas=$canvas;region=@{depth=6;x='12345000000000000';y='-23456000000000000'};offset=@('123456789','987654321');zoom=1.25}
    Cmd-K6 1 'komaphone.world.camera' @{camera=$deep}|Out-Null;Idle-K6 1|Out-Null
    Pointer-K6 1
    Cmd-K6 1 'komaphone.world.bookmark' @{name='Synthetic deep detail'}|Out-Null;$saved=Idle-K6 1
    $deep=$saved.active.viewer.camera;$bookmarks=$saved.active.viewer.bookmarks;$hash=(Scene-K6 1).canonicalHash
    Assert-K6 ($deep.region.depth -eq 6 -and [bigint]$deep.region.x -gt [bigint]9007199254740992) 'Deep fixture did not exercise a coordinate beyond exact IEEE754 integers'
    Cmd-K6 1 'komaphone.world.go' @{name='Synthetic home'}|Out-Null;Idle-K6 1|Out-Null
    Cmd-K6 1 'komaphone.world.go' @{name='Synthetic deep detail';animate=$true}|Out-Null
    Wait-K6 1 {param($s) $null -ne $s.flight} 'shared bookmark auto zoom started'|Out-Null
    Start-Sleep -Milliseconds 200;Cmd-K6 1 'komaphone.world.flight' @{state='pause'}|Out-Null;$paused=Idle-K6 1
    Assert-K6 ($paused.flight.paused -and $paused.flight.elapsed -gt 0 -and $paused.flight.elapsed -lt $paused.flight.duration) 'Bookmark pause missed the active flight'
    Start-Sleep -Milliseconds 200;$still=Status-K6 1
    Assert-K6 ($still.flight.elapsed -eq $paused.flight.elapsed -and (Json-K6 $still.active.viewer.camera) -ceq (Json-K6 $paused.active.viewer.camera)) 'Paused shared bookmark camera continued moving'
    Cmd-K6 1 'komaphone.world.flight' @{state='resume'}|Out-Null;Start-Sleep -Milliseconds 100
    Cmd-K6 1 'komaphone.world.flight' @{state='cancel'}|Out-Null;$cancelled=Idle-K6 1
    Assert-K6 ($null -eq $cancelled.flight) 'Shared bookmark flight did not cancel'
    Stop-App-K6 1;Start-App-K6 1;Wait-K6 1 {param($s) $s.account} 'navigation account reopened'|Out-Null;Open-World-K6 1
    Cmd-K6 1 'komaphone.world.open' @{canvas=$canvas}|Out-Null;$reopened=Idle-K6 1
    Assert-K6 ((Json-K6 $reopened.active.viewer.camera) -ceq (Json-K6 $cancelled.active.viewer.camera)) 'Acknowledged cancelled-flight camera changed after restart'
    Assert-K6 ((Json-K6 $reopened.active.viewer.bookmarks) -ceq (Json-K6 $bookmarks)) 'Exact lateral/deep bookmarks changed after restart'
    $navigationReadyClock=[Diagnostics.Stopwatch]::StartNew()
    Cmd-K6 1 'komaphone.world.go' @{name='Synthetic deep detail';animate=$true}|Out-Null
    $arrived=Wait-K6 1 {param($s) $null -eq $s.flight -and -not $s.busy -and (Json-K6 $s.active.viewer.camera) -ceq (Json-K6 $deep)} 'animated deep bookmark arrived exactly' 30
    Assert-K6 ((Scene-K6 1).canonicalHash -eq $hash) 'Bookmark navigation changed accepted drawing history'
    $rendered=Rendered-K6 1 $arrived.active.canonical_revision
    $navigationReadyClock.Stop();Record-K6 'deep bookmark command to rendered target observation' @{milliseconds=$navigationReadyClock.Elapsed.TotalMilliseconds;scope='Includes requested two-second animation, asynchronous local state, background preparation, UI scheduling and polling; not CPU preparation or input-to-photon';render=$rendered.worldRender}
    $deepScreenshot=Screenshot-K6 1 'deep-shared-bookmark';House-Pixels-K6 $deepScreenshot $rendered
    Cmd-K6 1 'komaphone.world.go' @{name='Synthetic far lateral'}|Out-Null;$lateral=Idle-K6 1
    Assert-K6 ((Json-K6 $lateral.active.viewer.camera) -ceq (Json-K6 $far)) 'Lateral bookmark lost exact wide offsets'
    Record-K6 'native deep lateral and animated bookmark recovery' @{far=$far;deep=$deep;pausedFlight=$paused.flight;pausedCamera=$paused.active.viewer.camera;cancelledCamera=$cancelled.active.viewer.camera;reopenedCamera=$reopened.active.viewer.camera;arrivedCamera=$arrived.active.viewer.camera;bookmarks=$bookmarks;canonicalHash=$hash}
    Cmd-K6 1 'komaphone.world.home'|Out-Null;Idle-K6 1|Out-Null
}
function Game-K6{
    foreach($n in 1..2){$ui=Send-K6 $n 'ui.inspect';if($ui.world.open){Click-K6 $n 'world.return'}}
    Cmd-K6 1 'komaphone.room.create' @{mode=@{kind='animation';rotations=1};timer_ms=$null}|Out-Null
    $created=Wait-K6 1 {param($s) $s.status.room -and -not $s.busy} 'synthetic Animation room' 60 'komaphone.room.status'
    Cmd-K6 1 'komaphone.room.invite' @{expires_in_ms=3600000;uses=1}|Out-Null
    Wait-K6 1 {param($s) $s.status.invitation_id -and -not $s.busy} 'party invitation' 60 'komaphone.room.status'|Out-Null
    $invite=Cmd-K6 1 'komaphone.room.invitation';Cmd-K6 2 'komaphone.room.join' @{room=$created.status.room.id;invitation=$invite.invitation}|Out-Null;$invite=$null
    Wait-K6 1 {param($s) $s.status.room.members.Count -eq 2 -and -not $s.busy} 'two native game members' 60 'komaphone.room.status'|Out-Null
    $readyRevision=(Cmd-K6 1 'komaphone.room.status').status.room.revision
    foreach($n in 1..2){
        Wait-K6 $n {param($s) $s.status.room.id -eq $created.status.room.id -and $s.status.room.revision -ge $readyRevision -and -not $s.busy} 'preceding lobby revision observed' 60 'komaphone.room.status'|Out-Null
        Cmd-K6 $n 'komaphone.room.ready' @{ready=$true}|Out-Null
        $ready=Wait-K6 $n {param($s) -not $s.busy -and $s.status.pending -eq 0 -and ($s.status.room.members|Where-Object {$_.participant.id -eq $s.status.profile.id}).ready} 'ready committed' 60 'komaphone.room.status'
        $readyRevision=$ready.status.room.revision
    }
    Wait-K6 1 {param($s) @($s.status.room.members|Where-Object{-not $_.ready}).Count -eq 0} 'both ready' 60 'komaphone.room.status'|Out-Null
    Cmd-K6 1 'komaphone.room.start'|Out-Null
    foreach($round in 0..1){foreach($n in 1..2){
        $turn=Wait-K6 $n {param($s) $s.status.room.phase -eq 'drawing' -and $s.status.room.round -eq $round -and -not $s.busy} 'native game assignment' 90 'komaphone.room.status'
        $project=$turn.status.room.assignment.assignment.project
        Cmd-K6 $n 'komaphone.room.openDraft'|Out-Null
        $saved=Wait-K6 $n {param($s) $s.active.project -eq $project -and $s.active.local_revision -ge 1 -and $s.active.pending -eq 0} 'native game draft' 60 'komaphone.project.status'
        Send-K6 $n 'ui.set' @{roomsOpen=$false;tool='brush';fit=$true}|Out-Null
        Cmd-K6 $n 'tools.setBrush' @{reset=$true;size=18;hardness=1;opacity=1}|Out-Null
        Cmd-K6 $n 'tools.setColors' @{foreground=$(if($n -eq 1){'#204080'}else{'#208040'})}|Out-Null
        $y=100+40*$round;$before=Cmd-K6 $n 'document.pixel' @{x=300;y=$y+30}
        Send-K6 $n 'ui.pointer' @{tool='brush';events=@(@{kind='down';x=100;y=$y},@{kind='move';x=300;y=$y+30},@{kind='up';x=500;y=$y})}|Out-Null
        Wait-K6 $n {param($s) $s.active.project -eq $project -and $s.active.local_revision -gt $saved.active.local_revision -and $s.active.pending -eq 0} 'painted game draft durably saved' 60 'komaphone.project.status'|Out-Null
        $after=Cmd-K6 $n 'document.pixel' @{x=300;y=$y+30}
        Assert-K6 ((Json-K6 $before) -ne (Json-K6 $after)) 'Native game drawing did not change the sampled pixel'
        Cmd-K6 $n 'komaphone.room.submit' @{note="Synthetic K6 frame $n/$round"}|Out-Null
        Wait-K6 $n {param($s) -not $s.busy -and ($s.status.room.assignment.submitted -or $s.status.room.round -gt $round -or $s.status.room.phase -eq 'reveal')} 'canonical native game submission' 120 'komaphone.room.status'|Out-Null
    }}
    $terminal=Wait-K6 1 {param($s) $s.status.room.phase -eq 'reveal' -and $s.status.room.reveal.Count -eq 4} 'complete original Animation' 120 'komaphone.room.status'
    $source=@{room=$terminal.status.room.id;game=$terminal.status.room.reveal[0].assignment.id.game;chain=0;fps=6;looping=$true}
    Record-K6 'actual terminal Animation original' @{source=$source;submissions=$terminal.status.room.reveal.Count;nativePlayers=2;rounds=2;pixelMutations=4}
    $source
}
function Gallery-Source-K6($source,[string]$canvas){
    Send-K6 1 'ui.set' @{roomsOpen=$false}|Out-Null
    Click-K6 1 'komaphone.gallery.download'
    Wait-K6 1 {param($s) -not $s.busy -and -not $s.downloading -and @($s.items|Where-Object {$_.id -eq $source.game -and -not $_.draft}).Count -eq 1} 'terminal original retained in Gallery' 180 'komaphone.gallery.status'|Out-Null
    $ui=Send-K6 1 'ui.inspect'
    if(-not $ui.gallery.open){$id=if($ui.roomControls.'workspace.gallery'){'workspace.gallery'}else{'home.gallery'};Click-K6 1 $id}
    Wait-K6 1 {param($s) -not $s.busy -and @($s.items|Where-Object {$_.id -eq $source.game -and -not $_.draft}).Count -eq 1} 'Gallery original row available' 60 'komaphone.gallery.status'|Out-Null
    Click-K6 1 ('gallery.item.'+$source.game+'.original')
    $gallery=Wait-K6 1 {param($s) -not $s.busy -and $s.active.id -eq $source.game -and $s.image} 'Gallery original preview' 90 'komaphone.gallery.status'
    Assert-K6 ($gallery.active.authors.Count -eq 2 -and $gallery.active.content.room.reveal.Count -eq 4) 'Gallery original lost native Animation frames or authors'
    $ui=Send-K6 1 'ui.inspect'
    Assert-K6 ($ui.gallery.chain -eq $source.chain -and $ui.gallery.fps -eq $source.fps -and $ui.gallery.looping -eq $source.looping) 'Gallery selection differs from the intended native Animation source'
    Screenshot-K6 1 'terminal-animation-gallery'|Out-Null
    Click-K6 1 'gallery.world'
    $deadline=[DateTime]::UtcNow.AddSeconds(20)
    do{$ui=Send-K6 1 'ui.inspect';if($ui.world.open -and -not $ui.gallery.open){break};Start-Sleep -Milliseconds 80}while([DateTime]::UtcNow -lt $deadline)
    Assert-K6 ($ui.world.open -and -not $ui.gallery.open) 'Gallery Place in a canvas did not open shared canvases'
    Click-K6 1 ('world.open.'+$canvas);Idle-K6 1|Out-Null
    $ui=Send-K6 1 'ui.inspect'
    if(-not $ui.roomControls.'komaphone.world.remix.prepare'){Click-K6 1 'world.section.media'}
    Click-K6 1 'komaphone.world.remix.prepare'
    $prepared=Wait-K6 1 {param($s) $s.prepared_media -and -not $s.busy} 'Gallery source authorized through real Prepare button' 120
    $selected=$prepared.prepared_media.provenance.source
    Assert-K6 ($selected.kind -eq 'gallery' -and $selected.room -eq $source.room -and $selected.game -eq $source.game -and $selected.chain -eq $source.chain -and $null -eq $selected.frame) 'Gallery-to-world route prepared a different original or selection'
    Record-K6 'actual Gallery to shared-canvas source route' @{entry=$gallery.active.id;canvas=$canvas;selection=$selected;frames=$prepared.prepared_media.frames;buttons=@('komaphone.gallery.download','gallery.item.original','gallery.world','world.open','komaphone.world.remix.prepare')}
    $prepared
}
if($FunctionsOnly){return}
if($Presence){. (Join-Path $PSScriptRoot 'k6-presence-smoke.ps1')}
try{
    Verify-Build-K6
    Preserve-Clipboard-K6
    $endpoint=Start-Service-K6
    if($Presence){$endpoint=Start-PresenceProxy-K6}
    foreach($n in 1..2){Start-App-K6 $n;Cmd-K6 $n 'komaphone.room.connect' @{endpoint=$endpoint;display_name="K6 native player $n"}|Out-Null;Wait-K6 $n {param($s) $s.status.paired -and $s.status.connected -and -not $s.busy} 'paired isolated transport' 60 'komaphone.room.status'|Out-Null;Wait-K6 $n {param($s) $s.connected -and $s.account} 'authenticated shared identity'|Out-Null;Open-World-K6 $n}
    $first=Status-K6 1;$second=Status-K6 2
    Click-K6 1 'komaphone.world.create';$created=Idle-K6 1;$canvas=$created.active.info.canvas
    Assert-K6 ([bool]$canvas) 'Canvas create did not publish an ID'
    $invite=Invitation-K6 1 $second.account.profile;Accept-K6 2 $invite|Out-Null;$invite=$null
    Record-K6 'two actual native members' @{canvas=$canvas;owner=$first.account.profile;member=$second.account.profile}
    if($Presence){Presence-K6 $canvas;return}
    if($Stress){Stress-K6 $canvas;Write-Output "K6 native capacity completed; passed=$($script:k6Evidence.passed)";return}
    if($Probe){
        $script:k6Evidence.scope='Minimal create/invite/accept/real-pointer/native-peer-command probe with two actual native clients; no full milestone or performance acceptance claim'
        Pointer-K6 1;Converged-K6 $canvas|Out-Null
        $probeStrokeId=Id-K6;Stroke-K6 2 $canvas 180 $probeStrokeId|Out-Null
        Wait-K6 2 {param($s) $s.active.pending -eq 0 -and -not $s.busy -and @((Scene-K6 2).strokes|Where-Object{$_.id -eq $probeStrokeId -and $_.visible}).Count -eq 1} 'specific probe peer stroke saved and accepted'|Out-Null
        $scene=Converged-K6 $canvas;Record-K6 'minimal native probe convergence' $scene
        foreach($side in @($scene.first,$scene.second)){Assert-K6 ($side.strokeCount -eq 2 -and @($side.strokes|Where-Object{$_.id -eq $probeStrokeId -and $_.visible -and $_.owner -eq $second.account.profile}).Count -eq 1) 'Probe requires the exact peer stroke on both native clients'}
        foreach($number in 1..2){$state=Status-K6 $number;Record-K6 "minimal native probe status $number" @{error=$state.error;state=$state.state;canvas=$state.active.info.canvas;canonicalRevision=$state.active.canonical_revision;pending=$state.active.pending;requestsPending=$state.requests_pending}}
        Screenshot-K6 1 'two-native-probe'|Out-Null
        Assert-K6 ($script:k6FirstMismatch.Count -eq 0) 'Minimal native probe observed an unexpected service-response mismatch; inspect payload-free app stderr diagnostics'
        foreach($number in 1..2){Assert-K6 (-not (Status-K6 $number).error) "Minimal native probe retained an unexpected client $number error"}
        Rendered-K6 1 $scene.revision|Out-Null
        $script:k6Evidence.functionalPassed=$true;$script:k6Evidence.passed=$true
        Write-Output 'Minimal K6 native response probe passed; full acceptance remains separate';return
    }
    Pointer-K6 1
    $pointerReady=Converged-K6 $canvas;Record-K6 'peer caught up before the separate undo fixture edit' $pointerReady
    $peerStrokeId=Id-K6;Stroke-K6 2 $canvas 180 $peerStrokeId|Out-Null
    Wait-K6 2 {param($s) $s.active.pending -eq 0 -and -not $s.busy -and @((Scene-K6 2).strokes|Where-Object{$_.id -eq $peerStrokeId -and $_.visible}).Count -eq 1} 'specific peer stroke durably saved and accepted before undo'|Out-Null
    $initial=Converged-K6 $canvas;Record-K6 'native convergence after pointer and command' $initial
    foreach($side in @($initial.first,$initial.second)){Assert-K6 (@($side.strokes|Where-Object{$_.id -eq $peerStrokeId -and $_.visible -and $_.owner -eq $second.account.profile}).Count -eq 1) 'Specific peer stroke is not present before owner undo'}
    Cmd-K6 1 'komaphone.world.undo'|Out-Null;Idle-K6 1|Out-Null;$undone=Converged-K6 $canvas
    Assert-K6 (@($undone.first.strokes|Where-Object{$_.owner -eq $second.account.profile -and $_.visible}).Count -eq 1) 'Author undo hid peer art'
    Assert-K6 (@($undone.first.strokes|Where-Object{$_.owner -eq $first.account.profile -and -not $_.visible}).Count -eq 1) 'Author undo failed to hide own stroke'
    Cmd-K6 1 'komaphone.world.redo'|Out-Null;Idle-K6 1|Out-Null;Record-K6 'author-scoped undo and redo' (Converged-K6 $canvas)
    $actors=@();foreach($n in 3..8){$actor=Headless-K6 $n;$invite=Invitation-K6 1 $actor.profile;$accepted=World-K6 $actor @{kind='accept';canvas=$canvas;id=$invite.id;secret=$invite.secret};$invite=$null;$actor.epoch=$accepted.info.epoch;$actors+=,$actor}
    for($i=0;$i -lt $actors.Count;$i++){
        $actor=$actors[$i];$stroke=@{id=(Id-K6);canvas=$canvas;owner=$actor.profile;revision=0;anchor=@{depth=0;x='0';y='0'};points=@(@(-100.0,(220.0+$i*15)),@(100.0,(220.0+$i*15)));width=5.0;color=@{rgba=@(0.6,0.2,0.3,1.0)};erase=$false}
        World-K6 $actor @{kind='append';operation=@{id=(Id-K6);canvas=$canvas;author=$actor.profile;device=$actor.device;sequence=1;authority_epoch=$actor.epoch;action=@{kind='add_stroke';stroke=$stroke}}}|Out-Null
    }
    $ui=Send-K6 1 'ui.inspect';if(-not $ui.roomControls.'world.people.refresh'){Click-K6 1 'world.section.people'}
    Click-K6 1 'world.people.refresh';Wait-K6 1 {param($s) $s.members.Count -eq 8 -and -not $s.busy} 'eight independently invited members through UI refresh'|Out-Null
    Wait-K6 2 {param($s) $s.active.stroke_count -eq 8} 'six headless edits reached native peer'|Out-Null
    Record-K6 'eight-member scene' (Converged-K6 $canvas)
    Screenshot-K6 1 'eight-members'|Out-Null
    $beforeOffline=Converged-K6 $canvas;Stop-Service-K6;$offline=@{}
    foreach($n in 1..2){
        Stroke-K6 $n $canvas (-210-10*$n)|Out-Null
        Wait-K6 $n {param($s) $s.active.pending -gt 0} "client $n durable offline queue"|Out-Null
        $scene=Scene-K6 $n;$added=@($scene.strokes|Where-Object createdRevision -gt $beforeOffline.revision)
        Assert-K6 ($added.Count -eq 1 -and $scene.canonicalHash -eq $beforeOffline.hash) 'Offline contribution changed accepted history before service acknowledgement'
        $offline[$n]=$added[0]
    }
    foreach($n in 1..2){Stop-App-K6 $n}
    foreach($n in 1..2){
        Start-App-K6 $n;Wait-K6 $n {param($s) $s.account} "client $n offline scoped identity restored"|Out-Null;Open-World-K6 $n
        Wait-K6 $n {param($s) @($s.items|Where-Object canvas -eq $canvas).Count -eq 1} "client $n cached gallery appeared while authority remains stopped"|Out-Null
        Thumbnail-K6 $n $canvas "offline-cached-gallery-$n"
        Click-K6 $n "world.open.$canvas"
        Wait-K6 $n {param($s) $s.active.info.canvas -eq $canvas -and $s.active.pending -gt 0} "client $n cached canvas and offline edit reopened"|Out-Null
        $scene=Scene-K6 $n
        Assert-K6 ($scene.canonicalHash -eq $beforeOffline.hash -and @($scene.strokes|Where-Object id -eq $offline[$n].id).Count -eq 1) 'Restart lost or replaced a durable offline contribution'
    }
    Start-Service-K6|Out-Null
    foreach($n in 1..2){Wait-K6 $n {param($s) $s.connected -and $s.active -and $s.active.pending -eq 0 -and $s.active.stroke_count -eq 10} "client $n offline edits synchronized after both client kills" 90|Out-Null}
    $reconnected=Converged-K6 $canvas
    foreach($n in 1..2){Assert-K6 (@($reconnected.first.strokes|Where-Object{$_.id -eq $offline[$n].id -and $_.visible}).Count -eq 1) 'An acknowledged offline contribution was missing after convergence'}
    Record-K6 'both native offline queues survived both client kills and service restart' @{before=$beforeOffline;offline=@($offline[1],$offline[2]);reconnected=$reconnected}
    # Fixed review interval; new peer edits must stay outside its seen boundary.
    Cmd-K6 1 'komaphone.world.review.begin'|Out-Null;Idle-K6 1|Out-Null;$review=(Status-K6 1).active.viewer.review.interval
    $midpoint=[long][Math]::Floor(($review.start_exclusive+$review.end_inclusive)/2)
    Cmd-K6 1 'komaphone.world.review.scrub' @{revision=$midpoint}|Out-Null;$midpointSaved=Idle-K6 1
    $savedReview=$midpointSaved.active.viewer.review;$reviewHash=(Scene-K6 1).canonicalHash
    $reviewPng=Export-K6 'png' 'review-midpoint-before.png' @{width=640;height=480}
    $reviewPixels=(Get-FileHash -LiteralPath $reviewPng -Algorithm SHA256).Hash
    Stop-App-K6 1;Start-App-K6 1;Wait-K6 1 {param($s) $s.account} 'review account reopened'|Out-Null;Open-World-K6 1
    Cmd-K6 1 'komaphone.world.open' @{canvas=$canvas}|Out-Null;$midpointReopened=Idle-K6 1
    Assert-K6 ((Json-K6 $midpointReopened.active.viewer.review) -ceq (Json-K6 $savedReview) -and (Scene-K6 1).canonicalHash -eq $reviewHash) 'Acknowledged catch-up midpoint or frozen interval changed after native client kill'
    $reopenedReviewPng=Export-K6 'png' 'review-midpoint-after-restart.png' @{width=640;height=480}
    Assert-K6 ((Get-FileHash -LiteralPath $reopenedReviewPng -Algorithm SHA256).Hash -eq $reviewPixels) 'Reopened review exported different pixels'
    Record-K6 'saved catch-up midpoint survived native restart' @{saved=$savedReview;reopened=$midpointReopened.active.viewer.review;canonicalHash=$reviewHash}
    Stroke-K6 2 $canvas -120|Out-Null;Idle-K6 2|Out-Null;Wait-K6 1 {param($s) $s.active.canonical_revision -gt $review.end_inclusive} 'live edit during fixed review'|Out-Null
    $arrivalReviewPng=Export-K6 'png' 'review-midpoint-after-arrival.png' @{width=640;height=480}
    Assert-K6 ((Get-FileHash -LiteralPath $arrivalReviewPng -Algorithm SHA256).Hash -eq $reviewPixels) 'PNG export included a newly arrived edit outside the fixed review cursor'
    Record-K6 'PNG follows the exact restored review cursor' @{midpoint=$midpoint;sha256=$reviewPixels;unchangedAfterRestart=$true;unchangedAfterNewVisiblePeerEdit=$true}
    Cmd-K6 1 'komaphone.world.review.markSeen'|Out-Null;Idle-K6 1|Out-Null;$marked=Status-K6 1
    Assert-K6 ($marked.active.viewer.review.reviewed_revision -eq $review.end_inclusive) 'Review marked a newer unseen edit as seen'
    Cmd-K6 1 'komaphone.world.review.control' @{state='cancel'}|Out-Null;Idle-K6 1|Out-Null
    Record-K6 'fixed catch-up interval preserved unseen arrivals' @{interval=$review;reviewed=$marked.active.viewer.review.reviewed_revision;available=$marked.active.canonical_revision}
    Navigation-K6 $canvas
    $camera=(Status-K6 1).active.viewer.camera;$zoomed=(Json-K6 $camera)|ConvertFrom-Json;$zoomed.zoom*=1.5;$tour=Id-K6
    Cmd-K6 1 'komaphone.world.tour.put' @{id=$tour;name='Synthetic native route';stops=@(@{camera=$camera;travel_ms=0;pause_ms=500;target=$null},@{camera=$zoomed;travel_ms=500;pause_ms=500;target=$null})}|Out-Null;Idle-K6 1|Out-Null
    Cmd-K6 1 'komaphone.world.tour.play' @{id=$tour}|Out-Null;Cmd-K6 1 'komaphone.world.tour.control' @{state='pause';elapsed_ms=1000}|Out-Null
    $during=(Status-K6 1).active.viewer.camera;Assert-K6 ($during.zoom -ne $camera.zoom) 'Saved tour did not move the camera'
    Cmd-K6 1 'komaphone.world.tour.control' @{state='cancel'}|Out-Null;Cmd-K6 1 'komaphone.world.home'|Out-Null
    $png=Export-K6 'png' 'shared.png' @{width=640;height=480};$pngDecoded=Image-K6 $png 640 480
    $gif=Export-K6 'gif' 'tour.gif' @{tour=$tour;width=320;height=240;fps=4};$gifDecoded=Image-K6 $gif 320 240
    $sequence=Export-K6 'sequence' 'tour-frames' @{tour=$tour;width=320;height=240;fps=4}
    $sequenceMeta=Get-Content -LiteralPath (Join-Path $sequence 'tour.json') -Raw|ConvertFrom-Json
    $frames=@(Get-ChildItem -LiteralPath $sequence -Filter 'frame-*.png' -File)
    Assert-K6 ($frames.Count -eq $sequenceMeta.frames -and $gifDecoded.frames -eq $frames.Count -and $frames.Count -gt 1) 'Tour GIF/sequence frame count mismatch'
    foreach($frame in $frames){Image-K6 $frame.FullName 320 240|Out-Null}
    $archive=Export-K6 'archive' 'shared.komworld'
    $manifest=Get-Content -LiteralPath (Join-Path $archive 'world.json') -Raw|ConvertFrom-Json
    Assert-K6 ($manifest.world.canvas -eq $canvas -and $manifest.world.revision -eq $sequenceMeta.world_revision) 'Editable archive lost the accepted canvas revision'
    Record-K6 'decoded exported files' @{png=$pngDecoded;gif=$gifDecoded;sequence=$sequenceMeta;archiveCanvas=$manifest.world.canvas;archiveRevision=$manifest.world.revision}
    $source=Game-K6
    $prepared=Gallery-Source-K6 $source $canvas
    Open-World-K6 2;Cmd-K6 2 'komaphone.world.open' @{canvas=$canvas}|Out-Null;Idle-K6 2|Out-Null
    Assert-K6 ($prepared.prepared_media.frames -eq 2 -and $prepared.prepared_media.provenance.authors.Count -eq 2) 'Prepared original lost frames or authors'
    $media=Id-K6;$placement=@{id=$media;anchor=@{depth=0;x='0';y='0'};offset=@(-200.0,-100.0);width=400.0;height=300.0}
    $mediaReadyClock=[Diagnostics.Stopwatch]::StartNew()
    Cmd-K6 1 'komaphone.world.remix.place' $placement|Out-Null;Idle-K6 1|Out-Null
    Wait-K6 2 {param($s) $s.active.media_count -eq 1} 'native peer received original media' 120|Out-Null
    $original=Converged-K6 $canvas;Record-K6 'native game media convergence' @{world=$original;prepared=$prepared.prepared_media}
    $deadline=[DateTime]::UtcNow.AddSeconds(90)
    do{$ui=Send-K6 1 'ui.inspect';if($ui.worldRender.stats.pending_assets -eq 0 -and $ui.worldRender.stats.uploaded_asset_bytes -gt 0){break};Start-Sleep -Milliseconds 100}while([DateTime]::UtcNow -lt $deadline)
    Record-K6 'accepted media GPU readiness' @{render=$ui.worldRender;world=$ui.world;status=(Status-K6 1)}
    if($ui.worldRender.stats.pending_assets -ne 0 -or $ui.worldRender.stats.uploaded_asset_bytes -le 0){Screenshot-K6 1 'media-render-timeout'|Out-Null}
    Assert-K6 ($ui.worldRender.stats.pending_assets -eq 0 -and $ui.worldRender.stats.uploaded_asset_bytes -gt 0) 'Accepted media did not reach the actual GPU renderer'
    $readyUi=Rendered-K6 1 $original.revision;$mediaReadyClock.Stop()
    Record-K6 'media placement to rendered accepted revision observation' @{milliseconds=$mediaReadyClock.Elapsed.TotalMilliseconds;scope='Includes local persistence, authority ACK, both-client convergence, asset fetch/decode/upload, background preparation, UI scheduling and polling; not CPU preparation or input-to-photon';render=$readyUi.worldRender}
    Screenshot-K6 1 'game-media-on-shared-canvas'|Out-Null
    Measure-Frames-K6
    $mediaArchive=Export-K6 'archive' 'remixed.komworld'
    $mediaManifest=Get-Content -LiteralPath (Join-Path $mediaArchive 'world.json') -Raw|ConvertFrom-Json
    Assert-K6 ($mediaManifest.world.canvas -eq $canvas -and $mediaManifest.assets.Count -ge 2) 'Editable media archive omitted canonical frames'
    foreach($asset in $mediaManifest.assets){
        $assetPath=Join-Path $mediaArchive ($asset.hash+'.png')
        Assert-K6 ((Get-FileHash -LiteralPath $assetPath -Algorithm SHA256).Hash.ToLowerInvariant() -eq $asset.hash) 'Archive asset failed its content hash'
        Image-K6 $assetPath $asset.width $asset.height|Out-Null
    }
    Record-K6 'editable media archive preserved verified originals' @{canvas=$canvas;revision=$mediaManifest.world.revision;assetCount=$mediaManifest.assets.Count}
    $restoreDenied=$null
    if($WorldProtocolVersion -eq 1){
        $restoreDenied=$false
        try{Send-K6 2 'app.worldFile' @{command='komaphone.world.restore';params=@{path=(Join-Path $script:k6Apps[2].Root 'nonexistent-synthetic-archive');canvas=(Id-K6)}}|Out-Null}catch{if($_.Exception.Message -match 'owner'){$restoreDenied=$true}else{throw}}
        Assert-K6 $restoreDenied 'Non-owner native restore was not rejected before filesystem work'
    }
    $restoredCanvas=Id-K6
    Send-K6 1 'app.worldFile' @{command='komaphone.world.restore';params=@{path=$mediaArchive;canvas=$restoredCanvas}}|Out-Null
    $restored=Wait-K6 1 {param($s) $s.export -and -not $s.export.busy -and @($s.items|Where-Object canvas -eq $restoredCanvas).Count -eq 1} 'owner-authorized archive restored as new private canvas' 120
    Assert-K6 (-not $restored.media_error) 'Native archive restore reported an error'
    Cmd-K6 1 'komaphone.world.open' @{canvas=$restoredCanvas}|Out-Null;Idle-K6 1|Out-Null
    $roundtripArchive=Export-K6 'archive' 'restored.komworld'
    $roundtrip=Get-Content -LiteralPath (Join-Path $roundtripArchive 'world.json') -Raw|ConvertFrom-Json
    if($WorldProtocolVersion -eq 1){
        $expected=Copied-World-K6 $mediaManifest.world $restoredCanvas
        Assert-K6 ((Json-K6 $roundtrip.world) -ceq (Json-K6 $expected)) 'Native archive restoration changed accepted history, authors, IDs or tour positions beyond its new canvas namespace'
    }else{
        Assert-K6 ([bool]$ImportVerifierPython) 'Protocol2 acceptance requires ImportVerifierPython for full retained-state comparison'
        & $ImportVerifierPython (Join-Path $PSScriptRoot 'verify-k7-import.py') $mediaArchive $roundtripArchive --output (Join-Path $k6Root 'import-comparison.json')
        Assert-K6 ($LASTEXITCODE -eq 0) 'Independent portable archive comparison failed'
    }
    if($WorldProtocolVersion -eq 1){Assert-K6 ((Json-K6 $roundtrip.assets) -ceq (Json-K6 $mediaManifest.assets)) 'Native archive restoration changed immutable asset descriptors'}
    Cmd-K6 1 'komaphone.world.members'|Out-Null
    Wait-K6 1 {param($s) $s.members.Count -eq 1 -and $s.members[0].owner -and $s.requests_pending -eq 0} 'restored copy has owner-only membership'|Out-Null
    $restoredRevision=(Status-K6 1).active.canonical_revision;Stroke-K6 1 $restoredCanvas -330|Out-Null;Idle-K6 1|Out-Null
    Assert-K6 ((Status-K6 1).active.canonical_revision -eq $restoredRevision+1) 'Fresh restored canvas could not allocate its next writer sequence'
    Cmd-K6 1 'komaphone.world.open' @{canvas=$canvas}|Out-Null;Idle-K6 1|Out-Null
    Assert-K6 ((Scene-K6 1).canonicalHash -eq $original.hash) 'Editing the restored copy mutated its source canvas'
    Record-K6 'native owner-only archive restore' @{source=$canvas;destination=$restoredCanvas;preservedRevision=$restoredRevision;nextEditAccepted=$true;sourceUnchanged=$true;nonOwnerDenied=$restoreDenied}
    # Revoke a known synthetic member. Existing pixels remain; authorization stops.
    $removed=$actors[0];Cmd-K6 1 'komaphone.world.remove' @{member=$removed.profile}|Out-Null;Idle-K6 1|Out-Null
    World-K6 $removed @{kind='info';canvas=$canvas} 403|Out-Null
    Record-K6 'revoked member cannot read' @{canvas=$canvas;member=$removed.profile;http=403}
    Cmd-K6 1 'komaphone.world.remix.copy' @{media=$media}|Out-Null
    $new=Cmd-K6 1 'komaphone.world.create' @{name='Independent media copy'};$destination=$new.canvas;Idle-K6 1|Out-Null
    $copy=Id-K6;$placement.id=$copy;Cmd-K6 1 'komaphone.world.remix.paste' $placement|Out-Null;Idle-K6 1|Out-Null
    $destinationState=Scene-K6 1
    Cmd-K6 2 'komaphone.world.list'|Out-Null
    Wait-K6 2 {param($s) -not $s.busy -and $s.requests_pending -eq 0} 'independent canvas directory'|Out-Null
    Assert-K6 (@((Status-K6 2).items|Where-Object canvas -eq $destination).Count -eq 0) 'A new independently owned canvas inherited the old member list'
    Cmd-K6 1 'komaphone.world.open' @{canvas=$canvas}|Out-Null;Idle-K6 1|Out-Null
    Assert-K6 ((Scene-K6 1).canonicalHash -eq $original.hash) 'Copying media mutated the original canvas'
    Cmd-K6 1 'komaphone.world.undo'|Out-Null;Idle-K6 1|Out-Null
    $hiddenSourceHash=(Scene-K6 1).canonicalHash
    Cmd-K6 1 'komaphone.world.open' @{canvas=$destination}|Out-Null;Idle-K6 1|Out-Null
    Assert-K6 ((Scene-K6 1).canonicalHash -eq $destinationState.canonicalHash) 'Hiding original media mutated its independent copy'
    $remixPng=Export-K6 'png' 'independent-remix.png' @{width=640;height=480};Image-K6 $remixPng 640 480|Out-Null
    Record-K6 'independent copied media and member lists' @{source=$canvas;destination=$destination;hash=$destinationState.canonicalHash;sourceUnchangedAtCopy=$true;copySurvivedSourceHide=$true}
    Screenshot-K6 1 'independent-remix-native'|Out-Null
    Click-K6 1 'world.tool.brush';$editUi=Send-K6 1 'ui.inspect';$editRect=$editUi.roomControls.'world.drawing';$editX=($editRect[0]+$editRect[2])/2;$editY=($editRect[1]+$editRect[3])/2
    Send-K6 1 'ui.worldInput' @{kind='down';x=$editX-150;y=$editY+20}|Out-Null
    Send-K6 1 'ui.worldInput' @{kind='move';x=$editX+150;y=$editY+20}|Out-Null
    Send-K6 1 'ui.worldInput' @{kind='up';x=$editX+150;y=$editY+20}|Out-Null
    Wait-K6 1 {param($s) $s.active.stroke_count -eq 1 -and $s.active.pending -eq 0} 'actual pointer overlay on copied media'|Out-Null
    $editedRemixPng=Export-K6 'png' 'independent-remix-edited.png' @{width=640;height=480};$editedPixels=Pixel-Difference-K6 $remixPng $editedRemixPng
    $editedDestination=Scene-K6 1
    Cmd-K6 1 'komaphone.world.open' @{canvas=$canvas}|Out-Null;Idle-K6 1|Out-Null;$afterHideSource=Scene-K6 1
    Assert-K6 ($afterHideSource.canonicalHash -eq $hiddenSourceHash) 'Editing independent media changed the frozen original canvas'
    Record-K6 'actual native overlay edited independent media copy' @{destination=$destination;destinationHash=$editedDestination.canonicalHash;source=$canvas;sourceHash=$afterHideSource.canonicalHash;pixelDifference=$editedPixels}
    # A revoked peer's durable offline edit must be retained for recovery, never uploaded under a later grant.
    Cmd-K6 1 'komaphone.world.open' @{canvas=$canvas}|Out-Null;Idle-K6 1|Out-Null
    Wait-K6 2 {param($s) $s.active.info.canvas -eq $canvas -and $s.active.pending -eq 0} 'peer ready before revocation'|Out-Null
    Stop-Service-K6;$revokedStrokeId=Id-K6;Stroke-K6 2 $canvas -290 $revokedStrokeId|Out-Null
    Wait-K6 2 {param($s) $s.active.pending -eq 1} 'peer offline edit saved'|Out-Null
    $offlineScene=Scene-K6 2
    Assert-K6 (@($offlineScene.strokes|Where-Object {$_.id -eq $revokedStrokeId -and $_.owner -eq $second.account.profile}).Count -eq 1) 'The exact revoked peer stroke was not retained locally'
    Stop-App-K6 2
    Start-Service-K6|Out-Null;Wait-K6 1 {param($s) $s.connected -and -not $s.busy} 'owner connected for revocation' 90|Out-Null
    Cmd-K6 1 'komaphone.world.remove' @{member=$second.account.profile}|Out-Null;Idle-K6 1|Out-Null;$beforeRevokedReplay=Scene-K6 1
    Start-App-K6 2;Wait-K6 2 {param($s) $s.connected -and $s.account} 'same peer identity after revoked restart' 90|Out-Null
    Open-World-K6 2;Cmd-K6 2 'komaphone.world.recovery'|Out-Null
    $script:k6RecoveryRefresh=[DateTime]::UtcNow
    $recovery=Wait-K6 2 {param($s)
        if(@($s.recovery|Where-Object{$_.canvas -eq $canvas -and $_.reason -eq 'AccessRemoved' -and $_.id -match '^[0-9a-fA-F]{32}$'}).Count -eq 1){return $true}
        # Recovery is a requested snapshot; refresh after the asynchronous authority denial.
        if($s.requests_pending -eq 0 -and ([DateTime]::UtcNow-$script:k6RecoveryRefresh).TotalMilliseconds -ge 500){Cmd-K6 2 'komaphone.world.recovery'|Out-Null;$script:k6RecoveryRefresh=[DateTime]::UtcNow}
        return $false
    } 'revoked offline edit retained' 90
    Assert-K6 ((Scene-K6 1).canonicalHash -eq $beforeRevokedReplay.canonicalHash) 'Revoked offline peer edit reached accepted history'
    $recoveredOpId=@($recovery.recovery|Where-Object {$_.canvas -eq $canvas -and $_.reason -eq 'AccessRemoved'})[0].id
    Record-K6 'revoked native queue retained without upload' @{recovery=$recovery.recovery;operationId=$recoveredOpId;strokeId=$revokedStrokeId;acceptedHash=$beforeRevokedReplay.canonicalHash}
    $invite=Invitation-K6 1 $second.account.profile;Accept-K6 2 $invite|Out-Null;$invite=$null
    Assert-K6 ((Scene-K6 1).canonicalHash -eq $beforeRevokedReplay.canonicalHash) 'Rejoining silently uploaded an old revoked-epoch edit'
    Record-K6 'new membership did not retag revoked edits' (Converged-K6 $canvas)
    Cmd-K6 2 'komaphone.world.recovery'|Out-Null
    Wait-K6 2 {param($s) $s.requests_pending -eq 0 -and @($s.recovery|Where-Object {$_.canvas -eq $canvas -and $_.id -eq $recoveredOpId -and $_.reason -eq 'AccessRemoved'}).Count -eq 1} 'same revoked operation retained after rejoining'|Out-Null
    Assert-K6 (@((Scene-K6 1).strokes|Where-Object id -eq $revokedStrokeId).Count -eq 0) 'Rejoining uploaded the exact revoked stroke'
    $otherCreated=Cmd-K6 2 'komaphone.world.create' @{name='Second owner private canvas'};$otherCanvas=$otherCreated.canvas
    Wait-K6 2 {param($s) $s.active.info.canvas -eq $otherCanvas -and $s.active.info.owner -eq $second.account.profile} 'second native member owns independent canvas'|Out-Null
    Stroke-K6 2 $otherCanvas 0|Out-Null;Idle-K6 2|Out-Null
    Cmd-K6 1 'komaphone.world.list'|Out-Null;Wait-K6 1 {param($s) $s.requests_pending -eq 0} 'first owner private directory'|Out-Null
    Assert-K6 (@((Status-K6 1).items|Where-Object canvas -eq $otherCanvas).Count -eq 0) 'One canvas owner gained access to another owner private canvas'
    Record-K6 'each native member can own independently shared canvases' @{canvas=$otherCanvas;owner=$second.account.profile;acceptedRevision=(Status-K6 2).active.canonical_revision;firstOwnerExcluded=$true}
    Stroke-Copy-Trash-K6 $otherCanvas $actors[1]
    if(-not $Diagnostic){Assert-K6 ($script:k6FirstMismatch.Count -eq 0) 'Unexpected service-response mismatches prevent native acceptance'}
    $script:k6Evidence.functionalPassed=$true;$script:k6Evidence.passed=[bool]$script:k6Evidence.performance.passed
    Write-Output "K6 native functional fixture completed; complete acceptance passed=$($script:k6Evidence.passed)"
}catch{
    $script:k6Evidence.error=$_.Exception.Message
    throw
}finally{
    $cleanupErrors=[System.Collections.Generic.List[string]]::new()
    if($script:k6Proxy){try{$script:k6Proxy.StopLoad();$script:k6Evidence.proxy=$script:k6Proxy.Snapshot()}catch{$cleanupErrors.Add('Presence fixture load did not stop')}}
    foreach($n in @($script:k6Apps.Keys)){try{Stop-App-K6 $n}catch{$cleanupErrors.Add($_.Exception.Message)}}
    if($script:k6Proxy){try{$script:k6Proxy.Dispose();$script:k6Proxy=$null}catch{$cleanupErrors.Add('Presence fixture proxy did not stop')}}
    try{Stop-Service-K6}catch{$cleanupErrors.Add($_.Exception.Message)}
    foreach($app in $script:k6Apps.Values){
        $database=Join-Path $app.DataRoot 'rooms.sqlite3'
        if(Test-Path -LiteralPath $database){try{& $CredentialCleanupExe '--discard-isolated-test-credentials' $database | Out-Null;if($LASTEXITCODE -ne 0){throw 'Isolated credential cleanup failed'}}catch{$cleanupErrors.Add($_.Exception.Message)}}
    }
    foreach($key in $script:k6Original.Keys){[Environment]::SetEnvironmentVariable($key,$script:k6Original[$key],'Process')}
    if($script:k6ClipboardCaptured -and $null -ne $script:k6OwnedClipboard){
        try{if((Get-Clipboard -Raw) -ceq $script:k6OwnedClipboard){[System.Windows.Forms.Clipboard]::SetDataObject($script:k6OriginalClipboard,$true)}}catch{$cleanupErrors.Add('Could not restore the unchanged owned clipboard value')}
    }
    $script:k6OriginalClipboard=$null;$script:k6OwnedClipboard=$null
    if($cleanupErrors.Count -gt 0){$script:k6Evidence.passed=$false}
    $script:k6Evidence.steps=$script:k6Steps.ToArray();$script:k6Evidence.inputs=$script:k6Inputs.ToArray();$script:k6Evidence.cleanupErrors=$cleanupErrors.ToArray();$script:k6Evidence.finished=[DateTime]::UtcNow.ToString('o')
    $script:k6Evidence|ConvertTo-Json -Depth 70|Set-Content -LiteralPath (Join-Path $k6Root 'result.json') -Encoding utf8
    Write-Output "K6 native evidence: $k6Root"
    if($cleanupErrors.Count -gt 0){throw 'Native fixture cleanup was incomplete; review the retained cleanupErrors before continuing'}
}
