<#
.SYNOPSIS
  Prepare a pinned, isolated Windows K4 client ZIP for an explicitly chosen LAN.
.DESCRIPTION
  Builds only the standalone Rust launcher. Does not start the app or service,
  change firewall/trust settings, install software, publish, or copy private keys.
  Requires a verified candidate from package-k4-candidate.ps1 and PowerShell 7.4+.
#>
[CmdletBinding()]
param(
    [string] $CandidateDirectory,
    [string] $ManifestSha256,
    [string] $AppSha256,
    [string] $ServiceSha256,
    [string] $PublicCertificate,
    [string] $CertificateSha256,
    [string] $Endpoint,
    [string] $ProfileName,
    [string] $Destination,
    [switch] $ValidateOnly,
    [switch] $LibraryOnly
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path

function Get-LanRelativePath([string] $Base, [string] $Child) {
    if ($PSVersionTable.PSVersion.Major -ge 7) {
        return [IO.Path]::GetRelativePath($Base, $Child)
    }
    $prefix = [IO.Path]::GetFullPath($Base).TrimEnd('\', '/') + [IO.Path]::DirectorySeparatorChar
    $full = [IO.Path]::GetFullPath($Child)
    if (-not $full.StartsWith($prefix, [StringComparison]::Ordinal)) { throw "Path escapes root: $Child" }
    return $full.Substring($prefix.Length)
}
$candidateFiles = @(
    'photocraft.exe', 'photocraft-sync.exe', 'LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE', 'ATTRIBUTION.md',
    'assets/fonts/OFL-Inter.txt', 'assets/fonts/OFL-JetBrainsMono.txt',
    'assets/icons/LICENSE-lucide.txt', 'assets/icons/LICENSE-noun-magnetic-lasso.txt',
    'assets/icons/LICENSE-symmetry-axes.txt', 'assets/dict/LICENSE-SCOWL.txt', 'assets/app-icon/LICENSE.txt',
    'crates/algo/data/LICENSE-libfacedetection.txt', 'crates/ui-egui/src/LICENSE-solarized.txt',
    'crates/ui-egui/src/i18n/LICENSE-translations.txt', 'docs/komaphone/k4-service.md',
    'docs/komaphone/k4-party-release.md', 'QUICKSTART.md', 'package-info.json', 'docs/komaphone/status.md', 'SHA256SUMS'
)

function Assert-LanAncestors([string] $Path) {
    $cursor = [IO.Path]::GetFullPath($Path)
    for ($depth = 0; $cursor; $depth++) {
        if ($depth -gt 64) { throw 'Path exceeds ancestor bound.' }
        if (Test-Path -LiteralPath $cursor) {
            $item = Get-Item -LiteralPath $cursor -Force
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "Reparse point is forbidden: $cursor" }
        }
        $cursor = [IO.Path]::GetDirectoryName($cursor)
    }
}

function Get-LanFile([string] $Path, [long] $Limit = 209715200) {
    Assert-LanAncestors $Path
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.PSIsContainer -or $item.Length -gt $Limit) { throw "Expected a bounded regular file: $Path" }
    return $item.FullName
}

function Assert-LanHash([string] $Path, [string] $Hash, [long] $Limit = 209715200) {
    if ($Hash -cnotmatch '^[0-9A-F]{64}$') { throw 'Expected an uppercase SHA256 pin.' }
    $file = Get-LanFile $Path $Limit
    if ((Get-FileHash -LiteralPath $file -Algorithm SHA256).Hash -cne $Hash) { throw "SHA256 mismatch: $file" }
}

function Get-LanEndpoint([string] $Value) {
    if ($Value -cnotmatch '^https://([0-9]+\.[0-9]+\.[0-9]+\.[0-9]+):([0-9]+)$') { throw 'Endpoint must be private numeric IPv4 HTTPS with an explicit port.' }
    $addressText = $Matches[1]
    $portText = $Matches[2]
    $address = [Net.IPAddress]::Parse($addressText)
    $octets = $address.GetAddressBytes()
    $port = 0
    if ($octets.Length -ne 4 -or -not [int]::TryParse($portText, [ref]$port) -or $port -lt 1 -or $port -gt 65535) { throw 'Invalid private IPv4 endpoint.' }
    $private = $octets[0] -eq 10 -or ($octets[0] -eq 172 -and $octets[1] -ge 16 -and $octets[1] -le 31) -or ($octets[0] -eq 192 -and $octets[1] -eq 168)
    if (-not $private -or $Value -cne "https://${address}:${port}") { throw 'Endpoint must use canonical RFC1918 IPv4 spelling.' }
    return $address.ToString()
}

