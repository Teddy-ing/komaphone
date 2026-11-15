# Cargo gives CARGO_ENCODED_RUSTFLAGS precedence even when it is present but empty.
function Get-K65LauncherCompilerEnvironment {
    return @{
        RUSTFLAGS = '-C target-feature=+crt-static'
        CARGO_ENCODED_RUSTFLAGS = ('-C' + [char]31 + 'target-feature=+crt-static')
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
