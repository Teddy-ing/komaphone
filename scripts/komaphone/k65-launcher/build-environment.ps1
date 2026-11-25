# Cargo gives CARGO_ENCODED_RUSTFLAGS precedence even when it is present but empty.
function Get-K65LauncherCompilerEnvironment {
    return @{
        RUSTFLAGS = '-C target-feature=+crt-static'
        CARGO_ENCODED_RUSTFLAGS = ('-C' + [char]31 + 'target-feature=+crt-static')
    }
}

function Resolve-K65CargoEnvironment {
    param(
        [Parameter(Mandatory)] [string] $WorkspaceRoot,
        [string] $CargoExe
    )
    $toolCargo = Join-Path $WorkspaceRoot 'target/k0-tools/cargo'
    $toolRustup = Join-Path $WorkspaceRoot 'target/k0-tools/rustup'
    $toolCargoExe = Join-Path $toolCargo 'bin/cargo.exe'
    $useWorkspaceTools = $false
    $resolvedCargo = $null

    if (-not [string]::IsNullOrWhiteSpace($CargoExe)) {
        $resolvedCargo = [IO.Path]::GetFullPath($CargoExe)
        if ($resolvedCargo.Equals([IO.Path]::GetFullPath($toolCargoExe), [StringComparison]::OrdinalIgnoreCase) -and
            (Test-Path -LiteralPath $toolCargo -PathType Container) -and
            (Test-Path -LiteralPath $toolRustup -PathType Container)) {
            $useWorkspaceTools = $true
        }
    } elseif ((Test-Path -LiteralPath $toolCargoExe -PathType Leaf) -and
              (Test-Path -LiteralPath $toolRustup -PathType Container)) {
        $resolvedCargo = [IO.Path]::GetFullPath($toolCargoExe)
        $useWorkspaceTools = $true
    } else {
        $fromPath = Get-Command 'cargo.exe' -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
        if ($fromPath) {
            $resolvedCargo = $fromPath.Source
        } else {
            $cargoHome = [Environment]::GetEnvironmentVariable('CARGO_HOME')
            $userProfile = [Environment]::GetEnvironmentVariable('USERPROFILE')
            $defaultCargo = if (-not [string]::IsNullOrWhiteSpace($cargoHome)) {
                Join-Path $cargoHome 'bin/cargo.exe'
            } elseif (-not [string]::IsNullOrWhiteSpace($userProfile)) {
                Join-Path $userProfile '.cargo/bin/cargo.exe'
            } else {
                $null
            }
            if ($defaultCargo -and (Test-Path -LiteralPath $defaultCargo -PathType Leaf)) {
                $resolvedCargo = [IO.Path]::GetFullPath($defaultCargo)
            }
        }
    }

    if (-not $resolvedCargo -or -not (Test-Path -LiteralPath $resolvedCargo -PathType Leaf)) {
        throw 'Could not locate cargo.exe (checked target/k0-tools/cargo/bin, PATH, and standard ~/.cargo/bin). Install Rust (rustup) or pass -CargoExe.'
    }

    $cargoBin = [IO.Path]::GetDirectoryName($resolvedCargo)
    $currentPath = [Environment]::GetEnvironmentVariable('PATH')
    $vars = @{
        PATH = if ([string]::IsNullOrEmpty($currentPath)) { $cargoBin } else { $cargoBin + [IO.Path]::PathSeparator + $currentPath }
    }
    if ($useWorkspaceTools) {
        $vars.CARGO_HOME = [IO.Path]::GetFullPath($toolCargo)
        $vars.RUSTUP_HOME = [IO.Path]::GetFullPath($toolRustup)
    }
    return [pscustomobject]@{
        CargoExe = $resolvedCargo
        UsesWorkspaceTools = $useWorkspaceTools
        Variables = $vars
    }
}

function Invoke-K65BuildEnvironment {
    param([Parameter(Mandatory)] [hashtable] $Variables,
          [Parameter(Mandatory)] [scriptblock] $Body)
    $buildEnvironmentSaved = @{}
    try {
        foreach ($buildEnvironmentName in $Variables.Keys) {
            $buildEnvironmentPath = 'Env:' + $buildEnvironmentName
            $buildEnvironmentSaved[$buildEnvironmentName] = @{
                Present = (Test-Path -LiteralPath $buildEnvironmentPath)
                Value = [Environment]::GetEnvironmentVariable($buildEnvironmentName, 'Process')
            }
            if ($null -eq $Variables[$buildEnvironmentName]) {
                Remove-Item -LiteralPath $buildEnvironmentPath -ErrorAction SilentlyContinue
            } else {
                [Environment]::SetEnvironmentVariable($buildEnvironmentName, $Variables[$buildEnvironmentName], 'Process')
            }
        }
        & $Body
    } finally {
        foreach ($buildEnvironmentName in $buildEnvironmentSaved.Keys) {
            $buildEnvironmentPrevious = $buildEnvironmentSaved[$buildEnvironmentName]
            if ($buildEnvironmentPrevious.Present) {
                [Environment]::SetEnvironmentVariable($buildEnvironmentName, $buildEnvironmentPrevious.Value, 'Process')
            } else {
                # In current PowerShell/.NET, SetEnvironmentVariable(name, $null)
                # creates an empty value. Remove through the provider explicitly.
                Remove-Item -LiteralPath ('Env:' + $buildEnvironmentName) -ErrorAction SilentlyContinue
            }
        }
    }
}