function Get-LanCertificate([string] $Path, [string] $Hash, [string] $Address) {
    $certificate = Get-LanFile $Path 16384
    Assert-LanHash $certificate $Hash 16384
    $pem = [IO.File]::ReadAllText($certificate)
    if ($pem -cnotmatch '\A\s*-----BEGIN CERTIFICATE-----\s*([A-Za-z0-9+/=\s]+)\s*-----END CERTIFICATE-----\s*\z') { throw 'Transfer exactly one public PEM certificate, without a private key.' }
    $certificateBytes = [Convert]::FromBase64String($Matches[1])
    $x509 = [Security.Cryptography.X509Certificates.X509Certificate2]::new($certificateBytes)
    try {
        if ($x509.HasPrivateKey -or -not $x509.MatchesHostname($Address, $false, $false)) { throw 'Public certificate SAN does not match the chosen private address.' }
        $now = [DateTime]::UtcNow
        if ($now -lt $x509.NotBefore.ToUniversalTime() -or $now -ge $x509.NotAfter.ToUniversalTime()) { throw 'Public certificate is not currently valid.' }
        return @{pemSha256=$Hash;derSha256=[Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($certificateBytes));notBeforeUtc=$x509.NotBefore.ToUniversalTime().ToString('o');notAfterUtc=$x509.NotAfter.ToUniversalTime().ToString('o')}
    } finally { $x509.Dispose() }
}

function Get-LanCandidate([string] $Root, [string] $ManifestHash, [string] $AppHash, [string] $ServiceHash) {
    Assert-LanAncestors $Root
    $rootPath = (Resolve-Path -LiteralPath $Root).Path
    Assert-LanHash (Join-Path $rootPath 'SHA256SUMS') $ManifestHash 16384
    $hashes = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::Ordinal)
    foreach ($line in [IO.File]::ReadAllLines((Join-Path $rootPath 'SHA256SUMS'))) {
        if ($line -cnotmatch '^([0-9A-F]{64})  (.+)$') { throw 'Invalid candidate manifest row.' }
        $hash = $Matches[1]
        $relative = $Matches[2]
        if ($relative -ceq 'SHA256SUMS' -or -not ($candidateFiles -ccontains $relative) -or $hashes.ContainsKey($relative)) { throw 'Unexpected or duplicate candidate manifest path.' }
        $hashes.Add($relative, $hash)
    }
    if ($hashes.Count -ne $candidateFiles.Count - 1) { throw 'Incomplete candidate manifest.' }
    if ($hashes['photocraft.exe'] -cne $AppHash -or $hashes['photocraft-sync.exe'] -cne $ServiceHash) { throw 'Candidate binary pins disagree.' }
    $hashes.Add('SHA256SUMS', $ManifestHash)
    $items = @(Get-ChildItem -LiteralPath $rootPath -Recurse -Force)
    $files = @($items | Where-Object { -not $_.PSIsContainer })
    if ($files.Count -ne $candidateFiles.Count) { throw 'Candidate file set differs from the allowlist.' }
    foreach ($item in $items) {
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Candidate contains a reparse point.' }
        if (-not $item.PSIsContainer) {
            $relative = [IO.Path]::GetRelativePath($rootPath, $item.FullName).Replace('\', '/')
            if (-not $hashes.ContainsKey($relative)) { throw "Unexpected candidate file: $relative" }
            Assert-LanHash $item.FullName $hashes[$relative]
        }
    }
    return @{path=$rootPath;hashes=$hashes}
}

function Write-LanText([string] $Path, [string] $Text) {
    Assert-LanAncestors $Path
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes($Text)
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
    } finally { $stream.Dispose() }
}

function Copy-LanFile([string] $Source, [string] $Target, [string] $Hash) {
    Assert-LanHash $Source $Hash
    Assert-LanAncestors $Target
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Target)) | Out-Null
    [IO.File]::Copy($Source, $Target, $false)
    Assert-LanHash $Target $Hash
}

