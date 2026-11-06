# Komaphone party modes

K3 implementation runbook. K3 acceptance and mandatory checks passed, including four native games and all nine release quick benchmarks. Consult [status](status.md) for the verified artifact pair and check ledger. This is a Windows development workflow using the private loopback service. Separate-machine release verification belongs to K4. The [K2 runbook](k2-private-rooms.md) explains isolated client/service roots and invitations.

## Play

The K3 retained binaries are `log/k3/bin/photocraft.exe` and `log/k3/bin/photocraft-sync.exe`; their verified hashes are in status. Start the service from a separate PowerShell terminal at the repository root:

```powershell
$k3TrialRoot = Join-Path $env:LOCALAPPDATA 'Komaphone-K3-Try'
& ./log/k3/bin/photocraft-sync.exe --bind 127.0.0.1:43127 --data-dir (Join-Path $k3TrialRoot 'service')
```

In another terminal, launch the first client:

```powershell
$k3TrialRoot = Join-Path $env:LOCALAPPDATA 'Komaphone-K3-Try'
$env:KOMAPHONE_DATA_DIR = Join-Path $k3TrialRoot 'client-a/data'
$env:PHOTOCRAFT_CONFIG_DIR = Join-Path $k3TrialRoot 'client-a/config'
& ./log/k3/bin/photocraft.exe --new-instance
```

Repeat in separate terminals with `client-b` and optionally `client-c`. Keep each player's roots separate and reuse them on restart. Open **Private room**, connect to `http://127.0.0.1:43127` with a display name, and use the host's room ID and private invitation to join. The service currently accepts loopback connections only; separate-machine networking is a K4 release gate. Ctrl+C in the service terminal stops the listener. These development artifacts are not an installer.

Create a private room, choose Animation or Exquisite Corpse, and invite one or two friends. Each participant chooses Ready; the host starts when everyone is ready. Untimed rounds are the default. Existing pause, host claim, skip and incomplete archive controls remain available in Private room.

Open assigned drawing to enter the mouse workspace. Each assignment has its own saved native project. The frame/band dimensions stay fixed; layers can still be transformed within it. Submit saved drawing freezes that revision while the service accepts it. Local saving and service acceptance are separate states. When all players finish, open the next assigned drawing.

- Animation creates one chain per player and rotates artists each round. Round zero starts blank. Subsequent rounds show the immediately preceding frame beside the canvas and optionally as adjustable onion skin. Copy previous frame adds an independent editable layer; a pristine blank baseline is hidden so transparent source pixels remain transparent. Other existing layers are retained. Notes are saved separately and handed to the next artist; they never become pixels or replay steps.
- Exquisite Corpse creates one creature per player across three 1024 × 512 bands. Later rounds display only the preceding part's bottom 24 rows, locked above the editable band. Draw to the bottom edge to give the next artist an alignment reference. With two players, the head author returns for the legs.
- At reveal, Animation offers chain selection, play/pause, frame scrub, 1–24 fps and looping. Open handoff notes is an explicit metadata request. Corpse offers head, head-plus-torso and full-creature views, with separate author credits and missing-part labels. Each original band occupies exactly 512 output rows. Reveal navigation is saved locally and resumes after restart.

Drawing timelapse, gallery, export, portable archive restoration and local remix are K4. Shared-world remix, guided tours/export and since-you-were-away playback remain required in K6.

## Command seam

All existing `komaphone.room.*` commands retain their K2 meanings. Added engine commands are available to the native UI and authenticated control channel under the same phase/ownership checks:

| Command | Parameters and behavior |
|---|---|
| `komaphone.animation.copyPrevious` | `{}`; copy the currently authorized, fully loaded previous frame into the current editable assignment. Undoable and recorded as local replay content; immutable source unchanged. |
| `komaphone.room.setNote` | `{"note":"text"}`; enqueue a bounded local metadata save for the current unfrozen assignment. `status.note_draft` confirms the committed assignment/text. Queueing alone is not a save acknowledgment. |
| `komaphone.reveal.position` | `{"game":"id","chain":0,"frame":0,"fps":6,"looping":true,"visible_parts":1}`; persist a valid position in the revealed game. `status.reveal_position` confirms the durable result. |
| `komaphone.reveal.notes` | `{}`; explicitly request the current revealed game's handoff notes. Denied before reveal and to waiting participants. |
| `komaphone.room.mediaStatus` | `{}`; inspect currently allowed canonical image references, loaded hashes, loading error and explicitly requested notes. No bearer/invitation, hidden native checkpoint or speculative thumbnail is included. |

View-only controls remain serialized UI state: `rooms.animation`, `rooms.corpse_view` and `rooms.view_reveal`. `ui.state` exposes the currently rendered room/mode control rectangles for mouse-driven acceptance.

## Storage and limits

The service remains authoritative for assignment privacy, order, deadlines and immutable accepted artifacts. Presentation requests fetch only authorized canonical images; reference/reveal PNGs stay in bounded ephemeral caches and are removed when their grant disappears. No hidden checkpoint or replay payload is prefetched. Completed downloads are acknowledged by the engine after cache acceptance; delayed chunks/events are rechecked against the current projection. Transfer/network failures retain local work and use bounded retry delays.

The presentation loader accepts at most 36 game images, at most 8 MiB per PNG, a 256 MiB compressed-image cache and a separate 256 MiB decoded-image cache. GPU textures are additional bounded copies of the decoded images. A maximal noisy 36-frame game can therefore use substantially more memory than a typical game; these are explicit safety ceilings, not a measured performance claim. Native originals retain their existing color model/profile/depth; presentation derivatives are canonical sRGB RGBA8.

Notes and reveal positions use checksummed SQLite records with full transactions. A private note recovery table retains the latest locally acknowledged text for each service, room and assignment, including skipped or unsubmitted turns. Its limit is 1,024 assignments or 2 MiB of encoded note records; reaching a limit refuses growth and preserves existing notes. Explicitly clearing a note replaces that assignment's text only. The current note projection and its recovery record commit together. Historical note recovery is available through the local store API; gallery presentation belongs to K4. Old K2 records default missing presentation fields. Failed writes retain the previous acknowledged state. The mode UI never embeds notes, references, credits or cursors into document pixels. Original project recovery and accepted service copies remain independent.

## Acceptance

`scripts/komaphone/k3-native-smoke.ps1` is the isolated actual-app harness for full two/three-player games in both modes, reference/copy/notes/reveal controls, cache/command privacy, and editing/upload/transfer/reveal recovery. It uses synthetic drawing data, independent client roots and loopback control ports. Run it only against the final app/service pair named in status. The headless HTTP suite is `cargo test -p photocraft-sync --test k3_games --offline -- --nocapture`.

The tracked status must list exact commands, results, inspected screenshots, hashes and remaining limits before K3 is marked complete. A script existing or parsing successfully is not acceptance evidence.
