# K4 party candidate acceptance

This is the procedure for the first Komaphone party candidate. The authoritative
result and exact current build are in [status](status.md). The real two-PC home-LAN
test completed both games and the other recorded checks. On 2026-11-07 the owner
accepted the unresolved laptop startup/export-path limitation as non-blocking
for K4. K4 is complete with known issue K4-LAUNCH-01; its latest failed observation
is retained and diagnosis/retest is deferred to K7. A local harness pass does not
substitute for separate-machine evidence or prove that laptop issue resolved.

## Try the local candidate

Use the tested Windows app and service paths recorded in status. Keep a stable
local data directory outside OneDrive for ordinary use. Start the service using
the [private service runbook](k4-service.md), launch two app instances with separate
data directories, connect their profiles, and invite the second player to a room.

1. Choose Animation or Exquisite Corpse, have both players select Ready, then Start.
   Open each assigned drawing, draw with the mouse, and Submit until reveal.
2. At reveal, select Save to gallery. Wait until the complete original has been
   saved locally. Open Gallery, select the game, and choose Drawing replay for
   per-frame/per-part history. Finished animation has a separate play control,
   frame selection, speed, and loop preference.
3. Enter a full output path outside the verified `candidate` program folder,
   using an existing parent folder and a new filename or sequence subfolder.
   Export an editable archive, PNG,
   PNG frame sequence, or animated GIF. A creature's PNG and sequence contain the
   assembled result. GIF has palette/binary-alpha and 10 ms timing limitations
   shown in the UI; editable originals and PNG retain their source fidelity.
4. Select Remix full animation or Remix creature to make a new editable local
   project. Remix selected still explicitly extracts one frame/part. Original
   history and source credits remain accessible from provenance.
5. Close and restart the app. Reopen the original and remix from Gallery. Use
   Import & backups to import an archive into a fresh data directory or back up
   all local drafts and gallery originals. Restore targets a new data directory;
   switch to that directory explicitly after closing the app. Service backup
   and credentials are handled separately by the service runbook.

Archive import and backup restore wait until the active game finishes. Backups
remain available during play; imported copies retain assignment identities, so
copying an archive cannot reveal previous assignments or enable an early remix.

## Local candidate limits

These bounded operational defaults preserve baseline 1.1:

| Area | Current bound |
|---|---|
| K4-LAUNCH-01, owner accepted | Use full export paths outside the program folder. The corrected launcher passed relative-export/restart locally; the latest laptop no-start report remains unresolved and is tracked for K7. |
| Gallery animation playback/export | Local projects: 1–36 frames; party animations: up to 12 frames per chain (three players × four rotations), with 36 submissions across the whole game; 1–24 fps |
| Animated GIF | 33,554,432 cumulative decoded pixels across its frames; this accommodates the maximum party animation, while large local timelines may need PNG frames or an archive |
| Portable originals | 64 MiB per asset, 512 MiB per bundle, 16 MiB archive manifest |
| Gallery media and restored native-document decoding | 8192 pixels per side, 16,777,216 total pixels, 256 MiB decoded total per document |
| Local storage | Independent 2 GiB draft and 2 GiB gallery stores; the UI shows their combined 4 GiB allowance, not one shared pool |

Gallery quota or corruption failures report an error and preserve retained
originals; they do not silently prune work or acknowledge a failed save. The
Advanced editor retains its larger-document engine support. Gallery operations
reject unsupported media without resizing or changing those originals. Limits
above come from party contracts, gallery/media load options and the default local
store configuration; they do not expand or change the agreed product baseline.

## Reproduce local native checks