function New-LanZip([string] $SourceDirectory, [string] $ZipPath) {
    if ($PSVersionTable.PSVersion.Major -ge 7) {
        [IO.Compression.ZipFile]::CreateFromDirectory($SourceDirectory, $ZipPath, [IO.Compression.CompressionLevel]::Optimal, $false)
        return
    }
    $zipStream = [IO.File]::Open($ZipPath, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $archive = [IO.Compression.ZipArchive]::new($zipStream, [IO.Compression.ZipArchiveMode]::Create, $false)
        try {
            foreach ($file in @(Get-ChildItem -LiteralPath $SourceDirectory -Recurse -File | Sort-Object FullName)) {
                $entryName = (Get-LanRelativePath $SourceDirectory $file.FullName).Replace('\', '/')
                [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($archive, $file.FullName, $entryName, [IO.Compression.CompressionLevel]::Optimal) | Out-Null
            }
        } finally { $archive.Dispose() }
    } finally { $zipStream.Dispose() }
}

function Assert-LanZip([string] $Path, [Collections.Generic.Dictionary[string,string]] $Expected) {
    $archive = [IO.Compression.ZipFile]::OpenRead($Path)
    try {
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        if ($archive.Entries.Count -ne $Expected.Count) { throw 'ZIP entry count differs from the exact payload.' }
        foreach ($entry in $archive.Entries) {
            if (-not $Expected.ContainsKey($entry.FullName) -or -not $seen.Add($entry.FullName) -or $entry.Length -gt 209715200) { throw "Unexpected/duplicate/oversized ZIP entry: $($entry.FullName)" }
            $stream = $entry.Open()
            $sha = [Security.Cryptography.SHA256]::Create()
            try {
                $actual = if ($PSVersionTable.PSVersion.Major -ge 7) {
                    [Convert]::ToHexString($sha.ComputeHash($stream))
                } else {
                    [BitConverter]::ToString($sha.ComputeHash($stream)).Replace('-', '')
                }
                if ($actual -cne $Expected[$entry.FullName]) { throw "ZIP bytes differ: $($entry.FullName)" }
            } finally { $sha.Dispose(); $stream.Dispose() }
        }
    } finally { $archive.Dispose() }
}

if ($LibraryOnly) { return }
foreach ($argument in @($CandidateDirectory, $ManifestSha256, $AppSha256, $ServiceSha256, $PublicCertificate, $CertificateSha256, $Endpoint, $ProfileName, $Destination)) {
    if ([string]::IsNullOrWhiteSpace($argument)) { throw 'Candidate, all SHA256 pins, certificate, endpoint, profile and fresh destination are required.' }
}
if ($ProfileName -cnotmatch '^k4-lan-[a-z0-9-]{1,57}$') { throw 'Profile must be a lowercase k4-lan- namespace of at most 64 characters.' }
$endpointAddress = Get-LanEndpoint $Endpoint
$candidate = Get-LanCandidate $CandidateDirectory $ManifestSha256 $AppSha256 $ServiceSha256
$certificate = Get-LanFile $PublicCertificate 16384
$certificateInfo = Get-LanCertificate $certificate $CertificateSha256 $endpointAddress
$outputPath = [IO.Path]::GetFullPath($Destination)
Assert-LanAncestors $outputPath
$parentPath = [IO.Path]::GetDirectoryName($outputPath)
if (-not (Test-Path -LiteralPath $parentPath -PathType Container)) { throw 'Destination parent must exist.' }
$zipPath = $outputPath + '.zip'
if ((Test-Path -LiteralPath $outputPath) -or (Test-Path -LiteralPath $zipPath)) { throw 'Destination directory and ZIP must both be new; no existing files are replaced.' }
if ($ValidateOnly) {
    [pscustomobject]@{validated=$true;candidate=$candidate.path;endpoint=$Endpoint;profile=$ProfileName;certificate=$certificateInfo;destination=$outputPath;zip=$zipPath}
    return
}

$stagePath = Join-Path $parentPath ('.komaphone-k4-lan-stage-' + [Guid]::NewGuid().ToString('N'))
$zipStage = $stagePath + '.zip'
foreach ($target in @($outputPath, $zipPath, $stagePath, $zipStage)) {
    if ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($target)) -ne $parentPath) { throw 'All publication targets must remain under the selected parent.' }
}
[IO.Directory]::CreateDirectory($stagePath) | Out-Null
$buildLog = Join-Path $parentPath ('.komaphone-k4-launcher-build-' + [Guid]::NewGuid().ToString('N') + '.log')
$environment = @{
    CARGO_HOME=(Join-Path $repoRoot 'target/k0-tools/cargo'); RUSTUP_HOME=(Join-Path $repoRoot 'target/k0-tools/rustup')
    PATH=((Join-Path $repoRoot 'target/k0-tools/cargo/bin') + [IO.Path]::PathSeparator + $env:PATH)
    CARGO_TARGET_DIR=(Join-Path $repoRoot 'target/agent-k4-lan-package'); CARGO_BUILD_JOBS='2'; CARGO_INCREMENTAL='0'; CARGO_NET_OFFLINE='true'
    RUSTFLAGS='-C target-feature=+crt-static'; CARGO_ENCODED_RUSTFLAGS=$null
    KOMAPHONE_PACKAGE_MANIFEST_SHA=$ManifestSha256; KOMAPHONE_PACKAGE_APP_SHA=$AppSha256; KOMAPHONE_PACKAGE_SERVICE_SHA=$ServiceSha256
    KOMAPHONE_PACKAGE_CERT_SHA=$CertificateSha256; KOMAPHONE_PACKAGE_ENDPOINT=$Endpoint; KOMAPHONE_PACKAGE_PROFILE=$ProfileName
}
$oldEnvironment = @{}
try {
    foreach ($name in $environment.Keys) {
        $oldEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
        if ($null -eq $environment[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $environment[$name], 'Process') }
    }
    $cargo = Join-Path $environment.CARGO_HOME 'bin/cargo.exe'
    & $cargo build --manifest-path (Join-Path $PSScriptRoot 'k4-launcher/Cargo.toml') --release --locked --offline *> $buildLog
    if ($LASTEXITCODE -ne 0) { throw "Launcher build failed; see $buildLog" }
} finally {
    foreach ($name in $oldEnvironment.Keys) {
        if ($null -eq $oldEnvironment[$name]) { Remove-Item -LiteralPath "Env:$name" -ErrorAction SilentlyContinue }
        else { [Environment]::SetEnvironmentVariable($name, $oldEnvironment[$name], 'Process') }
    }
}
$launcher = Get-LanFile (Join-Path $repoRoot 'target/agent-k4-lan-package/release/komaphone-k4-launcher.exe')
$launcherHash = (Get-FileHash -LiteralPath $launcher -Algorithm SHA256).Hash
foreach ($relative in $candidateFiles) { Copy-LanFile (Join-Path $candidate.path $relative) (Join-Path $stagePath ('candidate/' + $relative)) $candidate.hashes[$relative] }
Copy-LanFile $certificate (Join-Path $stagePath 'publiccert/service.pem') $CertificateSha256
foreach ($name in @('Start-Komaphone.exe', 'Start-Komaphone-Safe-GPU.exe')) { Copy-LanFile $launcher (Join-Path $stagePath $name) $launcherHash }
$guide = Get-LanFile (Join-Path $repoRoot 'docs/komaphone/k4-second-pc.md')
Copy-LanFile $guide (Join-Path $stagePath 'docs/komaphone/k4-second-pc.md') (Get-FileHash -LiteralPath $guide -Algorithm SHA256).Hash
$launcherNotices = Get-LanFile (Join-Path $PSScriptRoot 'k4-launcher/THIRD-PARTY-NOTICES.txt') 1048576
$launcherLockHash = (Get-FileHash -LiteralPath (Join-Path $PSScriptRoot 'k4-launcher/Cargo.lock') -Algorithm SHA256).Hash
if (-not ([IO.File]::ReadAllLines($launcherNotices) -ccontains "Cargo.lock SHA256: $launcherLockHash")) { throw 'Launcher notices are stale; run k4-launcher/update-notices.ps1 and review the resolved license files.' }
$launcherNoticesHash = (Get-FileHash -LiteralPath $launcherNotices -Algorithm SHA256).Hash
Copy-LanFile $launcherNotices (Join-Path $stagePath 'LAUNCHER-THIRD-PARTY-NOTICES.txt') $launcherNoticesHash
$readme = @'
KOMAPHONE - K4 SECOND-PC TEST CANDIDATE

