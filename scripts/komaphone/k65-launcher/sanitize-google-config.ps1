# Packaging-only parser. Return only minimal native public-client metadata.
# Never print provider input or parser exceptions: downloaded fields can be private.
function Read-K65GoogleDesktopConfiguration {
    param([Parameter(Mandatory)][string]$Path)
    $stream = $null
    try {
        $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read, [IO.FileShare]::Read)
        if ($stream.Length -lt 1 -or $stream.Length -gt 32768) { throw 'bounds' }
        $buffer = [byte[]]::new(32769)
        $count = 0
        while ($count -lt $buffer.Length) {
            $read = $stream.Read($buffer, $count, $buffer.Length - $count)
            if ($read -eq 0) { break }
            $count += $read
        }
        if ($count -lt 1 -or $count -gt 32768) { throw 'bounds' }
        $utf8 = [Text.UTF8Encoding]::new($false, $true)
        $document = $utf8.GetString($buffer, 0, $count) | ConvertFrom-Json -AsHashtable -Depth 16 -ErrorAction Stop
        if ($document -isnot [Collections.IDictionary] -or $document['installed'] -isnot [Collections.IDictionary]) { throw 'shape' }
        $installed = $document['installed']
        $clientId = '981926394354-07f4s3dss4vgp7mteh0qe8e6dhk3s99o.apps.googleusercontent.com'
        if ($installed['client_id'] -isnot [string] -or $installed['client_id'] -cne $clientId) { throw 'client' }
        $secret = $installed['client_secret']
        if ($secret -isnot [string] -or $secret.Length -lt 1 -or $secret.Length -gt 256 -or $secret -cmatch '[^\x21-\x7e]') { throw 'secret' }
        return ([ordered]@{installed=[ordered]@{client_id=$clientId;client_secret=$secret}} | ConvertTo-Json -Compress -Depth 3)
    } catch {
        throw 'Google Desktop configuration is missing or invalid. Use the original installed-client JSON for the pinned Komaphone client (maximum 32 KiB).'
    } finally {
        if ($null -ne $stream) { $stream.Dispose() }
    }
}
