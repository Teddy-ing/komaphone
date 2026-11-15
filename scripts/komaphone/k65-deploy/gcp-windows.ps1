# Run the tested Bash resource plan with the installed Windows Google Cloud SDK.
# Apply is only for the already approved inventory after authentication gates pass.
[CmdletBinding()]
param(
    [ValidateSet('Plan', 'Apply')]
    [string]$Mode = 'Plan'
)
$ErrorActionPreference = 'Stop'
$bash = Join-Path $env:ProgramFiles 'Git\bin\bash.exe'
if (-not (Test-Path -LiteralPath $bash -PathType Leaf)) {
    throw 'Git for Windows Bash is required. Use the documented Cloud Shell route instead.'
}
$sdkCommand = Get-Command gcloud.ps1 -CommandType ExternalScript -ErrorAction Stop
$sdkBin = Split-Path -Parent $sdkCommand.Source
$sdkRoot = Split-Path -Parent $sdkBin
$python = Join-Path $sdkRoot 'platform\bundledpython\python.exe'
if (-not (Test-Path -LiteralPath $python -PathType Leaf)) {
    throw 'The installed Google Cloud SDK bundled Python is missing; inspect the installation.'
}
$resourceScript = Join-Path $PSScriptRoot 'gcp-resources.sh'
$savedPath = $env:PATH
$savedPython = $env:CLOUDSDK_PYTHON
$savedApproval = $env:KOMAPHONE_APPROVED_PROJECT
try {
    # Child-process settings only; never change gcloud config/account/project defaults.
    $env:PATH = $sdkBin + [IO.Path]::PathSeparator + $savedPath
    $env:CLOUDSDK_PYTHON = $python.Replace('\', '/')
    $argument = '--plan'
    if ($Mode -eq 'Apply') {
        $env:KOMAPHONE_APPROVED_PROJECT = 'komaphone'
        $argument = '--apply'
    }
    # Pass individual arguments, not shell-built command text. Skip Bash profiles.
    & $bash --noprofile --norc $resourceScript.Replace('\', '/') $argument
    if ($LASTEXITCODE -ne 0) {
        throw "Resource $Mode stopped with exit code $LASTEXITCODE. Preserve any partial resources and inspect before resuming."
    }
} finally {
    $env:PATH = $savedPath
    $env:CLOUDSDK_PYTHON = $savedPython
    $env:KOMAPHONE_APPROVED_PROJECT = $savedApproval
}