Builds must already have passed their crate tests and mandatory checks. Use the
coordinator's current app binary, the retained service binary, and the existing
room-client credential-cleanup helper. The script launches isolated native windows,
uses authenticated loopback automation, and drives the rendered control rectangles.
Gallery export/import/backup/restore retain the existing ambient-file automation
denial. With `-ScopedFilesystem`, the harness uses `app.galleryFile` through each
client's explicitly granted disposable test root. It verifies the visible file
controls and their values, then polls actual completion and checks the output;
these are scoped command operations, not native file-button clicks. The same
source/phase checks, codecs and recovery logic run behind the capability adapter.
With `-NativeFilesystemClicks`, the harness pauses at those actions for
an authorized operator using the observed native desktop UI. The operator checks
the target process, selected original, visible path and fresh destination before
clicking, records the native observation, then acknowledges that exact request.
An acknowledgement alone is not success: the harness still requires the app's
completion result, output files/hashes and fresh-directory reopen checks.
Its process directories, screenshots, exports, hashes, and JSON ledger stay under
`log/k4/native-gallery-<unique-id>/`. It never uses the normal app profile.

```powershell
./scripts/komaphone/k4-native-smoke.ps1 `
  -AppExe ./log/k4/bin/photocraft-scoped-final.exe `
  -ServiceExe ./log/k4/bin/photocraft-sync-phase-verified.exe `
  -CredentialCleanupExe ./log/k4/bin/cleanup_credentials-phase-verified.exe `
  -ScopedFilesystem
```

The script checks both complete two-player games, durable Save to gallery,
rename/favorite/filter, distinct drawing and animation clocks, replay scrubbing,
PNG/GIF dimensions and frame counts, sequence output, portable archive export,
editable full remix, byte-identical original archive/PNG after remix edits,
restart recovery, prior-game download after Next Game, fresh unpaired archive
import, and backup restoration opening retained games and remix drafts. Synthetic
mouse strokes and generated test images contain no personal photographs.

Open and inspect the screenshots at 1280×720 and 1920×1080, including the actual
playback/export/remix controls, after running. A JSON pass without inspected PNGs
does not satisfy the visual gate. A diagnostic run is not final acceptance.
Retain failed runs and their error captures; fix the cause and rerun against the
final build rather than editing an existing ledger into a pass.
Record which actions used native desktop interaction or capability-scoped I/O.
The scoped mode is unattended native-app/control-channel acceptance; the click
mode requires an observed native operator for file actions. Never
disable the automation file-access policy to make the harness pass.

## Evidence still required beyond that script

| Requirement | Evidence source |
|---|---|
| Canonical replay endpoint; erase, undo/redo, fills, layer edits, transform/import/checkpoint history | Engine/media tests and deterministic CPU-render comparison |
| GIF order, loop, approximate duration, transparency and color limits | Decoder-backed media tests; inspect native exported GIF |
| Hidden-source refusal, credentials and historical frozen-member access | Engine/client privacy tests and service archive tests |
| Remix IDs, credits/provenance, full animation timing, explicit still extraction, either-side deletion independence | Engine/media/storage tests and native original/archive comparison |
| Incomplete and corrupt archives, crash/restart, quota/full-disk, cancellation and no-clobber failure paths | Independent local-store, media, client and service tests |
| Existing editor behavior, layering, WASM, command panic resilience and parity | Repository mandatory checks recorded in status |
| Private service TLS trust, hostname rejection, stalled-handshake bounds, safe backup/restore | Real loopback HTTPS service tests and CLI tests |
| Friend readiness on the intended private route | Separate-machine run below; cannot be simulated by local app instances |

## Separate-machine gate

Use the concrete existing-Windows-host private HTTPS route in
[k4-service.md](k4-service.md). Record host/client Windows versions, exact app and
service hashes, the private endpoint, certificate fingerprint, invited participant
count, and who authorized the machine/network changes. Never retain private keys,
bearer credentials, or invitation capabilities in screenshots or public logs.

On two actual Windows machines, invite a second player, finish one game of each
mode, disconnect and reconnect a client, restart the service, reopen the saved
games, replay and export them, make an independent remix, and import an archive
into a fresh client data directory. Record observed results and screenshots.

If those machines or the authorized network route are unavailable, the next
action is to obtain that access and execute this prepared test. Keep the result
as **local-only candidate; external gate pending**. Stop at K4 for this run; do
not claim the milestone complete or move scope to shared-world work.
