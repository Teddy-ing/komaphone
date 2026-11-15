<#
.SYNOPSIS
  Package one explicitly verified Windows app and a hosted launcher into a fresh ZIP.
.DESCRIPTION
  Builds/tests only the small Rust launcher. No app build, network configuration,
  service binary, custom certificate, user credentials, profile or artwork is included.
  Packages only pinned, sanitized Google Desktop public-client configuration.
  New destination and ZIP only. Failed staging is retained, never recursively deleted.
  This allowlist is for the verified build without optional CRAFT_FONTS_DIR input.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $AppExe,
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-fA-F]{64}$')] [string] $AppSha256,
    [Parameter(Mandatory)] [string] $Destination,
    [Parameter(Mandatory)] [string] $GoogleClientJson,
    [ValidateSet('K6.5', 'K7')] [string] $Milestone = 'K6.5',
    [ValidatePattern('^k65-hosted[a-z0-9-]{0,54}$')] [string] $ProfileName = 'k65-hosted',
    [string] $CargoExe,
    [switch] $ValidateOnly
)
function Get-KomaphoneCandidatePresentation {
    param([ValidateSet('K6.5', 'K7')] [string] $Milestone = 'K6.5')
    if ($Milestone -eq 'K7') {
        return [pscustomobject]@{
            milestone = 'K7'
            status = 'release hardening test candidate; K7 acceptance pending'
            heading = 'KOMAPHONE - K7 RELEASE HARDENING TEST CANDIDATE'
            guide = 'docs/komaphone/k7-friends.md'
            warning = 'This unsigned K7 test candidate has not completed K7 release acceptance.'
        }
    }
    return [pscustomobject]@{
        milestone = 'K6.5'
        status = 'hosted test candidate; hosted acceptance pending'
        heading = 'KOMAPHONE - K6.5 HOSTED TEST CANDIDATE'
        guide = 'docs/komaphone/k65-friends.md'
        warning = 'This unsigned test candidate has not completed all hosted acceptance.'
    }
}
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$presentation = Get-KomaphoneCandidatePresentation -Milestone $Milestone
$scriptRoot = $PSScriptRoot
$packageScript = $PSCommandPath
$workspaceRoot = (Resolve-Path -LiteralPath (Join-Path $scriptRoot '../..')).Path
# Reuse K4's bounded path/hash/copy/ZIP helpers. Its LAN-specific flow is not run.
$requested = @{AppSha256=$AppSha256;Destination=$Destination;ProfileName=$ProfileName;ValidateOnly=$ValidateOnly}
. (Join-Path $scriptRoot 'package-k4-lan.ps1') -LibraryOnly
$AppSha256 = $requested.AppSha256
$Destination = $requested.Destination
$ProfileName = $requested.ProfileName
$ValidateOnly = $requested.ValidateOnly
$AppSha256 = $AppSha256.ToUpperInvariant()
. (Join-Path $scriptRoot 'k65-launcher/sanitize-google-config.ps1')
$googleConfiguration = Read-K65GoogleDesktopConfiguration (Get-LanFile $GoogleClientJson)
$appPath = Get-LanFile $AppExe
Assert-LanHash $appPath $AppSha256
if ($env:CRAFT_FONTS_DIR) { throw 'This package allowlist requires a verified build without optional CRAFT_FONTS_DIR fonts.' }
$stream = [IO.File]::OpenRead($appPath)
$reader = [IO.BinaryReader]::new($stream)
try {
    if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) { throw 'App must be a Windows PE executable.' }
    $stream.Position = 0x3C
    $offset = $reader.ReadUInt32()
    if ($offset -lt 64 -or ([long]$offset + 26) -gt $stream.Length) { throw 'Invalid PE header bounds.' }
    $stream.Position = $offset
    if ($reader.ReadUInt32() -ne 0x00004550 -or $reader.ReadUInt16() -ne 0x8664) { throw 'App must be Windows x64.' }
    $stream.Position = $offset + 24
    if ($reader.ReadUInt16() -ne 0x20B) { throw 'App must be PE32+.' }
} finally { $reader.Dispose() }
if ((Get-AuthenticodeSignature -LiteralPath $appPath).Status -ne 'NotSigned') {
    throw 'This candidate is labeled unsigned; inspect signed or invalid-signature input separately.'
}
$outputPath = [IO.Path]::GetFullPath($Destination)
$zipPath = $outputPath + '.zip'
Assert-LanAncestors $outputPath
foreach ($path in @($outputPath, $zipPath)) { if (Test-Path -LiteralPath $path) { throw "Destination exists; nothing replaced: $path" } }
$parentPath = [IO.Path]::GetDirectoryName($outputPath)
if (-not (Test-Path -LiteralPath $parentPath -PathType Container)) { throw 'Destination parent must already exist.' }
$notices = @(
    'LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE', 'ATTRIBUTION.md',
    'assets/fonts/OFL-Inter.txt', 'assets/fonts/OFL-JetBrainsMono.txt',
    'assets/icons/LICENSE-lucide.txt', 'assets/icons/LICENSE-noun-magnetic-lasso.txt',
    'assets/icons/LICENSE-symmetry-axes.txt', 'assets/dict/LICENSE-SCOWL.txt',
    'assets/app-icon/LICENSE.txt', 'crates/algo/data/LICENSE-libfacedetection.txt',
    'crates/ui-egui/src/LICENSE-solarized.txt', 'crates/ui-egui/src/i18n/LICENSE-translations.txt',
    'docs/komaphone/k65-friends.md'
)
foreach ($path in $notices) { Get-LanFile (Join-Path $workspaceRoot $path) | Out-Null }
$guidePath = Get-LanFile (Join-Path $workspaceRoot $presentation.guide)
if ($ValidateOnly) { return [pscustomobject]@{validated=$true;appSha256=$AppSha256;destination=$outputPath;zip=$zipPath;profile=$ProfileName;milestone=$presentation.milestone;status=$presentation.status;guide=$presentation.guide} }
$stagePath = Join-Path $parentPath ('.komaphone-k65-stage-' + [Guid]::NewGuid().ToString('N'))
$zipStage = $stagePath + '.zip'
foreach ($path in @($outputPath, $zipPath, $stagePath, $zipStage)) {
    if ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($path)) -ne $parentPath) { throw 'Publication targets must remain within the chosen parent.' }
}
[IO.Directory]::CreateDirectory($stagePath) | Out-Null
$candidate = Join-Path $stagePath 'candidate'
Copy-LanFile $appPath (Join-Path $candidate 'photocraft.exe') $AppSha256
foreach ($relative in $notices) {
    # Keep the launcher's fixed inventory path while selecting the matching guide.
    $source = if ($relative -eq 'docs/komaphone/k65-friends.md') { $guidePath } else { Join-Path $workspaceRoot $relative }
    Copy-LanFile $source (Join-Path $candidate $relative) (Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
}
Write-LanText (Join-Path $candidate 'google-desktop-client.json') $googleConfiguration
$info = [ordered]@{
    schemaVersion=1; product='Komaphone'; milestone=$presentation.milestone; status=$presentation.status
    architecture='x64'; signed=$false; createdUtc=[DateTime]::UtcNow.ToString('o'); appSha256=$AppSha256
    endpoint='https://komaphone.tedsoftware.org'; profile=$ProfileName; optionalCraftFonts=$false
    appBuild='Verified MSVC optimized native candidate; exact hash supplied by coordinator'
    runtime='Microsoft Visual C++ v14 x64 runtime at least as recent as the app build tools; Windows 10/11 x64 graphics driver'
    packagingScriptSha256=(Get-FileHash -LiteralPath $packageScript -Algorithm SHA256).Hash
    googleClientConfiguration='Pinned sanitized installed-app public-client metadata; no user access tokens or admission credentials'
    scope='Explicit app/public-client-config/notices/guide allowlist; no service executable, custom certificate, user tokens, profile or artwork'
}
Write-LanText (Join-Path $candidate 'package-info.json') ($info | ConvertTo-Json -Depth 5)
$rows = @(Get-ChildItem -LiteralPath $candidate -Recurse -File | Sort-Object FullName | ForEach-Object {
    $relative = [IO.Path]::GetRelativePath($candidate, $_.FullName).Replace('\','/')
    "$( (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash )  $relative"
})
Write-LanText (Join-Path $candidate 'SHA256SUMS') (($rows -join "`n") + "`n")
$manifestHash = (Get-FileHash -LiteralPath (Join-Path $candidate 'SHA256SUMS') -Algorithm SHA256).Hash
$launcherRoot = Join-Path $scriptRoot 'k65-launcher'
. (Join-Path $launcherRoot 'build-environment.ps1')
$targetPath = Join-Path $workspaceRoot 'target/agent-k65-launcher'
$toolCargo = Join-Path $workspaceRoot 'target/k0-tools/cargo'
$toolRustup = Join-Path $workspaceRoot 'target/k0-tools/rustup'
if (-not $CargoExe) { $CargoExe = Join-Path $toolCargo 'bin/cargo.exe' }
$cargoPath = Get-LanFile $CargoExe
$buildLog = Join-Path $parentPath ('.komaphone-k65-launcher-' + [Guid]::NewGuid().ToString('N') + '.log')
$environment = @{
    CARGO_HOME=$toolCargo; RUSTUP_HOME=$toolRustup
    PATH=((Join-Path $toolCargo 'bin') + [IO.Path]::PathSeparator + $env:PATH)
    CARGO_TARGET_DIR=$targetPath; CARGO_BUILD_JOBS='2'; CARGO_INCREMENTAL='0'; CARGO_NET_OFFLINE='true'
    KOMAPHONE_PACKAGE_MANIFEST_SHA=$manifestHash; KOMAPHONE_PACKAGE_APP_SHA=$AppSha256; KOMAPHONE_PACKAGE_PROFILE=$ProfileName
}
$environment += Get-K65LauncherCompilerEnvironment
Invoke-K65BuildEnvironment -Variables $environment -Body {
    & $cargoPath test --manifest-path (Join-Path $launcherRoot 'Cargo.toml') --locked --offline *> $buildLog
    if ($LASTEXITCODE -ne 0) { throw "Launcher tests failed: $buildLog" }
    & $cargoPath clippy --manifest-path (Join-Path $launcherRoot 'Cargo.toml') --locked --offline --all-targets -- -D warnings *>> $buildLog
    if ($LASTEXITCODE -ne 0) { throw "Launcher lint failed: $buildLog" }
    & $cargoPath build --manifest-path (Join-Path $launcherRoot 'Cargo.toml') --release --locked --offline *>> $buildLog
    if ($LASTEXITCODE -ne 0) { throw "Launcher build failed: $buildLog" }
}
$launcher = Get-LanFile (Join-Path $targetPath 'release/komaphone-k65-launcher.exe')
$launcherHash = (Get-FileHash -LiteralPath $launcher -Algorithm SHA256).Hash
foreach ($name in @('Start-Komaphone.exe', 'Start-Komaphone-Safe-GPU.exe')) { Copy-LanFile $launcher (Join-Path $stagePath $name) $launcherHash }
$launcherNotices = Join-Path $launcherRoot 'THIRD-PARTY-NOTICES.txt'
$lockHash = (Get-FileHash -LiteralPath (Join-Path $launcherRoot 'Cargo.lock') -Algorithm SHA256).Hash
if (-not ([IO.File]::ReadAllLines($launcherNotices) -ccontains "Cargo.lock SHA256: $lockHash")) { throw 'Launcher notices do not match its dependency lock.' }
Copy-LanFile $launcherNotices (Join-Path $stagePath 'LAUNCHER-THIRD-PARTY-NOTICES.txt') (Get-FileHash -LiteralPath $launcherNotices -Algorithm SHA256).Hash
$readme = @'
@HEADING@

Extract the whole ZIP. Double-click Start-Komaphone.exe.
In Private room, expand First-time service invitation and enter the host's code,
then choose Sign in with Google.
Service: https://komaphone.tedsoftware.org
No VPN, certificate import, router setup or developer tokens are needed.

Local drawing/profile/export/log folder: %LOCALAPPDATA%\Komaphone\@PROFILE@
Updates keep this profile and install the new program in a separate version folder.
Keep old drawing folders/backups. Do not clone a live profile or delete its data.
Close Komaphone before relaunching. Relative export names use the exports folder.

If launch fails, read launcher-log.txt in the profile folder. Close the app and try
Start-Komaphone-Safe-GPU.exe. Install Microsoft's current Visual C++ v14 x64 runtime
if needed: https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/

Read candidate/docs/komaphone/k65-friends.md for the short guide and test limits.
@WARNING@ Only use
the exact ZIP/hash supplied by the coordinator; do not replace verified files.

Komaphone is independently based on PhotoCraft, without upstream endorsement.
Upstream copyright, licenses, contributors and attribution remain in candidate/.
Launcher dependency licenses are in LAUNCHER-THIRD-PARTY-NOTICES.txt.
'@
Write-LanText (Join-Path $stagePath 'README.txt') ($readme.Replace('@PROFILE@',$ProfileName).Replace('@HEADING@',$presentation.heading).Replace('@WARNING@',$presentation.warning))
$transfer = [ordered]@{schemaVersion=1;product='Komaphone';milestone=$presentation.milestone;status=$info.status;appSha256=$AppSha256;candidateManifestSha256=$manifestHash;launcherSha256=$launcherHash;profile=$ProfileName;endpoint=$info.endpoint;launcherLockSha256=$lockHash}
Write-LanText (Join-Path $stagePath 'transfer-info.json') ($transfer | ConvertTo-Json -Depth 4)
$expected = [Collections.Generic.Dictionary[string,string]]::new([StringComparer]::Ordinal)
$rows = @(Get-ChildItem -LiteralPath $stagePath -Recurse -File | Sort-Object FullName | ForEach-Object {
    $relative = [IO.Path]::GetRelativePath($stagePath, $_.FullName).Replace('\','/')
    $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash
    $expected.Add($relative,$hash)
    "$hash  $relative"
})
Write-LanText (Join-Path $stagePath 'TRANSFER-SHA256SUMS') (($rows -join "`n") + "`n")
$expected.Add('TRANSFER-SHA256SUMS',(Get-FileHash -LiteralPath (Join-Path $stagePath 'TRANSFER-SHA256SUMS') -Algorithm SHA256).Hash)
[IO.Compression.ZipFile]::CreateFromDirectory($stagePath,$zipStage,[IO.Compression.CompressionLevel]::Optimal,$false)
Assert-LanZip $zipStage $expected
[IO.Directory]::Move($stagePath,$outputPath)
[IO.File]::Move($zipStage,$zipPath,$false)
[pscustomobject]@{destination=$outputPath;zip=$zipPath;zipSha256=(Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash;files=$expected.Count;candidateManifestSha256=$manifestHash;launcherSha256=$launcherHash;profile=$ProfileName;buildLog=$buildLog;status=$info.status}
