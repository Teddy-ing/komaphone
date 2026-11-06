# K1 local drawing and drafts

This guide describes the K1 implementation and how to exercise it. The
[status ledger](status.md) owns the milestone result, exact check outcomes,
screenshots, and outstanding acceptance work. This guide is not a completion claim.

## Run the Windows app

On this development machine, the tested executable is retained outside the disposable
Cargo cache. From the repository root, a fully isolated trial is:

```powershell
$env:KOMAPHONE_DATA_DIR = Join-Path $env:LOCALAPPDATA 'Komaphone-K1-Try/drafts'
$env:PHOTOCRAFT_CONFIG_DIR = Join-Path $env:LOCALAPPDATA 'Komaphone-K1-Try/config'
& ./log/k1/bin/photocraft.exe --new-instance
```

This is a local development build, not an installer or the later friend-ready release.
Its SHA-256 is in `log/k1/bin/photocraft.sha256`. Use the same two overrides on later
launches to retain the trial's drafts and preferences.

To rebuild from source on this machine:

```powershell
. ./log/k0/env-msvc.ps1
cargo run -p photocraft -- --new-instance
```

The local environment helper selects the workspace's Rust/MSVC tools and the
coordinator target directory. It is a machine-local development artifact. On a
fresh checkout, use the normal Rust/MSVC setup in [development](../development.md)
instead. The machine names remain `photocraft-*`; the drawing workspace is Komaphone.
Komaphone does not check for or install upstream PhotoCraft updates. An independent
release/update channel remains future release work.

Permanent drafts use `%LOCALAPPDATA%\Komaphone\drafts.sqlite3` by default. To try
the build with an independent data directory, set the process-local override
before launching:

```powershell
. ./log/k0/env-msvc.ps1
$env:KOMAPHONE_DATA_DIR = Join-Path $env:LOCALAPPDATA 'Komaphone-K1-Try'
cargo run -p photocraft -- --new-instance
```

Relaunch with the same override to reopen those drafts. Use a different directory
for each independent test instance; `--new-instance` alone does not isolate its
storage. Keep real drafts on a local disk outside OneDrive, network shares, and
the repository. The override changes draft storage, not every upstream preference
or temporary-recovery location. Unsetting it returns later launches to the default
root; it does not delete the test drawings.

## Draw and reopen

Automation can select the compact or Advanced workspace with
`ui.set {"komaphone":true}` or `false`. `ui.inspect.komaphone` reports its
serialized view state; this view flag does not assert that a document is saved.

Choose **New drawing** for a white 1536 × 1024 canvas. The compact workspace keeps
these controls visible:

- Brush, pencil, eraser, fill, color picker, rectangle selection, lasso, move,
  rectangle/ellipse/line shapes, text, pan, and zoom.
- Brush size, opacity, smoothing, presets, and hardness for supported round tips.
  Size is in document pixels. Colors and the expandable palette are on the right.
- **Transform** for scale/rotation, **Deselect**, **Fit view**, and **100%**.
  Wheel zooms; Space plus drag pans. Undo and Redo are in the header.
- A compact layer list with add, duplicate, delete, rename, visibility, ordering,
  lock, and opacity controls. **Import image** uses the existing embedded-image
  import path.

**Advanced** opens the full existing editor. **Return to Komaphone drawing** brings
back the compact layout without replacing the document. The engine, color/depth
support, native format, and advanced subsystems remain available.

Open **Local drafts** to see saved titles, UTC update dates, revisions, and current
database usage. **Refresh drafts** reloads the list; **Open** restores a confirmed
checkpoint. Opening a project already in the session selects its existing editor.
This is a draft list, not the K4 gallery, replay player, or game archive browser.

## What the save indicator means

| Indicator | Meaning |
|---|---|
| Drawing / Unsaved | Input or a completed edit is not yet confirmed durable. An unfinished stroke, text edit, or transform is not covered by an earlier save. |
| Saving locally / waiting for local storage | A retained edit is queued or being written. Queueing is not an acknowledgment. |
| Saved locally · revision N | The corresponding completed edit and replay information committed to the local database. |
| Save failed | The current request remains retained, and its error explains why no new acknowledgment was issued. |
| Local only · not synced | The drawing has no service acknowledgment. This remains separate from whether it is saved on this computer. |

The existing tab asterisk tracks changes not saved to an external editor file.
The footer separately confirms whether those changes are durable in the local draft.

K1 starts no multiplayer service and does not present local writes as synced work.
Private rooms and transport belong to K2; actual party-mode integration belongs to
K3. Local saves protect acknowledged work against app interruption, not disk loss.

Closing an acknowledged local drawing does not require choosing an unrelated file
path. If an edit or save is unfinished, the close dialog waits while the window
remains responsive. It offers **Keep drawing**, **Retry local save** when there is
a failure, and **Close without waiting**. The last choice explicitly allows loss
of unacknowledged changes while retaining the last saved revision. External files
keep the existing Save / Don't Save behavior.

## Failures, recovery, and limits

After a recoverable storage problem is resolved, use **Retry save** or **Retry local
save**. The retry uses the original request identity and snapshot; repeated clicks
cannot append the same accepted edit twice. A disk/open failure pauses writes
through that store. A conflicting or invalid individual project retains its failed
request and later queued edits while independent projects can continue saving.
Further edits to the failed project stop until its retained save is resolved.

