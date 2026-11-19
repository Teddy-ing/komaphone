# Synthetic packaging tests; never read an actual downloaded OAuth configuration.
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'sanitize-google-config.ps1')
$directory = Join-Path ([IO.Path]::GetTempPath()) ('komaphone-google-config-test-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($directory) | Out-Null
$file = Join-Path $directory 'synthetic.json'
$clientId = '981926394354-07f4s3dss4vgp7mteh0qe8e6dhk3s99o.apps.googleusercontent.com'
$count = 0
function Assert-Rejected([byte[]]$Bytes) {
    [IO.File]::WriteAllBytes($file, $Bytes)
    $message = $null
    try { $null = Read-K65GoogleDesktopConfiguration $file } catch { $message = $_.Exception.Message }
    if (-not $message -or $message.Contains('synthetic-value')) { throw 'Invalid configuration was accepted or echoed into diagnostics.' }
}
function Json-Bytes($Value) { return [Text.Encoding]::UTF8.GetBytes(($Value | ConvertTo-Json -Compress -Depth 5)) }
try {
    $input = @{installed=@{client_id=$clientId;client_secret='synthetic-value&=only';token_uri='https://untrusted.invalid';project_id='excluded'};unrelated='excluded'}
    [IO.File]::WriteAllBytes($file, (Json-Bytes $input))
    $sanitized = Read-K65GoogleDesktopConfiguration $file
    if ($PSVersionTable.PSVersion.Major -ge 7) {
        $parsed = $sanitized | ConvertFrom-Json -AsHashtable
    } else {
        Add-Type -AssemblyName System.Web.Extensions
        $parsed = [Web.Script.Serialization.JavaScriptSerializer]::new().DeserializeObject($sanitized)
    }
    if ($parsed.Count -ne 1 -or $parsed.installed.Count -ne 2 -or $parsed.installed.client_id -cne $clientId -or $parsed.installed.client_secret -cne 'synthetic-value&=only') {
        throw 'Sanitized output did not retain exactly the pinned public-client fields.'
    }
    $count++
    foreach ($invalid in @(
        @{web=@{client_id=$clientId;client_secret='synthetic-value'}},
        @{installed=@{client_id='wrong';client_secret='synthetic-value'}},
        @{installed=@{client_id=$clientId;client_secret=''}},
        @{installed=@{client_id=$clientId;client_secret=('x'*257)}},
        @{installed=@{client_id=$clientId;client_secret="synthetic-value`n"}},
        @{installed=@{client_id=$clientId;client_secret='synthetic-value é'}},
        @{installed=@{client_id=$clientId;client_secret=@('synthetic-value')}}
    )) { Assert-Rejected (Json-Bytes $invalid); $count++ }
    foreach ($bytes in @([byte[]]@(), [Text.Encoding]::UTF8.GetBytes('synthetic-value invalid JSON'), [byte[]]@(255,254), [byte[]]::new(32769))) {
        Assert-Rejected $bytes
        $count++
    }
    Write-Output "PASS: $count synthetic Google Desktop configuration checks; no provider data used."
} finally {
    if ([IO.File]::Exists($file)) { [IO.File]::Delete($file) }
    [IO.Directory]::Delete($directory, $false)
}
