# Komaphone private service and backup

K4 adds native HTTPS, authenticated access to older revealed games, and verified
service backup/restore. The separate-Windows-machine acceptance result belongs in
[status](status.md). Loopback TLS tests alone do not establish friend readiness.
No public endpoint, hosting subscription, firewall change or deployment is implied
by this runbook. Use the final tested binaries identified in status.

## Local development

The service stays independent of the editor's automation port. Plain HTTP accepts
only numeric loopback interfaces; the previous launch remains supported:

```powershell
$serviceRoot = Join-Path $env:LOCALAPPDATA 'Komaphone/service'
& ./log/k4/bin/photocraft-sync-phase-verified.exe --bind 127.0.0.1:43127 --data-dir $serviceRoot
```

Readiness JSON identifies the actual address, protocol and `tls` status. Ctrl+C
requests graceful shutdown. Real data belongs on a local NTFS volume outside
OneDrive, network shares and the source checkout. Keep client data directories
independent and stable across restarts.

## Prepared private-network route

The concrete zero-subscription candidate uses two separate Windows client machines
on an already authorized private LAN or VPN and native HTTPS on TCP 43127. The
service can run on either client machine or a third existing Windows host; a third
machine is not required. Bind the service to its private interface, restrict the
firewall to the friends' private addresses, and keep the editor control channel
loopback-only. A public cloud provider is unnecessary for this acceptance route.
The service machine must remain running while either client needs synchronization.

Before actually using this route, record the service private address, both client
addresses, authorization to run a listener/firewall rule on that machine, and
the participants available to exercise both games. These external facts cannot
be replaced by three local processes. Do not open router port forwarding or a
public listener as a shortcut.