This unsigned Windows x64 candidate is ready for an assisted private-network
test. The separate-machine acceptance gate is still pending; it is not a
friend-ready release. No Rust, Git, Visual Studio or source checkout is needed.

1. Use Windows 10/11 x64 with a working graphics driver. Install Microsoft's
   Visual C++ v14 x64 runtime (14.44.35229 or later), if needed:
   https://aka.ms/vc14/vc_redist.x64.exe
   Official guidance: https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/
2. Put both PCs on the same normal home network. Keep the host PC awake.
3. Extract the complete ZIP into a local folder. Keep its files together.
4. Double-click Start-Komaphone.exe. First launch verifies and copies the app
   into the isolated local folder shown below. Future launches keep your work.
5. When the coordinator has started the private service, open Private room,
   choose a display name, verify the server field below, and choose Connect.
   Enter the private invitation supplied for that session to join the room.

Service: @ENDPOINT@
Local app/data/config/log root: %LOCALAPPDATA%\Komaphone\@PROFILE@
Launcher log: that folder's launcher-log.txt
Relative export filenames are saved in that folder's exports subfolder.
Public certificate validity ends: @EXPIRES@

The launcher provides certificate trust only to the app process; it does not
change Windows trust, firewall, router settings or start the private service.
The service must be started separately by the coordinator. Its address must
still match the packaged endpoint. If the address or certificate changes, use
a newly prepared package; do not edit or replace pinned files.

