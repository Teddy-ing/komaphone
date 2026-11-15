# Reproducible isolated packaging-policy tests; retain tiny fixtures for inspection.
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot '../package-k4-lan.ps1') -LibraryOnly
$testRoot = Join-Path $repoRoot ('log/k4/lan-package/policy-tests-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($testRoot) | Out-Null
$root = Join-Path $testRoot 'candidate'
$rows = @()
$hashes = @{}
foreach ($relative in $candidateFiles | Where-Object { $_ -ne 'SHA256SUMS' }) {
    $path = Join-Path $root $relative
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
    [IO.File]::WriteAllText($path, $relative, [Text.UTF8Encoding]::new($false))
    $hashes[$relative] = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
    $rows += "$($hashes[$relative])  $relative"
}
$manifest = Join-Path $root 'SHA256SUMS'
$manifestText = ($rows -join "`n") + "`n"
[IO.File]::WriteAllText($manifest, $manifestText, [Text.UTF8Encoding]::new($false))
$manifestHash = (Get-FileHash -LiteralPath $manifest -Algorithm SHA256).Hash
$results = [Collections.Generic.List[object]]::new()
function Assert-Test([bool] $Condition, [string] $Message) { if (-not $Condition) { throw $Message } }
function Assert-Refused([scriptblock] $Action) {
    $refused = $false
    try { & $Action | Out-Null } catch { $refused = $true }
    Assert-Test $refused 'Expected refusal, but operation succeeded.'
}
function Test-Case([string] $Name, [scriptblock] $Action) {
    try { & $Action; $results.Add(@{name=$Name;passed=$true}) }
    catch { $results.Add(@{name=$Name;passed=$false;error=$_.Exception.Message}) }
}
Test-Case 'exact candidate allowlist and hash pins' {
    $value = Get-LanCandidate $root $manifestHash $hashes['photocraft.exe'] $hashes['photocraft-sync.exe']
    Assert-Test ($value.hashes.Count -eq 22) 'Unexpected file count.'
    Assert-Refused { Get-LanCandidate $root ('A' * 64) $hashes['photocraft.exe'] $hashes['photocraft-sync.exe'] }
    Assert-Refused { Get-LanCandidate $root $manifestHash ('A' * 64) $hashes['photocraft-sync.exe'] }
}
Test-Case 'private canonical HTTPS only' {
    foreach ($endpoint in @('https://10.0.0.40:43127','https://172.16.1.2:443','https://192.168.1.2:443')) { Get-LanEndpoint $endpoint | Out-Null }
    foreach ($endpoint in @('http://10.0.0.40:43127','https://8.8.8.8:43127','https://127.0.0.1:43127','https://10.0.0.40:043127','https://010.0.0.40:43127','https://10.0.0.40:0','https://user@10.0.0.40:43127','https://10.0.0.40:43127/','https://10.0.0.40:43127?token=x','https://hostname:43127')) {
        Assert-Refused { Get-LanEndpoint $endpoint }
    }
}
Test-Case 'public certificate pins SAN validity and PEM-only transfer' {
    $rsa = [Security.Cryptography.RSA]::Create(2048)
    try {
        $request = [Security.Cryptography.X509Certificates.CertificateRequest]::new('CN=Komaphone packaging fixture', $rsa, [Security.Cryptography.HashAlgorithmName]::SHA256, [Security.Cryptography.RSASignaturePadding]::Pkcs1)
        $san = [Security.Cryptography.X509Certificates.SubjectAlternativeNameBuilder]::new()
        $san.AddIpAddress([Net.IPAddress]::Parse('10.0.0.40'))
        $request.CertificateExtensions.Add($san.Build())
        $path = Join-Path $testRoot 'public-fixture.pem'
        $now = [DateTimeOffset]::UtcNow
        $valid = $request.CreateSelfSigned($now.AddHours(-1), $now.AddDays(1))
        try { $publicPem = $valid.ExportCertificatePem() } finally { $valid.Dispose() }
        [IO.File]::WriteAllText($path, $publicPem, [Text.UTF8Encoding]::new($false))
        $pin = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
        Get-LanCertificate $path $pin '10.0.0.40' | Out-Null
        Assert-Refused { Get-LanCertificate $path $pin '10.0.0.41' }
        Assert-Refused { Get-LanCertificate $path ('A' * 64) '10.0.0.40' }
        foreach ($invalid in @($publicPem + "`n-----BEGIN PRIVATE KEY-----`nnot-a-key`n-----END PRIVATE KEY-----", $publicPem + "`n" + $publicPem, "-----BEGIN CERTIFICATE-----`nAA==`n-----END CERTIFICATE-----")) {
            [IO.File]::WriteAllText($path, $invalid, [Text.UTF8Encoding]::new($false))
            $pin = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
            Assert-Refused { Get-LanCertificate $path $pin '10.0.0.40' }
        }
        foreach ($interval in @(@($now.AddDays(-2),$now.AddDays(-1)), @($now.AddDays(1),$now.AddDays(2)))) {
            $invalid = $request.CreateSelfSigned($interval[0], $interval[1])
            try { [IO.File]::WriteAllText($path, $invalid.ExportCertificatePem(), [Text.UTF8Encoding]::new($false)) } finally { $invalid.Dispose() }
            $pin = (Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash
            Assert-Refused { Get-LanCertificate $path $pin '10.0.0.40' }
        }
    } finally { $rsa.Dispose() }
}
Test-Case 'traversal duplicate unknown and missing manifest rows' {
    foreach ($text in @($manifestText.Replace('NOTICE','../NOTICE'), $manifestText.Replace('NOTICE','credentials.json'), ($manifestText + $rows[0]), ($rows[1..($rows.Count - 1)] -join "`n"))) {
        [IO.File]::WriteAllText($manifest, $text, [Text.UTF8Encoding]::new($false))
        $pin = (Get-FileHash -LiteralPath $manifest -Algorithm SHA256).Hash
        Assert-Refused { Get-LanCandidate $root $pin $hashes['photocraft.exe'] $hashes['photocraft-sync.exe'] }
    }
    [IO.File]::WriteAllText($manifest, $manifestText, [Text.UTF8Encoding]::new($false))
}
Test-Case 'extra and changed candidate files' {
    $extra = Join-Path $root 'unexpected.txt'
    [IO.File]::WriteAllText($extra, 'unexpected')
    Assert-Refused { Get-LanCandidate $root $manifestHash $hashes['photocraft.exe'] $hashes['photocraft-sync.exe'] }
    [IO.File]::Delete($extra)
    $notice = Join-Path $root 'NOTICE'
    [IO.File]::WriteAllText($notice, 'changed notice')
    Assert-Refused { Get-LanCandidate $root $manifestHash $hashes['photocraft.exe'] $hashes['photocraft-sync.exe'] }
    [IO.File]::WriteAllText($notice, 'NOTICE', [Text.UTF8Encoding]::new($false))
}
Test-Case 'copy cannot clobber existing or publish mismatched input' {
    $target = Join-Path $testRoot 'copy-target'
    Copy-LanFile (Join-Path $root 'NOTICE') $target $hashes['NOTICE']
    Assert-Refused { Copy-LanFile (Join-Path $root 'NOTICE') $target $hashes['NOTICE'] }
    Assert-Test ([IO.File]::ReadAllText($target) -ceq 'NOTICE') 'Existing target changed.'
    $wrong = Join-Path $testRoot 'wrong-target'
    Assert-Refused { Copy-LanFile (Join-Path $root 'NOTICE') $wrong ('A' * 64) }
    Assert-Test (-not (Test-Path -LiteralPath $wrong)) 'Mismatch created a destination.'
}
Test-Case 'junction ancestors refused before reading or writing' {
    $outside = Join-Path $testRoot 'outside-sentinel'
    [IO.Directory]::CreateDirectory($outside) | Out-Null
    [IO.File]::WriteAllText((Join-Path $outside 'keep.txt'), 'untouched')
    $junction = Join-Path $testRoot 'redirect'
    New-Item -ItemType Junction -Path $junction -Target $outside | Out-Null
    Assert-Refused { Assert-LanAncestors (Join-Path $junction 'keep.txt') }
    Assert-Refused { Copy-LanFile (Join-Path $root 'NOTICE') (Join-Path $junction 'new.txt') $hashes['NOTICE'] }
    Assert-Test (-not (Test-Path -LiteralPath (Join-Path $outside 'new.txt'))) 'Write escaped through a junction.'
    Assert-Test ([IO.File]::ReadAllText((Join-Path $outside 'keep.txt')) -ceq 'untouched') 'Outside sentinel changed.'
}
Test-Case 'ZIP byte verification exact entry set and corruption rejection' {
    $payload = Join-Path $testRoot 'zip-source'
    [IO.Directory]::CreateDirectory($payload) | Out-Null
    [IO.File]::WriteAllText((Join-Path $payload 'test.txt'), 'zip fixture')
    $zip = Join-Path $testRoot 'payload.zip'
    [IO.Compression.ZipFile]::CreateFromDirectory($payload, $zip)
    $expected = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::Ordinal)
    $expected.Add('test.txt',(Get-FileHash -LiteralPath (Join-Path $payload 'test.txt') -Algorithm SHA256).Hash)
    Assert-LanZip $zip $expected
    $expected['test.txt'] = 'A' * 64
    Assert-Refused { Assert-LanZip $zip $expected }
    $expected.Add('../outside.txt','B' * 64)
    Assert-Refused { Assert-LanZip $zip $expected }
}
$failed = @($results | Where-Object { -not $_.passed })
[pscustomobject]@{root=$testRoot;passed=($results.Count - $failed.Count);failed=$failed.Count;results=$results} | ConvertTo-Json -Depth 5 | Tee-Object -FilePath (Join-Path $testRoot 'result.json')
if ($failed.Count) { exit 1 }