If the latest stored checkpoint or event is corrupt, reopening searches retained
revisions for a readable checkpoint. The original damaged rows remain intact. The
recovered drawing receives an independent project identity and baseline, with a
notice in Local drafts. Unsupported database schemas are rejected without rewriting
them. Keep the data directory when reporting a recovery error.

Current operational limits are:

| Resource | Limit |
|---|---|
| Default database quota | 2 GiB; displayed in Local drafts. A rollback journal also needs free disk space. |
| Local drawings | 1,024 projects; 8,192 pixels per side and 16,777,216 total pixels. |
| Editable checkpoint | 64 MiB encoded; 8 MiB manifest; 64 MiB per decoded blob; 512 MiB aggregate decoded data; 4,096 layers and the existing native-format group-depth limit. |
| Retained document snapshot | 512 MiB conservative accounting budget, including metadata and embedded assets. |
| Canonical replay record | 8 MiB; up to 1,000,000 records and seven days of relative replay time per project. |
| Pending recorder | 64 edits or a 512 MiB accounting budget; growth stops at the limit without dropping retained edits. |
| Retained failed projects | 16 or a 512 MiB accounting budget; further growth stops while existing requests remain retained. |

Dimensions alone do not guarantee a checkpoint fits: large imported metadata or
embedded content can exceed encoded or decoded limits even on a small canvas. The
worker checks that the generated checkpoint can reopen before acknowledging it. A failure
leaves the current drawing and the previous saved revision intact. Use Advanced
and **File > Save / Save As** to preserve an external editable copy when a local
project cannot fit. A large legacy document identified as outside local-draft
limits stays in the existing editor workflow and explicitly says to use File Save.
It does not silently gain a local-durability guarantee or prevent ordinary local
drawings from being created.

There is no automatic pruning of drafts or unsynced work. K1 does not provide a
portable full-game archive, a verified backup/restore workflow, or recoverable
gallery deletion; those remain assigned later work. An ordinary `.pcraft` external
save preserves its editor document but is not a K4 archive of project replay,
identity, and provenance. See the [storage contract](../../crates/local-store/README.md)
for paginated replay, recovery-search, and SQLite details.

## Engine and automation seam

These registered engine commands serve the UI, tests, and the existing authenticated
[control protocol](../control-protocol.md). Unknown parameters and malformed IDs
return errors. `new`, `list`, `open`, and `retry` require configured native storage;
`status` can report that storage is unavailable.

| Command | Parameters / result |
|---|---|
| `komaphone.project.new` | `{}` or `{"width":1536,"height":1024,"name":"Drawing"}`; creates a normal white editor document. Names are nonempty and at most 96 UTF-8 bytes. |
| `komaphone.project.list` | `{}`; queues a metadata refresh and returns `{"pending":true}`. |
| `komaphone.project.open` | `{"project":"<32-character lowercase hex project ID>"}`; reopens or selects that project. |
| `komaphone.project.retry` | `{}`; retries retained local work, prioritizing the active failed project when the writer is free. |
| `komaphone.project.status` | `{}`; polls completions and returns `available`, `loading`, `drafts`, `error`, `notice`, `usage`, `externalReason`, and the active project's status. |

Existing drawing commands keep their IDs, including `paint.stroke`,
`layer.new.layer`, `layer.setProps`, `layer.renameLayer`, `edit.undo`, and `edit.redo`.
The compact UI reuses these commands. Workspace selection and camera navigation
remain view state.

For headless integration, configure `Session::enable_local_projects` with an
isolated database path, execute commands, and call `poll_local_projects` while work
is pending. Tests can call `flush_local_projects` with a bounded timeout. Treat
`{"pending":true}` and `available:true` as scheduling/configuration information,
not save success. Wait for the matching local revision with no relevant error or
pending edit. A flush drains runnable projects before reporting an independently
retained project failure.

Replay is recorded from the first new document. Brush operations capture resolved
settings, seeds, targets, samples, and relative timing before Actions portability
rewrites or coalescing. Undo and redo remain visible events. Other edits, including
unsupported command playback, retain explicit result-checkpoint steps. Imports
begin from a baseline without invented earlier drawing history. Every saved event
has a native result checkpoint; reopening restores final pixels and layers, not
the editor's prior interactive undo stack. The polished replay viewer is K4 work.

## Reproduce focused checks

Use the coordinator's existing build directory on this machine; do not start a
second whole-app build alongside an active milestone run.

```powershell
. ./log/k0/env-msvc.ps1
cargo test -p photocraft-local-store
cargo test -p photocraft-engine --test local_replay
cargo test -p photocraft-engine --test local_projects
cargo test -p photocraft-engine --test local_project_failures
cargo test -p photocraft-engine --test local_recovery
cargo test -p photocraft-engine --test local_replay_assets
```

Storage tests include a real child-process kill after acknowledgment, a kill during
uncommitted SQLite disk writes, and `SQLITE_FULL`. Engine tests exercise the actual
background worker, reopened drawing pixels/layers, failed-request retention,
independent-project progress, and a small canvas with oversized encoded metadata.
They do not simulate physical power removal or establish backup protection.

Current run artifacts include [storage tests](../../log/k1/local-store-tests.log),
[storage clippy](../../log/k1/local-store-clippy.log),
[engine integration tests](../../log/k1/engine-local-tests.log), and
[workspace tests](../../log/k1/workspace-tests.log). These local, ignored logs can
contain intermediate runs. Consult [status](status.md) for final commands/results,
inspected screenshots, mandatory gates still unchecked, and the next milestone
action. Do not infer a completed K1 gate from this recipe or from a single test log.
