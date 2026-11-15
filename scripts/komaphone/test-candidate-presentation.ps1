# Exercise the packager's pure presentation selector without executing packaging,
# reading Google configuration, building a launcher, or touching existing packages.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$path = Join-Path $PSScriptRoot 'package-k65-candidate.ps1'
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
if ($errors.Count -ne 0) { throw 'Packager has PowerShell syntax errors.' }
$function = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -eq 'Get-KomaphoneCandidatePresentation' }, $false)
if (-not $function) { throw 'Candidate presentation selector is missing.' }
. ([scriptblock]::Create($function.Extent.Text))
$count = 0
$legacy = Get-KomaphoneCandidatePresentation
if ($legacy.milestone -cne 'K6.5' -or $legacy.status -cne 'hosted test candidate; hosted acceptance pending' -or $legacy.guide -cne 'docs/komaphone/k65-friends.md') {
    throw 'Historical default presentation changed.'
}
$count++
$k7 = Get-KomaphoneCandidatePresentation -Milestone K7
if ($k7.milestone -cne 'K7' -or $k7.status -cne 'release hardening test candidate; K7 acceptance pending' -or $k7.guide -cne 'docs/komaphone/k7-friends.md') {
    throw 'K7 is not explicitly selected and pending.'
}
$count++
foreach ($mode in @('', 'K8', 'final release')) {
    $rejected = $false
    try { $null = Get-KomaphoneCandidatePresentation -Milestone $mode } catch { $rejected = $true }
    if (-not $rejected) { throw 'Unsupported package mode was accepted.' }
    $count++
}
$milestone = $ast.ParamBlock.Parameters | Where-Object { $_.Name.VariablePath.UserPath -eq 'Milestone' }
$profile = $ast.ParamBlock.Parameters | Where-Object { $_.Name.VariablePath.UserPath -eq 'ProfileName' }
if ($milestone.DefaultValue.SafeGetValue() -cne 'K6.5' -or $profile.DefaultValue.SafeGetValue() -cne 'k65-hosted') {
    throw 'Public default mode or stable profile changed.'
}
$count++
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '../..'))
foreach ($selection in @($legacy, $k7)) {
    $guide = [IO.File]::ReadAllText((Join-Path $root $selection.guide))
    if (-not $guide.Contains('k65-hosted') -or -not $guide.Contains('Start-Komaphone.exe') -or -not $selection.warning.Contains('not completed')) {
        throw 'Candidate guide or warning omits profile, launch, or pending acceptance.'
    }
    $count++
}
[pscustomobject]@{passed=$count;failed=0;packagingExecuted=$false;configurationRead=$false}
