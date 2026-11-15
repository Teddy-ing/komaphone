# Dot-sourced by k6-native-smoke.ps1. Uses its isolated clients, evidence and cleanup.
function Start-PresenceProxy-K6 {
    $source=Join-Path $PSScriptRoot 'k6-deploy/LatencyProxy.cs'
    Add-Type -Path $source
    Copy-Item -LiteralPath $source -Destination (Join-Path $k6Root 'LatencyProxy.cs')
    Copy-Item -LiteralPath $PSCommandPath -Destination (Join-Path $k6Root 'presence-harness.ps1')
    $script:k6Proxy=[K6LatencyProxy]::new($script:k6Port,50)
    $script:k6HttpEndpoint="http://127.0.0.1:$($script:k6Proxy.Port)"
    $calibration=@(for($i=0;$i -lt 5;$i++){
        $watch=[Diagnostics.Stopwatch]::StartNew();$response=Invoke-WebRequest -Uri ($script:k6HttpEndpoint+'/health') -TimeoutSec 10
        $watch.Stop();Assert-K6 ($response.StatusCode -eq 200) 'Delayed proxy health failed';$watch.Elapsed.TotalMilliseconds
    })
    Assert-K6 (@($calibration|Where-Object {$_ -lt 100}).Count -eq 0) 'Proxy failed to impose its declared 100ms round-trip delay'
    Record-K6 'simulated RTT proxy calibration' @{oneWayDelayMs=50;addedRttMs=100;observedHealthRequestMs=$calibration;credentials='Memory only; no request/header/body logs';proxySha256=(Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash;harnessSha256=(Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash}
    $script:k6HttpEndpoint
}
function Presence-Pixel-K6([string]$path,[double]$x,[double]$y,$window){
    Add-Type -AssemblyName System.Drawing
    $bitmap=[Drawing.Bitmap]::FromFile($path)
    try{
        $px=[int][Math]::Round($x*$bitmap.Width/$window.width);$py=[int][Math]::Round($y*$bitmap.Height/$window.height);$dark=0
        for($dy=-2;$dy -le 2;$dy++){for($dx=-2;$dx -le 2;$dx++){
            Assert-K6 ($px+$dx -ge 0 -and $px+$dx -lt $bitmap.Width -and $py+$dy -ge 0 -and $py+$dy -lt $bitmap.Height) 'Presence pixel target lies outside observer image'
            $pixel=$bitmap.GetPixel($px+$dx,$py+$dy)
            if($pixel.R -lt 100 -and $pixel.G -lt 100 -and $pixel.B -lt 100){$dark++}
        }}
        # Timestamp the completed pixel decode before unrelated file hashing.
        $decodedTicks=[Diagnostics.Stopwatch]::GetTimestamp()
        @{x=$px;y=$py;darkPixels=$dark;visible=($dark -ge 3);decodedTicks=$decodedTicks;sha256=(Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash}
    }finally{$bitmap.Dispose()}
}
function Arm-PresenceCapture-K6([string]$name){
    $app=$script:k6Apps[2]
    Assert-K6 (-not $script:k6PendingPresenceCapture) 'Only one observer screenshot request may be outstanding'
    if(-not $app.Client -or ([DateTime]::UtcNow-$app.LastUse).TotalSeconds -gt 20){Connect-K6 $app}
    $id=Id-K6;$path=Join-Path $app.Root "$name.png"
    $request=@{id=$id;method='ui.screenshot';params=@{path="$name.png";focus=$false;settle=$false}}
    $sentTicks=[Diagnostics.Stopwatch]::GetTimestamp()
    $app.Writer.WriteLine((Json-K6 $request))
    $script:k6PendingPresenceCapture=@{id=$id;path=$path;sentTicks=$sentTicks;writtenTicks=[Diagnostics.Stopwatch]::GetTimestamp()}
    $script:k6PendingPresenceCapture
}
function Read-PresenceCapture-K6([long]$deadlineTicks){
    $app=$script:k6Apps[2];$capture=$script:k6PendingPresenceCapture
    Assert-K6 ([bool]$capture) 'Observer screenshot was not armed'
    $remaining=($deadlineTicks-[Diagnostics.Stopwatch]::GetTimestamp())*1000.0/[Diagnostics.Stopwatch]::Frequency
    Assert-K6 ($remaining -gt 0) 'Presence pixel observation exceeded its four-second deadline'
    $stream=$app.Client.GetStream();$stream.ReadTimeout=[Math]::Max(1,[int][Math]::Ceiling($remaining))
    try{
        $line=$app.Reader.ReadLine();$responseTicks=[Diagnostics.Stopwatch]::GetTimestamp()
        Assert-K6 ([bool]$line) 'Observer screenshot connection closed'
        $reply=$line|ConvertFrom-Json;$parsedTicks=[Diagnostics.Stopwatch]::GetTimestamp()
        Assert-K6 ($reply.id -ceq $capture.id) 'Observer screenshot response ID differs from its sole outstanding request'
        Assert-K6 $reply.ok "Observer screenshot refused: $($reply.error)"
        $app.LastUse=[DateTime]::UtcNow;$capture.responseTicks=$responseTicks;$capture.parsedTicks=$parsedTicks;$capture.result=$reply.result
        $capture
    }catch{
        # A failed/expired reply cannot be safely reused for another request.
        $app.Client.Dispose();$app.Client=$null;throw
    }finally{
        if($app.Client){$stream.ReadTimeout=60000};$script:k6PendingPresenceCapture=$null
    }
}
function Arm-PresenceProbeWait-K6($token){
    $app=$script:k6Apps[2]
    Assert-K6 (-not $script:k6PendingPresenceCapture) 'Only one observer request may be outstanding'
    $id=Id-K6;$request=@{id=$id;method='ui.worldProbe';params=@{action='wait';token=$token}}
    $sentTicks=[Diagnostics.Stopwatch]::GetTimestamp();$app.Writer.WriteLine((Json-K6 $request))
    $script:k6PendingPresenceCapture=@{id=$id;token=$token;sentTicks=$sentTicks;writtenTicks=[Diagnostics.Stopwatch]::GetTimestamp()}
    $script:k6PendingPresenceCapture
}
function Probe-Pixels-K6($proof,[bool]$expectedVisible){
    Assert-K6 ($proof.pixels.Count -eq 25 -and $proof.pixel.Count -eq 2 -and $proof.imageSize.Count -eq 2) 'Probe did not return its exact5x5 actual image pixels'
    Assert-K6 ($proof.pixel[0] -ge 2 -and $proof.pixel[1] -ge 2 -and $proof.pixel[0]+2 -lt $proof.imageSize[0] -and $proof.pixel[1]+2 -lt $proof.imageSize[1]) 'Probe pixel patch lies outside its captured image'
    $dark=0
    foreach($rgba in $proof.pixels){
        Assert-K6 ($rgba.Count -eq 4) 'Probe pixel has no exact RGBA value'
        foreach($component in $rgba){Assert-K6 ($component -ge 0 -and $component -le 255 -and [Math]::Floor($component) -eq $component) 'Probe pixel component is not a byte'}
        if($rgba[0] -lt 100 -and $rgba[1] -lt 100 -and $rgba[2] -lt 100){$dark++}
    }
    Assert-K6 ($dark -eq $proof.darkPixels -and ($dark -ge 3) -eq $expectedVisible) 'Probe actual pixels differ from its blank/matching claim'
}
function Probe-Match-K6($armed,$matched,[string]$canvas,$sender,$recipient,$seen,[double]$ox,[double]$oy){
    Assert-K6 ($armed.armed -and $matched.matched -and $matched.token -eq $armed.token) 'Probe token or matching state changed'
    Probe-Pixels-K6 $armed.baseline $false;Probe-Pixels-K6 $matched $true
    Assert-K6 ($matched.frame -gt $armed.baseline.frame -and $matched.geometryEpoch -eq $armed.baseline.geometryEpoch) 'Probe reused an old frame or changed geometry'
    foreach($field in @('scope','pixel','imageSize')){Assert-K6 ((Json-K6 $matched.$field) -ceq (Json-K6 $armed.baseline.$field)) "Probe $field changed after arming"}
    $scope=$matched.scope;$preview=$matched.preview
    Assert-K6 ($scope.canvas -eq $canvas -and $scope.profile -eq $recipient.profile -and $scope.device -eq $recipient.device) 'Probe observed a different canvas/account/device'
    Assert-K6 ($preview.owner -eq $sender.profile -and $preview.device -eq $sender.device -and $preview.id -match '^[0-9a-fA-F]{32}$' -and -not $seen.Contains($preview.id) -and $preview.points.Count -ge 2) 'Rendered probe witness is not the exact new sender preview'
    Assert-K6 ($preview.color.rgba.Count -eq 4 -and $preview.color.rgba[3] -eq 1 -and @($preview.color.rgba[0..2]|Where-Object {$_ -lt 0 -or $_ -ge (100.0/255)}).Count -eq 0) 'Probe fixture preview is not the expected opaque dark ink'
    Assert-K6 (-not $preview.erase -and $scope.camera.zoom -eq 1 -and $scope.camera.region.depth -eq 0 -and $scope.camera.region.x -eq '0' -and $scope.camera.region.y -eq '0' -and @($scope.camera.offset|Where-Object {$_ -ne '0'}).Count -eq 0 -and $preview.anchor.depth -eq 0) 'Probe fixture left the fixed home camera'
    # At root depth each region is1024 local units. This fixture's only input
    # crosses the target midpoint; its ghost cursor is12px to the right.
    $first=$preview.points[0];$last=$preview.points[-1]
    $originX=[double]$preview.anchor.x*1024;$originY=[double]$preview.anchor.y*1024
    Assert-K6 ([Math]::Abs($originX+$first[0]-($ox-12)) -lt 0.01 -and [Math]::Abs($originX+$last[0]-($ox+12)) -lt 0.01 -and [Math]::Abs($originY+$first[1]-$oy) -lt 0.01 -and [Math]::Abs($originY+$last[1]-$oy) -lt 0.01) 'Rendered preview endpoints do not cross the measured target'
}
function Probe-Saved-Pixels-K6([string]$path,$proof){
    Add-Type -AssemblyName System.Drawing;$bitmap=[Drawing.Bitmap]::FromFile($path)
    try{
        Assert-K6 ($bitmap.Width -eq $proof.imageSize[0] -and $bitmap.Height -eq $proof.imageSize[1]) 'Retained probe PNG has different dimensions'
        $index=0
        for($dy=-2;$dy -le 2;$dy++){for($dx=-2;$dx -le 2;$dx++){
            $actual=$bitmap.GetPixel($proof.pixel[0]+$dx,$proof.pixel[1]+$dy);$expected=$proof.pixels[$index]
            Assert-K6 ($actual.R -eq $expected[0] -and $actual.G -eq $expected[1] -and $actual.B -eq $expected[2] -and $actual.A -eq $expected[3]) 'Saved probe PNG differs from the exact timed ColorImage pixels'
            $index++
        }}
    }finally{$bitmap.Dispose()}
}
function Presence-Stats-K6($rows){
    $values=@($rows.visibleMs|Sort-Object)
    @{samples=$values.Count;p50_ms=$values[[Math]::Max(0,[Math]::Ceiling($values.Count*0.5)-1)];p95_ms=$values[[Math]::Max(0,[Math]::Ceiling($values.Count*0.95)-1)];max_ms=$values[-1]}
}
function Presence-K6([string]$initialCanvas){
    $script:k6Evidence.scope='Real native raw stroke input to another native client screenshot showing the matching transient preview; four then eight members; every authority request traverses 50ms outbound and50ms inbound delay'
    $script:k6Evidence.presenceScope=if($PresencePngDiagnostic){'Diagnostic historical PNG loop: raw down/move submission through first decoded observer PNG pixels; repeated settle:false screenshots retain three UI passes and geometry safety. Includes network, scheduling, capture/PNG encoding/file/decode; no overhead subtracted.'}else{'Observed upper bound from raw down/move submission through receipt and parsing of a matching actual GPU pixel-readback reply on the same harness Stopwatch. Probe arm ACK requires an actual blank baseline. Exact matching ColorImage is saved as PNG and decoded after the timer, with exact25RGBA equality checked. No cross-process clock subtraction or estimated overhead subtraction; not input-to-photon.'}
    $script:k6Evidence.presenceObservationMethod=if($PresencePngDiagnostic){'prearmed-actual-pixels-v1'}else{'native-prearmed-world-pixels-v2'}
    $script:k6Evidence.presencePngDiagnostic=[bool]$PresencePngDiagnostic
    $script:k6Evidence.presenceReadiness=if($PresencePngDiagnostic){'Historical PNG-loop setup preserved: sender durable completion before next trial; observer may still be catching up'}else{'Untimed exact canonical hash/revision convergence of both native clients before each baseline; prior durable stroke cannot insert into the measured blank target during the pinned probe. All88 raw inputs, continuous headless5Hz publishers,100ms RTT and250ms four-user gate unchanged.'}
    $script:k6Evidence.presenceClock=@{kind='System.Diagnostics.Stopwatch.GetTimestamp';frequency=[Diagnostics.Stopwatch]::Frequency}
    $script:k6Evidence.presenceDiagnosticScope='Inspector and extra screenshot occur after first-pixel observation while the stroke remains held. Their elapsed times include the preceding pixel loop; they are post-visibility identity verification, not directly comparable to the old standalone inspector-then-capture latency.'
    $script:k6Evidence.presenceTargetP95Ms=250
    $script:k6Evidence.presenceAcceptanceRule='docs/komaphone/delivery-plan.md:156: four-user p95 <=250ms; eight-user group must sustain valid load and show every preview, with its unmodified p95 and backpressure reported as scaling evidence'
    $script:k6Evidence.presenceObservationBounds=@{deadlineSeconds=4;maximumSequentialCaptures=256;maximumOutstandingCaptures=1}
    $senderAccount=(Status-K6 1).account;$recipientAccount=(Status-K6 2).account
    $profile=$senderAccount.profile;$recipient=$recipientAccount.profile
    $actors=[System.Collections.Generic.List[object]]::new();$groups=[System.Collections.Generic.List[object]]::new()
    foreach($count in @(4,8)){
        $canvas=$initialCanvas
        if($count -eq 8){
            $created=Cmd-K6 1 'komaphone.world.create' @{name='Eight-member delayed presence'};$canvas=$created.canvas;Idle-K6 1|Out-Null
            $invite=Invitation-K6 1 $recipient;Accept-K6 2 $invite|Out-Null;$invite=$null
        }
        while($actors.Count -lt $count-2){$actors.Add((Headless-K6 ($actors.Count+3)))}
        foreach($actor in $actors){
            $invite=Invitation-K6 1 $actor.profile;$accepted=World-K6 $actor @{kind='accept';canvas=$canvas;id=$invite.id;secret=$invite.secret};$invite=$null;$actor.epoch=$accepted.info.epoch
        }
        foreach($number in 1..2){Cmd-K6 $number 'komaphone.world.home'|Out-Null;Idle-K6 $number|Out-Null}
        # The observer accepted a synthetic capability. Clear that textbox before any screenshot.
        $ui=Send-K6 2 'ui.inspect'
        if($ui.roomControls.'world.invitation'){
            Click-K6 2 'world.invitation';Send-K6 2 'ui.key' @{key='A';ctrl=$true}|Out-Null;Send-K6 2 'ui.key' @{key='Backspace'}|Out-Null
            Click-K6 2 'world.section.accept'
        }
        Click-K6 1 'world.tool.brush'
        $sender=Send-K6 1 'ui.inspect';$observer=Send-K6 2 'ui.inspect'
        $captureCalibration=@(for($sample=0;$sample -lt 5;$sample++){
            $captureClock=[Diagnostics.Stopwatch]::StartNew();Screenshot-K6 2 "presence-$count-capture-calibration-$sample" $false|Out-Null;$captureClock.Stop();$captureClock.Elapsed.TotalMilliseconds
        })
        Assert-K6 ([bool]$observer.perf.timings.gpuInfo.adapter -and [bool]$observer.perf.timings.gpuInfo.backend) 'Presence observer adapter metadata missing'
        $a=$sender.roomControls.'world.drawing';$b=$observer.roomControls.'world.drawing'
        Assert-K6 ($a -and $b -and $a[2]-$a[0] -gt 1200 -and $b[2]-$b[0] -gt 1200) 'Presence viewports are too small for distinct blank targets'
        $ax=($a[0]+$a[2])/2;$ay=($a[1]+$a[3])/2;$bx=($b[0]+$b[2])/2;$by=($b[1]+$b[3])/2
        $tokens=[System.Collections.Generic.List[string]]::new();$bodies=[System.Collections.Generic.List[string]]::new()
        for($i=0;$i -lt $actors.Count;$i++){
            $actor=$actors[$i];$tokens.Add($actor.token)
            $body=@{version=$WorldProtocolVersion;action=@{kind='presence';canvas=$canvas;epoch=$actor.epoch;camera=@{canvas=$canvas;region=@{depth=0;x='0';y='0'};offset=@('0','0');zoom=1.0};cursor=$null;preview=@(@(-710.0,(380.0+$i*8)),@(-680.0,(380.0+$i*8)));stroke=@{id=(Id-K6);anchor=@{depth=0;x='0';y='0'};width=3.0;color=@{rgba=@(0.6,0.6,0.6,1.0)};erase=$false}}}
            $preflight=World-K6 $actor $body.action
            Assert-K6 ($preflight.kind -eq 'peers') 'Synthetic preview load preflight was not accepted'
            $bodies.Add((Json-K6 $body))
        }
        $script:k6Proxy.StartLoad($tokens.ToArray(),$bodies.ToArray(),200);$tokens.Clear();$bodies.Clear()
        Wait-K6 2 {param($s) $s.peers.Count -eq $count -and @($s.peers|Where-Object {$_.profile -ne $profile -and $_.profile -ne $recipient -and $_.stroke -and $_.preview.Count -eq 2}).Count -eq $actors.Count} 'every synthetic preview publisher visible in observer peer data' 30|Out-Null
        $rows=[System.Collections.Generic.List[object]]::new();$seen=[System.Collections.Generic.HashSet[string]]::new();$processesBefore=Processes-K6;$groupClock=[Diagnostics.Stopwatch]::StartNew()
        $peakPendingSender=0;$peakPendingObserver=0;$peakPresenceSender=0;$peakPresenceObserver=0;$peakTotalSender=0;$peakTotalObserver=0;$minPeers=[int]::MaxValue
        $requestScope=if((Status-K6 1).PSObject.Properties.Name -contains 'presence_requests_pending'){'requests_pending counts durable/control; presence_requests_pending is separate'}else{'Legacy candidate requests_pending includes every request kind; separate presence counter unavailable'}
        try{
            for($trial=0;$trial -lt 44;$trial++){
                $ox=-500+140*($trial%8);$oy=-250+90*[Math]::Floor($trial/8)
                $readiness=$null
                if(-not $PresencePngDiagnostic){
                    $ready=Converged-K6 $canvas 30
                    Assert-K6 ($ready.first.revision -eq $ready.second.revision -and $ready.hash -match '^[0-9a-fA-F]{64}$') 'Probe baseline requires exact both-client accepted revision and hash'
                    $readiness=@{canvas=$ready.canvas;revision=$ready.revision;canonicalHash=$ready.hash;beforeInputClock=$true}
                }
                $baselinePath=Screenshot-K6 2 "presence-$count-$trial-before" $false;$baseline=Presence-Pixel-K6 $baselinePath ($bx+$ox) ($by+$oy) $observer.window
                Assert-K6 (-not $baseline.visible) 'Presence target was not blank before the raw input'
                foreach($peer in (Status-K6 2).peers){if($peer.profile -eq $profile -and $peer.stroke){$seen.Add($peer.stroke.id)|Out-Null}}
                $receivedMs=$null;$captureMs=$null;$shown=$false;$peerId=$null;$previewPoints=0;$visiblePath=$null;$pixel=$null
                $visibleMs=$null;$measurementEndTicks=$null;$diagnosticVisibleMs=$null;$diagnosticPath=$null;$diagnosticPixel=$null
                $probeArmed=$null;$probeMatch=$null;$probeSaved=$false;$probeReceipt=$null
                $downInput=@{kind='down';x=$ax+$ox-12;y=$ay+$oy};$moveInput=@{kind='move';x=$ax+$ox+12;y=$ay+$oy}
                $captures=[System.Collections.Generic.List[object]]::new();$captureIndex=0
                if($PresencePngDiagnostic){$armed=Arm-PresenceCapture-K6 "presence-$count-$trial-observe-$captureIndex"}else{
                    $probeArmed=Send-K6 2 'ui.worldProbe' @{action='arm';canvas=$canvas;sender=$profile;x=$bx+$ox;y=$by+$oy;excludedStrokes=@($seen|Sort-Object)}
                    Assert-K6 ($probeArmed.armed -and $null -ne $probeArmed.token) 'Probe arm did not confirm its actual blank baseline'
                    Probe-Pixels-K6 $probeArmed.baseline $false
                    Assert-K6 ($probeArmed.baseline.scope.canvas -eq $canvas -and $probeArmed.baseline.scope.canonicalRevision -eq $readiness.revision -and $probeArmed.baseline.scope.displayRevision -eq $readiness.revision) 'Probe baseline differs from the converged accepted scene'
                    Assert-K6 ($probeArmed.baseline.pixel[0] -eq $baseline.x -and $probeArmed.baseline.pixel[1] -eq $baseline.y) 'Probe baseline sampled a different target than the actual baseline PNG'
                    $armed=Arm-PresenceProbeWait-K6 $probeArmed.token
                }
                $inputTicks=[Diagnostics.Stopwatch]::GetTimestamp();$deadlineTicks=$inputTicks+4*[Diagnostics.Stopwatch]::Frequency
                Assert-K6 ($armed.writtenTicks -le $inputTicks) 'Observer request was not prearmed before raw input'
                $script:k6Evidence.presenceCurrentTrial=@{members=$count;trial=$trial;canvas=$canvas;senderProfile=$profile;inputTicks=$inputTicks;deadlineTicks=$deadlineTicks;input=@($downInput,$moveInput);readiness=$readiness;baseline=$baseline;baselinePath=$baselinePath;phase='prearmed';armed=$armed;probeArmed=$probeArmed;captures=@()}
                try{
                    $downReply=Send-K6 1 'ui.worldInput' $downInput
                    $downCompletedTicks=[Diagnostics.Stopwatch]::GetTimestamp()
                    $moveReply=Send-K6 1 'ui.worldInput' $moveInput
                    $inputCompletedTicks=[Diagnostics.Stopwatch]::GetTimestamp()
                    if($PresencePngDiagnostic){do{
                        $captured=Read-PresenceCapture-K6 $deadlineTicks
                        $pixel=Presence-Pixel-K6 $captured.path ($bx+$ox) ($by+$oy) $observer.window
                        $captures.Add(@{requestId=$captured.id;path=$captured.path;sentTicks=$captured.sentTicks;writtenTicks=$captured.writtenTicks;responseTicks=$captured.responseTicks;decodedTicks=$pixel.decodedTicks;darkPixels=$pixel.darkPixels;visible=$pixel.visible;sha256=$pixel.sha256})
                        $script:k6Evidence.presenceCurrentTrial.phase='observing';$script:k6Evidence.presenceCurrentTrial.captures=$captures.ToArray()
                        if($pixel.visible -and $pixel.decodedTicks -le $deadlineTicks){
                            $shown=$true;$visiblePath=$captured.path
                            $measurementEndTicks=$pixel.decodedTicks;$visibleMs=($measurementEndTicks-$inputTicks)*1000.0/[Diagnostics.Stopwatch]::Frequency
                            break
                        }
                        if([Diagnostics.Stopwatch]::GetTimestamp() -ge $deadlineTicks){break}
                        Assert-K6 ($captures.Count -lt 256) 'Presence observation exceeded its 256-capture hard bound'
                        $captureIndex++;$armed=Arm-PresenceCapture-K6 "presence-$count-$trial-observe-$captureIndex"
                    }while($true)}else{
                        $probeReceipt=Read-PresenceCapture-K6 $deadlineTicks
                        $measurementEndTicks=$probeReceipt.parsedTicks
                        Assert-K6 ($measurementEndTicks -le $deadlineTicks) 'Probe matching reply exceeded the four-second deadline'
                        $probeMatch=$probeReceipt.result
                        $script:k6Evidence.presenceCurrentTrial.probeReceipt=$probeReceipt
                        Probe-Match-K6 $probeArmed $probeMatch $canvas $senderAccount $recipientAccount $seen $ox $oy
                        $shown=$true;$visibleMs=($measurementEndTicks-$inputTicks)*1000.0/[Diagnostics.Stopwatch]::Frequency
                        $name="presence-$count-$trial-native-visible.png"
                        Send-K6 2 'ui.worldProbe' @{action='save';token=$probeArmed.token;path=$name}|Out-Null;$probeSaved=$true
                        $visiblePath=Join-Path $script:k6Apps[2].Root $name
                        Probe-Saved-Pixels-K6 $visiblePath $probeMatch
                        $pixel=Presence-Pixel-K6 $visiblePath ($bx+$ox) ($by+$oy) $observer.window
                        Assert-K6 $pixel.visible 'Retained matched image did not contain the exact visible target'
                    }
                    # Keep the stroke held until a separate identity check proves the
                    # observed pixels belong to this new author's actual preview.
                    $status=Status-K6 2
                    $receivedMs=([Diagnostics.Stopwatch]::GetTimestamp()-$inputTicks)*1000.0/[Diagnostics.Stopwatch]::Frequency
                    $peakPendingObserver=[Math]::Max($peakPendingObserver,[int]$status.requests_pending)
                    $peakPresenceObserver=[Math]::Max($peakPresenceObserver,[int]$status.presence_requests_pending)
                    $peakTotalObserver=[Math]::Max($peakTotalObserver,([int]$status.requests_pending+[int]$status.presence_requests_pending))
                    $minPeers=[Math]::Min($minPeers,@($status.peers).Count)
                    $peer=@($status.peers|Where-Object {$_.profile -eq $profile -and $_.stroke -and $_.preview.Count -ge 2 -and -not $seen.Contains($_.stroke.id)})
                    if($shown){
                        Assert-K6 ($peer.Count -eq 1) 'Visible pixels lack exactly one new preview from the expected sender'
                        Assert-K6 ($peer[0].camera.canvas -eq $canvas -and $peer[0].stroke.id -match '^[0-9a-fA-F]{32}$') 'Visible preview identity has a different canvas or invalid stroke ID'
                        if($probeMatch){Assert-K6 ($peer[0].stroke.id -eq $probeMatch.preview.id -and $peer[0].device -eq $probeMatch.preview.device) 'Post-timing peer identity differs from the issued rendered-frame witness'}
                        $peerId=$peer[0].stroke.id;$previewPoints=$peer[0].preview.Count
                        $captureStart=[Diagnostics.Stopwatch]::GetTimestamp()
                        $diagnosticPath=Screenshot-K6 2 "presence-$count-$trial-post-visibility-verified" $false
                        $diagnosticPixel=Presence-Pixel-K6 $diagnosticPath ($bx+$ox) ($by+$oy) $observer.window
                        $captureMs=($diagnosticPixel.decodedTicks-$captureStart)*1000.0/[Diagnostics.Stopwatch]::Frequency
                        $diagnosticVisibleMs=($diagnosticPixel.decodedTicks-$inputTicks)*1000.0/[Diagnostics.Stopwatch]::Frequency
                        Assert-K6 $diagnosticPixel.visible 'Verified held preview disappeared from its actual pixel target'
                    }
                }finally{
                    Send-K6 1 'ui.worldInput' @{kind='up';x=$ax+$ox+12;y=$ay+$oy}|Out-Null
                    if($probeArmed -and -not $probeSaved -and -not $script:k6PendingPresenceCapture){
                        try{Send-K6 2 'ui.worldProbe' @{action='cancel';token=$probeArmed.token}|Out-Null}catch{$script:k6Evidence.presenceCurrentTrial.cancelError=$_.Exception.Message}
                    }
                }
                $sourceStatus=Status-K6 1
                $peakPendingSender=[Math]::Max($peakPendingSender,[int]$sourceStatus.requests_pending)
                $peakPresenceSender=[Math]::Max($peakPresenceSender,[int]$sourceStatus.presence_requests_pending)
                $peakTotalSender=[Math]::Max($peakTotalSender,([int]$sourceStatus.requests_pending+[int]$sourceStatus.presence_requests_pending))
                if($peerId){$seen.Add($peerId)|Out-Null}
                $row=@{trial=$trial;warmup=($trial -lt 4);visible=$shown;visibleMs=$visibleMs;measurementEndTicks=$measurementEndTicks;inputTicks=$inputTicks;downCompletedTicks=$downCompletedTicks;inputCompletedTicks=$inputCompletedTicks;input=@($downInput,$moveInput);inputReplies=@($downReply,$moveReply);readiness=$readiness;deadlineTicks=$deadlineTicks;captures=$captures.ToArray();probeArmed=$probeArmed;probeMatch=$probeMatch;probeReceipt=$probeReceipt;postVisibilityInspectorMs=$receivedMs;postVisibilityCaptureAndDecodeMs=$captureMs;postVisibilityVerifiedMs=$diagnosticVisibleMs;postVisibilityVerifiedPath=$diagnosticPath;postVisibilityPixel=$diagnosticPixel;previewPoints=$previewPoints;stroke=$peerId;senderProfile=$profile;canvas=$canvas;target=@{x=$bx+$ox;y=$by+$oy};baseline=$baseline;baselinePath=$baselinePath;pixel=$pixel;visiblePath=$visiblePath}
                $script:k6Evidence.presenceCurrentTrial.phase='verified';$script:k6Evidence.presenceCurrentTrial.result=$row
                $rows.Add($row)
                Wait-K6 1 {param($s) $s.active -and -not $s.busy -and $s.active.pending -eq 0 -and $s.active.canonical_revision -eq $s.active.display_revision} 'preview trial durably committed after visibility clock stopped'|Out-Null
                if($trial%10 -eq 0){Write-Host "K6 presence: group $count trial $trial visible=$shown observed_ms=$($row.visibleMs)"}
            }
        }finally{$groupClock.Stop();$load=$script:k6Proxy.Snapshot();$script:k6Proxy.StopLoad()}
        $workloadValid=$load.LoadSent -gt 0 -and $load.LoadErrors -eq 0 -and $minPeers -eq $count
        $measured=@($rows|Where-Object {-not $_.warmup});$complete=$rows.Count -eq 44 -and $measured.Count -eq 40 -and @($rows|Where-Object {-not $_.visible}).Count -eq 0
        $timing=if($complete){Presence-Stats-K6 $measured}else{$null}
        $timingRequired=$count -eq 4;$meets250ms=$complete -and $timing.p95_ms -le 250
        $group=@{members=$count;nativeClients=2;headlessPreviewSenders=$count-2;periodMs=200;warmup=4;measuredSamples=$measured.Count;timing=$timing;timingRequired=$timingRequired;meets250ms=$meets250ms;allVisible=$complete;passed=($complete -and (-not $timingRequired -or $meets250ms));rows=$rows.ToArray();load=$load;durationSeconds=$groupClock.Elapsed.TotalSeconds;minimumObservedPeers=$minPeers;requestMetricScope=$requestScope;peakSenderPendingRequests=$peakPendingSender;peakObserverPendingRequests=$peakPendingObserver;peakSenderPresenceRequests=$peakPresenceSender;peakObserverPresenceRequests=$peakPresenceObserver;peakSenderTotalRequests=$peakTotalSender;peakObserverTotalRequests=$peakTotalObserver;senderDroppedPreviews=$sourceStatus.dropped_previews;observerDroppedPreviews=$status.dropped_previews;observerAdapter=$observer.perf.timings.gpuInfo;window=$observer.window;processesBefore=$processesBefore;processesAfter=(Processes-K6)}
        $group.captureCalibrationMs=$captureCalibration;$group.screenshotSettle=$false;$group.requestMetricsSampledAfterVisibility=$true
        $group.workloadValid=$workloadValid;$group.passed=$group.passed -and $workloadValid
        $groups.Add($group);Record-K6 "visible preview group $count" $group
        $script:k6Evidence.presence=$groups.ToArray()
        Assert-K6 $workloadValid 'Presence benchmark did not sustain all authorized concurrent preview publishers; measured evidence retained'
    }
    $script:k6Evidence.presence=$groups.ToArray();$script:k6Evidence.functionalPassed=@($groups|Where-Object {-not $_.allVisible}).Count -eq 0
    $script:k6Evidence.passed=@($groups|Where-Object {-not $_.passed}).Count -eq 0 -and $script:k6FirstMismatch.Count -eq 0
    Write-Output "K6 visible presence fixture completed; unchanged250ms p95 target passed=$($script:k6Evidence.passed)"
}
