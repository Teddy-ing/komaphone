$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'build-environment.ps1')
$expected = Get-K65LauncherCompilerEnvironment
$powershellExe = (Get-Process -Id $PID).Path
$cases = @(
    @{ Label='absent'; RUSTFLAGS=$null; CARGO_ENCODED_RUSTFLAGS=$null },
    @{ Label='empty'; RUSTFLAGS=''; CARGO_ENCODED_RUSTFLAGS='' },
    @{ Label='stale'; RUSTFLAGS='-C target-feature=-crt-static'; CARGO_ENCODED_RUSTFLAGS=('-C' + [char]31 + 'target-feature=-crt-static') }
)
foreach ($case in $cases) {
    Invoke-K65BuildEnvironment -Variables @{RUSTFLAGS=$case.RUSTFLAGS; CARGO_ENCODED_RUSTFLAGS=$case.CARGO_ENCODED_RUSTFLAGS} -Body {
        foreach ($failure in @($false, $true)) {
            $caught = $false
            try {
                Invoke-K65BuildEnvironment -Variables $expected -Body {
                    $child = & $powershellExe -NoProfile -Command '[ordered]@{plain=$env:RUSTFLAGS;encoded=$env:CARGO_ENCODED_RUSTFLAGS} | ConvertTo-Json -Compress'
                    if ($LASTEXITCODE -ne 0) { throw 'Child environment probe failed' }
                    $observed = $child | ConvertFrom-Json
                    if ($observed.plain -cne $expected.RUSTFLAGS -or $observed.encoded -cne $expected.CARGO_ENCODED_RUSTFLAGS) {
                        throw 'Child compiler flags did not override inherited values'
                    }
                    if ($failure) { throw 'Expected test exception' }
                }
            } catch {
                if (-not $failure -or $_.Exception.Message -ne 'Expected test exception') { throw }
                $caught = $true
            }
            if ($caught -ne $failure) { throw 'Exception propagation failed' }
            foreach ($flag in @('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS')) {
                if ((Test-Path -LiteralPath ('Env:' + $flag)) -ne ($null -ne $case[$flag])) { throw "Environment presence was not restored: $($case.Label)" }
                if ([Environment]::GetEnvironmentVariable($flag, 'Process') -cne $case[$flag]) { throw "Environment value was not restored: $($case.Label)" }
            }
        }
    }
}
'PASS: 6 inherited compiler-environment cases, child propagation and success/failure restoration'