If no window appears, check the runtime installation and launcher-log.txt.
If normal graphics startup fails, close the app and try
Start-Komaphone-Safe-GPU.exe. It uses the same local profile and may run slower.
Keep originals/backups before changing data directories. Do not copy a live
profile to another PC; invitations provide access without cloning credentials.

The coordinator will guide the two game modes, reconnect/restart, gallery,
replay/export/remix, and fresh archive import checks. See
docs/komaphone/k4-second-pc.md for details and known limits.

Komaphone is independent and based on PhotoCraft. Upstream and asset licenses,
copyright, contributor notices and attribution remain under candidate/.
Existing photocraft executable names are retained; no upstream endorsement is implied.
Standalone launcher dependency notices are in LAUNCHER-THIRD-PARTY-NOTICES.txt.

TRANSFER-SHA256SUMS covers all files except itself. The coordinator separately
provides the ZIP's SHA256. This package contains a public certificate only:
no service private key, invitations, credentials, user artwork or test database.
'@
$readme = $readme.Replace('@ENDPOINT@', $Endpoint).Replace('@PROFILE@', $ProfileName).Replace('@EXPIRES@', $certificateInfo.notAfterUtc)
Write-LanText (Join-Path $stagePath 'README.txt') $readme
$info = [ordered]@{
    schemaVersion=1;product='Komaphone';milestone='K4';status='local-only candidate; separate-machine private-network gate pending'
    architecture='x64';signed=$false;createdUtc=[DateTime]::UtcNow.ToString('o');endpoint=$Endpoint;profile=$ProfileName
    appSha256=$AppSha256;serviceSha256=$ServiceSha256;candidateManifestSha256=$ManifestSha256;launcherSha256=$launcherHash;certificate=$certificateInfo
    appBuild='MSVC native optimized dev profile';launcherBuild='Rust release with static CRT';runtime='Microsoft Visual C++ v14 x64 >=14.44.35229 for app'
    packagingScriptSha256=(Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
    launcherSourceSha256=(Get-FileHash -LiteralPath (Join-Path $PSScriptRoot 'k4-launcher/src/main.rs') -Algorithm SHA256).Hash
    launcherCargoLockSha256=$launcherLockHash;launcherNoticesSha256=$launcherNoticesHash
    scope='No service key, credentials, invitations, profile data, public hosting or firewall/trust changes.'
}
Write-LanText (Join-Path $stagePath 'transfer-info.json') ($info | ConvertTo-Json -Depth 5)
$expected = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::Ordinal)
$rows = @(Get-ChildItem -LiteralPath $stagePath -Recurse -File | Sort-Object FullName | ForEach-Object {
    $relative = [IO.Path]::GetRelativePath($stagePath, $_.FullName).Replace('\', '/')
    $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
    $expected.Add($relative, $hash)
    "$hash  $relative"
})
Write-LanText (Join-Path $stagePath 'TRANSFER-SHA256SUMS') (($rows -join "`n") + "`n")
$expected.Add('TRANSFER-SHA256SUMS', (Get-FileHash -LiteralPath (Join-Path $stagePath 'TRANSFER-SHA256SUMS') -Algorithm SHA256).Hash)
[IO.Compression.ZipFile]::CreateFromDirectory($stagePath, $zipStage, [IO.Compression.CompressionLevel]::Optimal, $false)
Assert-LanZip $zipStage $expected
# Both operations fail if another process created a destination. Existing data is never replaced.
[IO.Directory]::Move($stagePath, $outputPath)
[IO.File]::Move($zipStage, $zipPath, $false)
[pscustomobject]@{
    destination=$outputPath;zip=$zipPath;zipSha256=(Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash;files=$expected.Count
    transferManifestSha256=$expected['TRANSFER-SHA256SUMS'];launcherSha256=$launcherHash;endpoint=$Endpoint;profile=$ProfileName;buildLog=$buildLog;status=$info.status
}
