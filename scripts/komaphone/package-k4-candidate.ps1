<#
.SYNOPSIS
  Copy explicitly verified Windows x64 binaries into a fresh local K4 candidate.
.DESCRIPTION
  No build, signing, install, network request, credential access or deployment.
  Destination must not exist; its parent must already exist. A unique sibling
  staging directory is published with a no-clobber directory rename only after
  every copied file and the SHA256 manifest have been verified. Failed staging
  is retained for inspection, never recursively removed by this script.

  Inputs are the native MSVC dev-profile optimized binaries used for local
  acceptance, not release-profile binaries. Supply their recorded SHA256s.
  If the app was built with CRAFT_FONTS_DIR, also supply that exact checkout as
  CraftFontsDirectory so its additional font licenses accompany the binary.
.EXAMPLE
  ./scripts/komaphone/package-k4-candidate.ps1 -AppExe <verified-app.exe> `
    -AppSha256 <recorded-sha256> -ServiceExe <verified-service.exe> `
    -ServiceSha256 <recorded-sha256> -Destination <new-candidate-directory>
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $AppExe,
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-fA-F]{64}$')] [string] $AppSha256,
    [Parameter(Mandatory)] [string] $ServiceExe,
    [Parameter(Mandatory)] [ValidatePattern('^[0-9a-fA-F]{64}$')] [string] $ServiceSha256,
    [Parameter(Mandatory)] [string] $Destination,
    [string] $CraftFontsDirectory
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '../..')).Path

function Get-CandidateFile([string] $Path) {
    $item = Get-Item -LiteralPath $Path
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Expected a regular local file: $Path"
    }
    return $item.FullName
}

function Get-CandidatePe([string] $Path) {
    $stream = [IO.File]::OpenRead($Path)
    $reader = [IO.BinaryReader]::new($stream)
    try {
        if ($stream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5A4D) { throw "Invalid Windows executable: $Path" }
        $stream.Position = 0x3C
        $offset = $reader.ReadUInt32()
        if ($offset -lt 64 -or ([long]$offset + 94) -gt $stream.Length) { throw "Invalid PE header bounds: $Path" }
        $stream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x00004550) { throw "Invalid PE signature: $Path" }
        $machine = $reader.ReadUInt16()
        $stream.Position = $offset + 24
        $magic = $reader.ReadUInt16()
        $stream.Position = $offset + 92
        $subsystem = $reader.ReadUInt16()
        if ($machine -ne 0x8664 -or $magic -ne 0x20B -or $subsystem -notin @(2, 3)) {
            throw "Candidate must be a Windows x64 executable: $Path"
        }
        return @{ machine = 'x64'; subsystem = $subsystem }
    } finally {
        $reader.Dispose()
    }
}

function Write-CandidateText([string] $Path, [string] $Text) {
    $stream = [IO.File]::Open($Path, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
    try {
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes($Text)
        $stream.Write($bytes, 0, $bytes.Length)
        $stream.Flush($true)
    } finally {
        $stream.Dispose()
    }
}

$appPath = Get-CandidateFile $AppExe
$servicePath = Get-CandidateFile $ServiceExe
$appPe = Get-CandidatePe $appPath
$servicePe = Get-CandidatePe $servicePath
if ($servicePe.subsystem -ne 3) { throw 'The private service must retain its console subsystem.' }
foreach ($inputFile in @(@{path=$appPath; hash=$AppSha256}, @{path=$servicePath; hash=$ServiceSha256})) {
    if ((Get-FileHash -LiteralPath $inputFile.path -Algorithm SHA256).Hash -ne $inputFile.hash) {
        throw "Input differs from its verified SHA256: $($inputFile.path)"
    }
    if ((Get-AuthenticodeSignature -LiteralPath $inputFile.path).Status -ne 'NotSigned') {
        throw 'This script labels the candidate unsigned; inspect this signed or invalid-signature input separately.'
    }
}

$outputPath = [IO.Path]::GetFullPath($Destination)
if (Test-Path -LiteralPath $outputPath) { throw "Destination already exists; nothing replaced: $outputPath" }
$parentPath = [IO.Path]::GetDirectoryName($outputPath)
if (-not $parentPath -or -not (Test-Path -LiteralPath $parentPath -PathType Container)) {
    throw 'Destination must be a new directory within an existing parent directory.'
}
$parentPath = (Resolve-Path -LiteralPath $parentPath).Path
$parentInfo = Get-Item -LiteralPath $parentPath
while ($null -ne $parentInfo) {
    if ($parentInfo.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Destination ancestors must not be junctions or symbolic links.' }
    $parentInfo = $parentInfo.Parent
}
$outputPath = Join-Path $parentPath ([IO.Path]::GetFileName($outputPath))
$stagePath = Join-Path $parentPath ('.komaphone-k4-staging-' + [Guid]::NewGuid().ToString('N'))
# Validate exact sibling targets before the final directory move. No delete occurs.
foreach ($targetPath in @($outputPath, $stagePath)) {
    if ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($targetPath)) -ne $parentPath) {
        throw 'Candidate staging and destination must stay within the selected parent.'
    }
}

