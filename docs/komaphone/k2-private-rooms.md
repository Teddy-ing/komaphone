# K2 private rooms: local Windows runbook

This guide covers the implemented local service, native client and their recovery
boundaries. The [status ledger](status.md) owns milestone acceptance, the final app
artifact and inspected screenshots. The retained app/service pair passed the real
native two/three-client recovery, invitation, submission and timer smoke test.

K2 uses two or three independently stored native clients on one Windows computer.
The service currently supports loopback HTTP only. It rejects non-loopback binds;
this build is not a deployment or a friend-ready release on separate machines.

## Use the retained build, or compile from source

The repository remains Rust/egui/wgpu. Machine names remain `photocraft-*`.
On this development machine, the verified executables are already retained at
`log/k2/bin/photocraft.exe` and `log/k2/bin/photocraft-sync.exe`; no rebuild is
needed. Their hashes and acceptance evidence are in [status](status.md).
To rebuild from source, from the repository root:

```powershell
. ./log/k0/env-msvc.ps1
cargo build -p photocraft -p photocraft-sync --offline
```

The local helper selects Rust/MSVC and `target/agent-root`; it is not a portable
installer. A fresh checkout uses the normal [development setup](../development.md)
and may need an initial dependency download. The following launch commands use
the retained pair. After a source build, substitute `target/agent-root/debug`
for `log/k2/bin` to run the newly compiled executables.

In a dedicated PowerShell terminal, start the service:

```powershell
$k2TrialRoot = Join-Path $env:LOCALAPPDATA 'Komaphone-K2-Try'
& ./log/k2/bin/photocraft-sync.exe --bind 127.0.0.1:43127 --data-dir (Join-Path $k2TrialRoot 'service')
```

Its readiness line is JSON containing `ready`, the actual bound `address`, and
`protocol_version`. It contains no invitation or client credential. If the port
is occupied, use another loopback port and enter that address in each client.
`--bind 127.0.0.1:0` selects a free port for automated tests. Ctrl+C gracefully
closes the listener and joins the database worker.

In a second terminal, start client A:

```powershell
$k2TrialRoot = Join-Path $env:LOCALAPPDATA 'Komaphone-K2-Try'
$env:KOMAPHONE_DATA_DIR = Join-Path $k2TrialRoot 'client-a/data'
$env:PHOTOCRAFT_CONFIG_DIR = Join-Path $k2TrialRoot 'client-a/config'
& ./log/k2/bin/photocraft.exe --new-instance
```

In a third terminal, start client B:

```powershell
$k2TrialRoot = Join-Path $env:LOCALAPPDATA 'Komaphone-K2-Try'
$env:KOMAPHONE_DATA_DIR = Join-Path $k2TrialRoot 'client-b/data'
$env:PHOTOCRAFT_CONFIG_DIR = Join-Path $k2TrialRoot 'client-b/config'
& ./log/k2/bin/photocraft.exe --new-instance
```

For three players, repeat with `client-c/data` and `client-c/config` in a fourth
terminal. Reuse those exact paths on later launches. `--new-instance` separates
windows, not databases: every test player needs a distinct data/config root.
Keep real data on a local disk outside OneDrive, network shares and the source
checkout. These environment changes affect only the launching shell and children.

The actual data override is **`KOMAPHONE_DATA_DIR`**. There is no
`--komaphone-data-dir` flag in the current desktop parser. Without the override,
permanent Komaphone data defaults to `%LOCALAPPDATA%\Komaphone`.

## Connect and exercise the room flow

1. Open **Private room** from the Komaphone workspace. Enter
   `http://127.0.0.1:43127`, choose a display name, and select **Connect** in each
   client. Each isolated root receives a stable identity and credential.
2. In client A choose **Animation** or **Exquisite Corpse**, set rotations/timer
   options, and select **Create room**. Untimed is the default. Exquisite Corpse
   explains that two players intentionally repeat the head author for the legs.
3. Select **Create private invitation**. Use **Copy room ID** and **Copy
   invitation** to transfer the two values privately to B, and C when present.
   Each joining client enters both values and selects **Join room**. The UI's
   invitation expires after one hour and permits two redemptions; **Revoke
   invitation** disables further use. The room ID alone cannot authorize a join.
4. Each player selects **Ready**. The host selects **Start game**. The service
   freezes the roster and supplies each player's assignment.
5. Select **Open assigned drawing**, draw, and wait for **Saved locally** before
   **Submit saved drawing**. Local durability, queued service work and accepted
   submission are distinct states. Repeated clicks and retries do not replace
   accepted work. Subsequent rounds receive new assignment identities.
6. Exercise **Pause**, **Resume**, **Leave room**, and **Claim host role** after
   host loss. A host may explicitly **Skip … with placeholder** or **End and
   keep incomplete game**. These operations retain labeled incomplete results.