Create a fresh private certificate for the chosen service address using an
installed OpenSSL. The example address below must be replaced with the actual
authorized address. The subject alternative name is the identity checked by
clients. This uses OpenSSL's documented certificate-generation and extension
options ([req documentation](https://docs.openssl.org/3.5/man1/openssl-req/)).

```powershell
$serviceAddress = '192.168.1.10' # replace with the authorized private address
$tlsRoot = Join-Path $env:LOCALAPPDATA 'Komaphone/private-tls'
New-Item -ItemType Directory -Force $tlsRoot | Out-Null
$openssl = 'C:/Program Files/Git/mingw64/bin/openssl.exe' # or installed native OpenSSL
& $openssl req -x509 -newkey rsa:3072 -nodes -days 365 `
  -subj '/CN=Komaphone private service' `
  -addext "subjectAltName=IP:$serviceAddress" `
  -addext 'basicConstraints=critical,CA:FALSE' `
  -addext 'extendedKeyUsage=serverAuth' `
  -keyout (Join-Path $tlsRoot 'service-key.pem') `
  -out (Join-Path $tlsRoot 'service.pem')
if ($LASTEXITCODE -ne 0) { throw 'Certificate creation failed' }
& $openssl x509 -in (Join-Path $tlsRoot 'service.pem') -noout -dates -fingerprint -sha256
```

The server key is deliberately unencrypted for unattended native loading; protect
its directory with the service user's Windows file permissions and keep it out
of shared folders. Distribute only `service.pem` privately to each friend, compare
its SHA256 fingerprint through an existing trusted channel, and retain the key
only on the service machine. Never deploy the deliberately public test key under
`apps/komaphone-service/tests/fixtures/tls`. Renew the private certificate before
expiry and distribute the new certificate before changing the service.

Start HTTPS on that private interface after applicable authorization:

```powershell
& ./log/k4/bin/photocraft-sync-phase-verified.exe --bind "${serviceAddress}:43127" `
  --data-dir $serviceRoot --tls-cert (Join-Path $tlsRoot 'service.pem') `
  --tls-key (Join-Path $tlsRoot 'service-key.pem')
```

Both PEM inputs are mandatory for non-loopback binding and each is capped at
64 KiB; chains are limited to eight certificates. Invalid PEM or a mismatched
key fails before opening the listener. TLS handshakes are capped at 32 concurrent
attempts with a three-second deadline each; plaintext requests on that port fail.
Bearer credentials and invitations still authorize room membership separately.

An administrator can review this firewall command with `-WhatIf` first. Substitute
the actual private client addresses; apply it only after the administrator's
authorization. The rule binds the program, port, local address, remote addresses
and Windows Private profile ([Microsoft reference](https://learn.microsoft.com/en-us/powershell/module/netsecurity/new-netfirewallrule?view=windowsserver2025-ps)).

```powershell
$serviceProgram = (Resolve-Path ./log/k4/bin/photocraft-sync-phase-verified.exe).Path
$friendAddresses = @('192.168.1.11', '192.168.1.12') # authorized client addresses
New-NetFirewallRule -DisplayName 'Komaphone private service' -Direction Inbound `
  -Action Allow -Protocol TCP -LocalAddress $serviceAddress -LocalPort 43127 `
  -RemoteAddress $friendAddresses -Program $serviceProgram -Profile Private -WhatIf
```

On each client set `KOMAPHONE_TRUSTED_CERT` to the received certificate's absolute
path before launch. The application adds this certificate to normal TLS trust;
it does not bypass hostname, signature or expiration checks. Set an independent
`KOMAPHONE_DATA_DIR` and `PHOTOCRAFT_CONFIG_DIR` if testing alongside normal use.
Open Private room and connect to `https://192.168.1.10:43127` using the actual
service address. Wrong trust files, wrong hostnames and expired certificates must
remain errors. No global Windows trust-store modification is required.

## Historical game authorization

History lists at most 16 games per page, with a revision cursor, original authors,
mode and complete/incomplete status. It includes the currently revealed game and
up to 256 prior terminal games per room. `Game`, `GameFetch` and `GameNotes` check
membership in that game's frozen roster for every request. New lobby members or
waiting participants cannot read an earlier game. A former player can still
access a game already revealed to them after leaving a later lobby. Hidden active
games never become history or remix sources. Notes require an explicit request.

These read operations do not add mutation receipts or keep a current game alive
through heartbeat side effects. Historical assets remain immutable originals.
Asset chunks carry the revealed room revision as an indexed lookup hint. A
terminal heartbeat may increase the later archived revision; lookup still checks
the exact game, frozen member and asset, and never grants access from the hint.
PNG thumbnails are derivatives; editable checkpoints and replay dependencies
must also finish downloading before a local gallery copy is described as saved.

## Backup and fresh restore

Use a second disk for protection from drive loss. A backup on the same drive is
only a local recovery copy. Service backups contain room membership/credential
verifiers and all private drawings; protect them like the live data directory.
Portable game exports are the appropriate way to share a selected editable game.

```powershell
$backupFile = Join-Path 'E:/Komaphone-backups' 'service-2026-11-05.sqlite3'
& ./log/k4/bin/photocraft-sync-phase-verified.exe --data-dir $serviceRoot --backup-to $backupFile
if ($LASTEXITCODE -ne 0) { throw 'Backup failed; original service is retained' }
```

The command uses SQLite's consistent backup API even when the service is running.
It validates the schema, integrity, complete object hashes, current rooms and
archive snapshots. It publishes only a verified file, with atomic no-clobber
creation on the destination NTFS volume. Existing destinations are refused. A
quota/disk/validation failure never replaces the source or reports success.
Keep normal free space for the SQLite journal, backup staging file and backup.

Restore while the old service is stopped, into a new directory:

```powershell
$restoredRoot = Join-Path $env:LOCALAPPDATA 'Komaphone/service-restored'
& ./log/k4/bin/photocraft-sync-phase-verified.exe --data-dir $restoredRoot --restore-from $backupFile
if ($LASTEXITCODE -ne 0) { throw 'Restore failed; keep the original and backup' }
& ./log/k4/bin/photocraft-sync-phase-verified.exe --bind 127.0.0.1:43127 --data-dir $restoredRoot
```

First verify the restored result locally. Use the approved private HTTPS arguments
when returning it to the private route. The original directory and backup remain
intact. Restoring service authority preserves its participant IDs and credentials;
never run original and restored authorities concurrently on the same advertised
endpoint. A client's fresh installation instead imports portable games/projects
and creates a fresh local device identity; copying a live client database is not
the supported clone procedure.

Limits remain 2 GiB default service quota (`--quota-mib`), 256 identities, 128 rooms,
128 invitations per room, 65,536 mutation receipts, 256 archived games per room,
64 MiB per immutable object and 512 MiB object bytes per room. Growth fails with an
actionable error while retaining accepted work. There is no automatic pruning.
Back up before changing limits or software versions; incompatible schemas fail
without upgrading or replacing the original file.

## Acceptance record

For separate-machine acceptance record both Windows versions, build hashes,
service private interface and TLS certificate fingerprint (never private keys,
bearers or invitation tokens). Exercise invitation, one complete Animation and
one complete Exquisite Corpse game, client disconnect/reconnect, service restart,
gallery reopen, replay, exports, remix, and archive import into a fresh client root.
Retain screenshots and exact results. Record incomplete observations as pending.

The external inputs required to execute that prepared test are:

- Access to two separate Windows machines and participants able to operate them;
  multiple processes or profiles on one machine do not satisfy this gate.
- Their actual private addresses and an existing mutually reachable private LAN
  or VPN route. One chosen machine must host the service for the test.
- Applicable authorization to run that private listener and, if needed, apply
  the narrow Windows firewall rule. No public hosting purchase or port forwarding
  is needed for this candidate route.
- A private way to deliver the tested binaries and server certificate and compare
  its fingerprint. Each client creates its own profile and stable data directory.

Do not call the party build friend-ready or K4 complete until the resulting
two-machine record passes. Loopback TLS, local game simulations, and prepared
commands support the test but cannot establish this external result.

Service checks use `target/agent-k4-review` and are documented in status:

```powershell
cargo test -p photocraft-sync --offline --no-fail-fast
cargo clippy -p photocraft-sync --all-targets --offline -- -D warnings
```

The native TLS adapter reuses pinned tokio-rustls 0.26.6 and rustls 0.23.46,
already in the lockfile, with an explicit AWS-LC provider to avoid ambiguous
process-global selection. The upstream APIs and MIT OR Apache-2.0 license were
checked against exact cached sources and [tokio-rustls documentation](https://docs.rs/tokio-rustls/0.26.6/tokio_rustls/),
[rustls configuration](https://docs.rs/rustls/0.23.46/rustls/struct.ConfigBuilder.html),
and the [upstream license](https://github.com/rustls/tokio-rustls/blob/main/LICENSE-MIT).
