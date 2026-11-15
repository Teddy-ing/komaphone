# Shared canvases

K6 adds shared canvases to the native Windows app and has passed local acceptance. [Status](status.md) records the tests, native measurements and limitations. Google sign-in and the public HTTPS service are the next milestone, [K6.5](hosting-plan.md); hosted friend access has not been tested. Local canvases remain usable without either.

## Try the retained local build

On this development machine, candidate6 is retained under `log/k6/bin/candidate-6/`; the [status ledger](status.md#k6-final-acceptance-evidence--2026-11-12) records hashes and acceptance. From the repository root, start the service in a dedicated PowerShell terminal:

```powershell
$k6TryRoot = Join-Path $env:LOCALAPPDATA 'Komaphone-K6-Try'
& ./log/k6/bin/candidate-6/photocraft-sync.exe --bind 127.0.0.1:43127 --data-dir (Join-Path $k6TryRoot 'service')
```

In another terminal, launch a separate trial profile:

```powershell
$k6TryRoot = Join-Path $env:LOCALAPPDATA 'Komaphone-K6-Try'
$env:KOMAPHONE_DATA_DIR = Join-Path $k6TryRoot 'client-a/data'
$env:PHOTOCRAFT_CONFIG_DIR = Join-Path $k6TryRoot 'client-a/config'
& ./log/k6/bin/candidate-6/photocraft.exe --new-instance
```

For a second player, repeat the client block in a third terminal with `client-b` in both paths. Reuse the same paths on later launches. Each player opens **Private room**, enters `http://127.0.0.1:43127`, chooses a display name and selects **Connect**, then follows the canvas controls below. Close the clients and press Ctrl+C in the service terminal when finished. These trial roots keep existing personal artwork separate; the retained binaries are development builds, not installers. A fresh checkout must build from the [development setup](../development.md).

## Open and share a canvas

Connect your profile through **Private room**, then open **Shared canvases** from Home, the drawing workspace, or Local canvases. Enter a name and choose **Create canvas**. A new canvas belongs only to you. The gallery shows each downloaded or authorized canvas's preview, owner and saved revision; names need not be unique.

Under **People and invitations**, refresh the people list, select a person and create an invitation. Share that invitation with that person. They paste it under **Accept an invitation**. Each canvas has its own membership; accepting one invitation does not open your other canvases. Owners can remove members. Members can leave while keeping their already accepted contributions in the canvas.

Draw with the mouse, use the wheel to zoom around the pointer, and use middle drag or Space to pan. Drawing continues directly at the chosen scale. Eraser and undo affect your device's drawing layer. Other members' named cursors and unfinished strokes are temporary previews; losing a preview does not lose saved artwork.

## Offline work and recovery

The status bar distinguishes a stroke still being saved, locally saved edits waiting to sync, and the accepted service revision. After restarting offline, previously downloaded canvases remain in the gallery. New invitations and canvases that have never been downloaded require the service.

If access is removed, old offline edits remain associated with their original account, canvas and membership epoch. Re-invitation does not silently upload them. **Saved edits needing review** exposes retained originals. Continuing after a conflict starts new edits without discarding those originals. Switch back to the original account/canvas to save an unfinished gesture retained during a switch.

Owners can **Move canvas to Trash** after queued edits have synchronized. Normal access stops, invitations are invalidated, and artwork/history remain stored. Under **Trash**, refresh and choose **Restore canvas and members** to restore the original ID and previous members with fresh membership epochs. Old queued operations retain their old epochs. There is no permanent-delete control.

## Review, navigation and tours

Save personal bookmarks under **Bookmarks**. **Auto zoom** animates toward a saved view, with pause and cancel controls.

**Since your last review** shows changes after your explicit reviewed revision. Begin a review to freeze its end revision, then play, pause, change speed, scrub, filter highlights by artist, or jump to an affected location. The replay still follows the complete canonical history. New arrivals remain unseen until a later review; background sync does not mark them seen. Exit review before drawing again.

Under **Tours**, add views as stops, edit travel/pause durations, reorder or remove stops, and save a named route. Saved tours synchronize to members. Play, pause, scrub and cancel without changing artwork. A tour whose referenced artwork was removed offers repair or explicit skipping of missing destinations.

## Copy artwork and export

From a completed game in Gallery, choose **Place in a canvas**, open the named destination and prepare the source under **Export and remix**. Place the complete animation or the explicitly selected still. Copies keep original artist credits; they have independent placement and artwork ownership.

The same section lets you copy placed media or one synchronized stroke, open another canvas, and paste at its current view. Both source and destination access are checked when uploading. Changing, hiding or deleting a copy does not modify its source. You can draw on your own layer over copied game artwork.

Enter a full output path and choose:

- **PNG view** for the visible accepted scene, including the selected historical review position.
- **Tour GIF** or **Tour PNG frames** for a saved route at a frozen accepted revision.
- **Editable archive** for the full accepted world and referenced media.

Exports exclude UI, cursors and review highlights, and refuse to overwrite an existing destination. Unsynchronized edits must finish syncing before they can appear in accepted exports. Resolution is bounded to 2,048 pixels per side and tour export to 24 frames per second; GIF retains its palette/timing limitations.

**Restore archive as a new private canvas** requires the archive's original service and current source ownership. It creates a new owner-only canvas while preserving artwork/history/credits, leaving the source untouched. This differs from restoring the original canvas from Trash. To recover a lost service, use the verified service-database backup procedure in the [deployment runbook](k6-gcp-setup.md).

## Verification and limits

The tracked [native harness](../../scripts/komaphone/k6-native-smoke.ps1) runs isolated clients and a local test service. It preserves source/binary pins, decoded artwork checks, restart/recovery results and separate frame/presence timings. The [control protocol](../control-protocol.md#komaphone-shared-canvases-k6) documents the same stable command seam used by the UI.

Final candidate6 passes the two-client probe, full shared workflow/recovery, delayed-presence and131,072-point stress runs. Four-user preview p95 is239.39ms under simulated100ms RTT; regular/stress frame p95 is5.29/23.43ms. The full final UI suite passes5,192 tests with22 existing ignored tests; all nine quick benchmark programs complete successfully. These measurements and the eight-user proxy-error qualification are detailed in status.

Operational limits bound histories, points, media, queues, previews and caches; fixture groups of two, four and eight are coverage sizes, not product membership caps. An exhausted limit reports an error and preserves acknowledged originals. Local test results and Linux deployment feasibility do not establish internet latency or hosted access between households.