These controls exercise K2 room authority and actual durable assignment packages.
The complete Animation reference/onion-skin/copy flow, corpse seam presentation
and polished reveals remain K3. Do not judge missing K3 presentation as an implied
K2 implementation or label this local build a complete party game release.

## Recover from interruption

During service loss, existing local drawing remains available and completed edits
continue to save. The room window shows **Service unavailable · reconnecting**
and requests waiting for confirmation. Service absence cannot produce an accepted
submission. Restart the service with the same data directory, then reopen each
client with its original data/config directories. Clients retain stable identities,
assignment position, exact pending requests and local draft revisions.

The service restores active rooms paused. Reconnected players retain their turns;
the moderator explicitly resumes. After host heartbeat expiry, another connected
member can claim moderation through a serialized revision check. Competing claims
cannot both win. Leaving a lobby removes membership and requires a new invitation
to rejoin; leaving active play preserves the frozen roster and assignment.

A timed deadline freezes the last acknowledged local revision. Preparation and
transfer use that retained revision; later local saves cannot silently change the
frozen submission. A 30-second transfer grace ends with explicit missing entries
when work has not arrived. Disconnect pauses progression/countdown. The service
does not invent a drawing for an absent participant.

**Retry pending request** resends the exact persisted operation. When the service
has permanently rejected a request, **Keep rejected work and refresh** retains a
recovery copy of the remaining batch and refreshes the room so the user can review
it. It does not delete the original drawing or pretend a rejection was accepted.
Do not erase databases or credential entries to clear an error on work you need.

## What is stored, and what the limits mean

| Store | Location and purpose | Initial bound |
|---|---|---|
| Local drawings | `drafts.sqlite3` under `KOMAPHONE_DATA_DIR`; editable saved revisions and replay | 2 GiB default; see [K1 guide](k1-local-drafts.md) |
| Room client | `rooms.sqlite3` under the same root; stable identity, authorized projection, pending/retained requests and freeze state | 512 MiB database; 384 MiB serialized pending/retained payloads; 8192 requests |
| Client secrets | Windows Credential Manager via keyring; a distinct namespace per isolated database | No plaintext fallback in SQLite, ordinary status or logs |
| Service | `service.sqlite3` under `--data-dir`; credentials' verifiers, room state, invitation counters, receipts and complete/staged assets | 2 GiB default database; `--quota-mib` configures the service |
| Room assets | Incremental immutable service BLOBs | 64 MiB per object; 512 MiB stored object bytes per room |
| Protocol | Versioned HTTP JSON and participant-specific responses | 256 KiB request; 32 KiB binary upload chunks; bounded 4.25 MiB response |

Large JSON byte arrays add overhead, so the client's serialized outbox can reach
its cap before the raw asset limit. Replay packs retain intermediate checkpoints
without consuming a top-level asset slot per edit; they still count against byte
budgets. Limits return actionable errors and preserve saved data. Nothing is
automatically pruned to make a failed submission appear successful.

Incomplete staged uploads remain under the service quota through long outages;
acknowledged chunks are not silently expired behind an exact client retry.
Every individual HTTP body has a 10-second transfer deadline. Completed objects
are hash-checked before they become reachable. The service also caps identities,
rooms, invitations, archives and retained receipts; the full operational table is
in the [service implementation guide](../../apps/komaphone-service/README.md).

Shared frame images and corpse joining strips are metadata-free sRGB PNGs. Native
checkpoints retain the original document depth, profile and pixels. Both native
export and the service convert the CPU-composited document colors through the
same CMS helper before encoding the preview; raw linear-RGB channels are not
treated as already-display-ready sRGB. A conversion error preserves the local
original and prevents false submission acceptance.

To raise the service byte quota, stop it and relaunch against the same root with,
for example, `--quota-mib 4096`. This does not change the individual object, room,
client-outbox or protocol bounds. There is no K2 quota-pruning/admin UI.

## Service backup and restore

The implemented `photocraft_sync::Store::backup(destination)` uses SQLite's online
backup API into a fresh destination, refuses overwrite, then verifies snapshots
and complete object hashes. Its restore fixture reopens the new database and
checks accepted state. This API is not yet a service CLI or native UI action;
portable game archives and the finished backup administration interface are K4.

For a manual cold backup, first stop the service with Ctrl+C and wait for the
process to exit cleanly. A running database must not be backed up by copying just
`service.sqlite3`; an interrupted process with a rollback journal must first be
reopened for SQLite recovery and then stopped cleanly. Preserve the original
directory if recovery fails. After a clean stop, a concrete copy procedure is:

```powershell
$k2TrialRoot = Join-Path $env:LOCALAPPDATA 'Komaphone-K2-Try'
$k2Source = Join-Path $k2TrialRoot 'service/service.sqlite3'
$k2BackupRoot = Join-Path $k2TrialRoot ('backups/' + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $k2BackupRoot -ErrorAction Stop | Out-Null
Copy-Item -LiteralPath $k2Source -Destination (Join-Path $k2BackupRoot 'service.sqlite3') -ErrorAction Stop
Get-FileHash -LiteralPath (Join-Path $k2BackupRoot 'service.sqlite3') -Algorithm SHA256
```

Restore into a **fresh** directory, keeping the original service root intact.
Copy the backed-up database there as `service.sqlite3`; start the service with
`--data-dir` pointing to that directory. Startup verifies the schema, SQLite
integrity, bounded room records and complete object hashes before serving.
Successful restore pauses active rooms, and the original clients reconnect using
their existing Windows credentials. Do not run the original and restored service
as competing authorities for the same room.

The cold-copy shell procedure is an operational recipe, not a recorded execution
claim; the exact tested online-backup/reopen case is identified in the ledger.
A copy on the same disk is not protection against disk loss. Protect service
backups as private room content: the service administrator can read stored artwork.
Cloning a client database is not a supported credential/device transfer workflow;
do not run cloned writers concurrently. K4 must provide portable originals/replay
archives and verify restoration into a fresh application root.

## Automation is a separate local capability

The tracked [native acceptance harness](../../scripts/komaphone/k2-native-smoke.ps1)
launches isolated service and app processes, checks real submissions/restarts and
deadline freezing, writes evidence under `log/k2`, and removes only its test
credentials. Build its cleanup helper alongside the app/service:

```powershell
cargo build -p photocraft-room-client --example cleanup_credentials --offline
& ./scripts/komaphone/k2-native-smoke.ps1 -AppExe ./target/agent-root/debug/photocraft.exe -ServiceExe ./target/agent-root/debug/photocraft-sync.exe -CredentialCleanupExe ./target/agent-root/debug/examples/cleanup_credentials.exe
```

With the retained artifact set present, `& ./scripts/komaphone/k2-native-smoke.ps1`
uses `log/k2/bin` directly. The default includes the real timed-game check;
`-SkipTimedGame` is only a diagnostic subset and cannot close the gate. The harness
uses generated drawings and independent roots, never personal projects.

Manual room use does not require an editor control listener. For an isolated
automation instance, the real desktop flags include `--control`,
`--control-token-file`, `--automation-read-root` and `--automation-write-root`:

```powershell
$k2TrialRoot = Join-Path $env:LOCALAPPDATA 'Komaphone-K2-Try'
$env:KOMAPHONE_DATA_DIR = Join-Path $k2TrialRoot 'automation-a/data'
$env:PHOTOCRAFT_CONFIG_DIR = Join-Path $k2TrialRoot 'automation-a/config'
$k2AutomationRoot = Join-Path $k2TrialRoot 'automation-a/control'
New-Item -ItemType Directory -Force -Path $k2AutomationRoot | Out-Null
& ./log/k2/bin/photocraft.exe --new-instance --control 43128 --control-token-file (Join-Path $k2AutomationRoot 'control.token') --automation-read-root $k2AutomationRoot --automation-write-root $k2AutomationRoot
```

The app creates a missing control token file. Use a distinct control port/root for
each automated instance. The [control protocol](../control-protocol.md) describes
authentication and inspection. `komaphone.room.*` commands use the engine seam;
ordinary room status omits secrets. `komaphone.room.invitation` is an intentional
secret-sharing action and must not be included in general diagnostic logs.
Never expose or reuse the editor control port as the multiplayer protocol.

## Trust, release scope and evidence

Use only numeric loopback service addresses with this K2 service. It rejects
browser-origin requests and non-loopback binds. The native transport requires
validated HTTPS outside loopback, but that does not create a TLS service listener:
the external route still needs implementation, certificates and separate-machine
verification. Do not disable certificate checks, open public access, buy hosting
or describe a deployment as ready from these local commands.

The [service guide](../../apps/komaphone-service/README.md#dependency-review--2026-11-05)
records the scoped [RustSec database](https://github.com/RustSec/advisory-db)
review at `26fd2d43f7280a1fb925a3743ec463017afe7119`: 24 exact selected/new registry
versions, 11 matching records, all within published patched ranges. This was not
a whole-lockfile audit or proof of security. Exact license manifests/hashes are
retained in `log/k2/dependency-licenses.json`; matchit requires **both MIT and
BSD-3-Clause** notices. The [client guide](../../crates/room-client/README.md)
documents the Windows Credential Manager adapter. Existing copyright, licenses
and contributor attribution remain preserved.

K3 owns full Animation/Exquisite Corpse play and reveal presentation. K4 owns
gallery, polished replay, export, portable archives, local non-destructive remix
and the separate-Windows-machine friend-ready smoke. Deep zoom is K5; shared
world remix, guided tours/export and since-you-were-away playback remain required
at K6. This runbook does not move or remove those gates.
