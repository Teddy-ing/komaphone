$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'build-environment.ps1')

$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ('komaphone-cargo-fallback-test-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($tempRoot) | Out-Null
$passed = 0

try {
    # 1. Workspace target/k0-tools present -> selected with CARGO_HOME/RUSTUP_HOME.
    $wsWithTools = Join-Path $tempRoot 'ws-with-tools'
    $k0CargoBin = Join-Path $wsWithTools 'target/k0-tools/cargo/bin'
    $k0Rustup = Join-Path $wsWithTools 'target/k0-tools/rustup'
    [IO.Directory]::CreateDirectory($k0CargoBin) | Out-Null
    [IO.Directory]::CreateDirectory($k0Rustup) | Out-Null
    $k0CargoExe = Join-Path $k0CargoBin 'cargo.exe'
    [IO.File]::WriteAllText($k0CargoExe, '')

    $resolved = Resolve-K65CargoEnvironment -WorkspaceRoot $wsWithTools
    if (-not $resolved.UsesWorkspaceTools -or
        -not $resolved.CargoExe.Equals([IO.Path]::GetFullPath($k0CargoExe), [StringComparison]::OrdinalIgnoreCase) -or
        -not $resolved.Variables.ContainsKey('CARGO_HOME') -or
        -not $resolved.Variables.ContainsKey('RUSTUP_HOME')) {
        throw 'Expected workspace target/k0-tools selection when present.'
    }
    $passed++

    # 2. Workspace target/k0-tools absent -> falls back to PATH cargo.exe without overriding CARGO_HOME/RUSTUP_HOME.
    $wsNoTools = Join-Path $tempRoot 'ws-no-tools'
    $sysBin = Join-Path $tempRoot 'sys-bin'
    [IO.Directory]::CreateDirectory($wsNoTools) | Out-Null
    [IO.Directory]::CreateDirectory($sysBin) | Out-Null
    $sysCargoExe = Join-Path $sysBin 'cargo.exe'
    [IO.File]::WriteAllText($sysCargoExe, '')

    Invoke-K65BuildEnvironment -Variables @{ PATH = $sysBin; CARGO_HOME = $null; USERPROFILE = (Join-Path $tempRoot 'empty-profile') } -Body {
        $fallback = Resolve-K65CargoEnvironment -WorkspaceRoot $wsNoTools
        if ($fallback.UsesWorkspaceTools -or
            -not $fallback.CargoExe.Equals([IO.Path]::GetFullPath($sysCargoExe), [StringComparison]::OrdinalIgnoreCase) -or
            $fallback.Variables.ContainsKey('CARGO_HOME') -or
            $fallback.Variables.ContainsKey('RUSTUP_HOME')) {
            throw 'Expected PATH cargo.exe fallback without workspace CARGO_HOME/RUSTUP_HOME.'
        }
    }
    $passed++

    # 3. Workspace target/k0-tools absent and not on PATH -> falls back to standard ~/.cargo/bin/cargo.exe.
    $homeProfile = Join-Path $tempRoot 'home-profile'
    $homeCargoBin = Join-Path $homeProfile '.cargo/bin'
    [IO.Directory]::CreateDirectory($homeCargoBin) | Out-Null
    $homeCargoExe = Join-Path $homeCargoBin 'cargo.exe'
    [IO.File]::WriteAllText($homeCargoExe, '')

    Invoke-K65BuildEnvironment -Variables @{ PATH = (Join-Path $tempRoot 'no-bin'); CARGO_HOME = $null; USERPROFILE = $homeProfile } -Body {
        $fallbackHome = Resolve-K65CargoEnvironment -WorkspaceRoot $wsNoTools
        if ($fallbackHome.UsesWorkspaceTools -or
            -not $fallbackHome.CargoExe.Equals([IO.Path]::GetFullPath($homeCargoExe), [StringComparison]::OrdinalIgnoreCase) -or
            $fallbackHome.Variables.ContainsKey('CARGO_HOME') -or
            $fallbackHome.Variables.ContainsKey('RUSTUP_HOME')) {
            throw 'Expected ~/.cargo/bin/cargo.exe fallback.'
        }
    }
    $passed++

    # 4. Explicit -CargoExe outside target/k0-tools -> uses explicit cargo.exe without overriding CARGO_HOME/RUSTUP_HOME.
    $explicit = Resolve-K65CargoEnvironment -WorkspaceRoot $wsWithTools -CargoExe $sysCargoExe
    if ($explicit.UsesWorkspaceTools -or
        -not $explicit.CargoExe.Equals([IO.Path]::GetFullPath($sysCargoExe), [StringComparison]::OrdinalIgnoreCase) -or
        $explicit.Variables.ContainsKey('CARGO_HOME') -or
        $explicit.Variables.ContainsKey('RUSTUP_HOME')) {
        throw 'Explicit external -CargoExe should not force workspace k0-tools environment.'
    }
    $passed++

    # 5. Missing cargo.exe everywhere -> fails with actionable message.
    $threw = $false
    Invoke-K65BuildEnvironment -Variables @{ PATH = (Join-Path $tempRoot 'no-bin'); CARGO_HOME = $null; USERPROFILE = (Join-Path $tempRoot 'empty-profile') } -Body {
        try {
            $null = Resolve-K65CargoEnvironment -WorkspaceRoot $wsNoTools
        } catch {
            if ($_.Exception.Message -match 'Could not locate cargo\.exe') {
                $script:threw = $true
            } else {
                throw
            }
        }
    }
    if (-not $threw) { throw 'Expected clear error when cargo.exe is unavailable.' }
    $passed++

    Write-Output "PASS: $passed Cargo resolution/fallback checks."
} finally {
    if (Test-Path -LiteralPath $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
    }
}