# Explicit allowlist: no directory recursion from the build, profile or test roots.
$files = [Collections.Generic.List[object]]::new()
$files.Add(@{source=$appPath; relative='photocraft.exe'; expected=$AppSha256})
$files.Add(@{source=$servicePath; relative='photocraft-sync.exe'; expected=$ServiceSha256})
$notices = @(
    'LICENSE-MIT', 'LICENSE-APACHE', 'NOTICE', 'ATTRIBUTION.md',
    'assets/fonts/OFL-Inter.txt', 'assets/fonts/OFL-JetBrainsMono.txt',
    'assets/icons/LICENSE-lucide.txt', 'assets/icons/LICENSE-noun-magnetic-lasso.txt',
    'assets/icons/LICENSE-symmetry-axes.txt', 'assets/dict/LICENSE-SCOWL.txt',
    'assets/app-icon/LICENSE.txt', 'crates/algo/data/LICENSE-libfacedetection.txt',
    'crates/ui-egui/src/LICENSE-solarized.txt', 'crates/ui-egui/src/i18n/LICENSE-translations.txt',
    'docs/komaphone/k4-service.md', 'docs/komaphone/k4-party-release.md'
)
foreach ($relative in $notices) {
    $source = Get-CandidateFile (Join-Path $repoRoot $relative)
    $files.Add(@{source=$source; relative=$relative; expected=(Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash})
}
if (-not $CraftFontsDirectory -and $env:CRAFT_FONTS_DIR) {
    throw 'CRAFT_FONTS_DIR is set; explicitly supply the matching CraftFontsDirectory or confirm the verified build omitted it in a clean shell.'
}
$extraFonts = @()
if ($CraftFontsDirectory) {
    $fontRoot = (Resolve-Path -LiteralPath $CraftFontsDirectory).Path
    $extraFonts = @(Get-ChildItem -LiteralPath (Join-Path $fontRoot 'fonts') -Directory | ForEach-Object {
        $license = Get-CandidateFile (Join-Path $_.FullName 'OFL.txt')
        @{source=$license; relative=('OFL-' + $_.Name + '.txt'); expected=(Get-FileHash -LiteralPath $license -Algorithm SHA256).Hash}
    })
    if ($extraFonts.Count -eq 0) { throw 'No craft-fonts license files were found.' }
    foreach ($font in $extraFonts) { $files.Add($font) }
}

$quickstart = @'
# Komaphone K4 local candidate

This unsigned Windows x64 folder contains the MSVC dev-profile optimized app and
private service used for local testing. It is not a release-profile build or an
installer. The separate-Windows-machine private-network gate is still pending:
this candidate is not friend-ready. Keep the folder together and retain SHA256SUMS.

Komaphone is an independent project based on PhotoCraft. Upstream copyright,
licenses, contributors and asset attribution are preserved in LICENSE-MIT,
LICENSE-APACHE, NOTICE, ATTRIBUTION.md and the adjacent license files. This package
does not imply upstream endorsement. Existing photocraft executable names remain.

## Requirements

Use Windows x64 with a working wgpu-compatible graphics driver. These local
binaries dynamically import VCRUNTIME140.dll and the Windows Universal CRT.
Install the Microsoft Visual C++ v14 Redistributable x64, version 14.44.35229 or later,
if it is absent, using Microsoft's official download guidance:
https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/
The runtime installer is not bundled or run automatically. The upstream static-CRT
release packaging procedure was not used for this candidate. No Rust toolchain is
needed to run it. An app console window may remain visible in this dev build.

## Try it locally

Open PowerShell in this extracted candidate folder. Use a stable data directory
on a local NTFS disk outside OneDrive and other synchronized folders. These commands
keep candidate data separate from an existing installation.

In the first PowerShell window, start the service and leave it running:

```powershell
$serviceData = Join-Path $env:LOCALAPPDATA 'Komaphone/K4Candidate/service'
& ./photocraft-sync.exe --bind 127.0.0.1:43127 --data-dir $serviceData
```

In a second window, launch the first app profile:

```powershell
$env:KOMAPHONE_DATA_DIR = Join-Path $env:LOCALAPPDATA 'Komaphone/K4Candidate/player-one'
$env:PHOTOCRAFT_CONFIG_DIR = Join-Path $env:LOCALAPPDATA 'Komaphone/K4Candidate/config-one'
& ./photocraft.exe
```

For a second local player, use a third window and change both suffixes to
player-two and config-two. In each app open Private room and connect to
http://127.0.0.1:43127, create/join a room using its private invitation, then play
Animation or Exquisite Corpse. Separate profiles prevent single-instance forwarding.
Keep each profile's directories unchanged when restarting; credentials use that
Windows user's protected credential store. Do not clone live profiles.

At reveal choose Save to gallery, then replay, export, or Remix. Import & backups
supports portable archives and backup to a fresh data directory. Import and restore
wait until the active game finishes. Close apps normally; stop the service with
Ctrl+C. Keep your original data and a verified backup before changing directories.

## Next gate and detailed instructions

Read docs/komaphone/k4-party-release.md and docs/komaphone/k4-service.md for the
prepared private HTTPS route, certificate trust, backup/restore, limits and
two-separate-Windows-machine acceptance procedure. Their source-checkout example
binary paths under log/k4/bin mean ./photocraft.exe and ./photocraft-sync.exe in
this folder. Source-checkout automation scripts are not bundled here.

No firewall rule, public listener, hosting purchase, deployment, signing or
installation was performed by packaging. Test TLS keys, invitation tokens,
credentials, artwork, user profiles and test databases are excluded. Configure
your own private service certificate when the authorized private route is ready.

package-info.json records the exact candidate hashes and packaging assumptions.
SHA256SUMS covers every packaged file except SHA256SUMS itself. Verify its expected
hash through the coordinator's acceptance record before using a copied package.
'@

$packageInfo = [ordered]@{
    schemaVersion=1
    product='Komaphone'
    milestone='K4'
    status='local-only candidate; separate-machine private-network gate pending'
    buildProfile='MSVC native dev profile, optimized; not a release-profile build'
    architecture='x64'
    signed=$false
    createdUtc=[DateTime]::UtcNow.ToString('o')
    app=@{file='photocraft.exe';sha256=$AppSha256.ToUpperInvariant();pe=$appPe}
    service=@{file='photocraft-sync.exe';sha256=$ServiceSha256.ToUpperInvariant();pe=$servicePe}
    runtime='Microsoft Visual C++ v14 Redistributable x64 >=14.44.35229 and Windows UCRT; graphics driver for wgpu'
    optionalCraftFonts=($extraFonts.Count -gt 0)
    packagingScriptSha256=(Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash
    scope='Explicit binaries and notice/runbook allowlist only; no profile, credentials, test key or artwork copied'
}

if (Test-Path -LiteralPath $stagePath) { throw "Unique staging path already exists: $stagePath" }
[IO.Directory]::CreateDirectory($stagePath) | Out-Null
try {
    foreach ($file in $files) {
        $copyPath = Join-Path $stagePath $file.relative
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($copyPath)) | Out-Null
        [IO.File]::Copy($file.source, $copyPath, $false)
        if ((Get-FileHash -LiteralPath $copyPath -Algorithm SHA256).Hash -ne $file.expected) {
            throw "Copied file differs from verified input: $($file.relative)"
        }
    }
    Write-CandidateText (Join-Path $stagePath 'QUICKSTART.md') $quickstart
    Write-CandidateText (Join-Path $stagePath 'package-info.json') ($packageInfo | ConvertTo-Json -Depth 6)
    $status = "# Candidate package status`n`nThis is a local-only, unsigned K4 candidate. The separate-machine gate remains pending.`nSee ../../QUICKSTART.md and ../../package-info.json for package scope and exact binaries.`nThe repository coordinator retains full test results and screenshots separately.`n"
    Write-CandidateText (Join-Path $stagePath 'docs/komaphone/status.md') $status
    $manifest = @(Get-ChildItem -LiteralPath $stagePath -Recurse -File | Sort-Object FullName | ForEach-Object {
        $relative = $_.FullName.Substring($stagePath.Length + 1).Replace('\', '/')
        '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash, $relative
    })
    Write-CandidateText (Join-Path $stagePath 'SHA256SUMS') (($manifest -join "`n") + "`n")
    $manifestHash = (Get-FileHash -LiteralPath (Join-Path $stagePath 'SHA256SUMS') -Algorithm SHA256).Hash
    # Directory.Move fails if another process creates Destination; never overwrite it.
    [IO.Directory]::Move($stagePath, $outputPath)
    [pscustomobject]@{destination=$outputPath;files=$manifest.Count;sha256Manifest=$manifestHash;status=$packageInfo.status}
} catch {
    throw "Candidate packaging failed. No existing destination was replaced. Inspect retained staging at ${stagePath}: $($_.Exception.Message)"
}
