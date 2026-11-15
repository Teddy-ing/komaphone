# Try the Komaphone K7 candidate

This is an **unsigned K7 release hardening test candidate; K7 acceptance is pending**, not the final release. Use the complete ZIP and SHA256 supplied by the coordinator. Fresh friend onboarding from another household and the affected laptop's export/restart regression must pass before release acceptance.

The pending **candidate 2** uses shared-canvas protocol 2 and requires a coordinated service upgrade. Wait for the coordinator to confirm that the destination service and your package are compatible before connecting. Local candidate testing does not mean the hosted service has been upgraded. Older packages remain historical test artifacts; do not mix their files with this candidate or use them as an improvised rollback.

## Install and join

1. Extract the entire Windows x64 ZIP to a local folder. Keep its files together and double-click **Start-Komaphone.exe**. You need Windows 10/11 x64, a working graphics driver and Microsoft's [Visual C++ v14 x64 Redistributable](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/). No Rust toolchain is needed.
2. Open **Private room** and enter your display name. Expand **First-time service invitation**, paste the private invitation supplied by the host, then choose **Sign in with Google**.
3. Complete sign-in in your normal browser and return to Komaphone. The package configures `https://komaphone.tedsoftware.org`; no VPN, certificate import, router configuration or developer token is needed. Google sign-in identifies you; separate room and canvas invitations control access.
4. Accept the host's room invitation, choose **Ready**, and let the host start Animation or Exquisite Corpse. Both modes use two or three players. Draw, wait for **Saved locally**, then submit. If a disconnected game remains paused after everyone reconnects, the moderator chooses Resume.
5. At reveal, save the game to **Gallery**. Open it there to play the animation, inspect individual drawing replays, export or make an independent remix. Animation supports GIF, PNG frames and an editable archive; creatures support PNG and an editable archive.

The service is private. If Google says the app is restricted to test users, tell the coordinator privately which account you intend to use. Never send passwords or stored app credentials, or disable browser security to get past an error.

## Shared canvases, tours and catch-up

Open **Shared canvases** to create a named canvas or accept its owner's invitation. Each canvas has its own member list; joining one does not reveal the owner's other canvases. Pan or zoom and draw directly. Other members own their own strokes; your undo does not undo their work. Canvas groups can exceed four members; party rooms still have two or three players.

Under **Tours**, add views as stops, name and save the route. Saved tour names appear above **Play** and **Edit stops**. Under **Export and remix**, choose a saved tour for GIF or PNG-frame export. Export uses a frozen synced revision. **Since your last review** lets you replay changes, filter author highlights and explicitly mark the interval seen; incoming changes remain unseen until reviewed.

In Gallery, **Place in a canvas** prepares an independent copy of a revealed game result. Open the destination canvas, prepare the source under **Export and remix**, then place it. Copies preserve credits without changing the original.

## Saved work and updates

Your profile, drawings, settings, exports and launcher log remain under:

```text
%LOCALAPPDATA%\Komaphone\k65-hosted
```

The older folder name is intentional: this candidate uses the same profile as K6.5. Close Komaphone before starting the new version. Extract the new ZIP separately, then run its launcher. It installs verified program files into a separate version folder. Do not replace individual files inside an older package or clone a live profile to another computer. Keep old packages and drawing folders. A separate test profile supplied by the coordinator has its own named folder.

Relative export names go into the profile's `exports` folder; full paths can select another existing folder. Use a new output name or folder each time: exports do not silently overwrite existing files. Wait for export completion before closing the app. **Saved locally**, **Sync pending** and **Synced** are different; wait for sync before expecting another member to see your drawing. Local drawing remains available signed out or during an outage. Account switching preserves pending work separately.

Export portable Gallery archives/backups and shared-canvas editable archives before relying on a new version. Local recovery protects against app interruption, not disk loss. Wait for the complete shared-canvas history to synchronize before exporting an archive.

In **Shared canvases → Restore an archive**, select the complete saved archive folder. Candidate 2 can restore it without the original service or source-canvas ownership, including when no canvas is open. You must be signed in to a compatible destination service. Restore creates a new private canvas owned only by you; invite others separately. Artwork, retained history, tours and historical credits come from the archive. Those credits are historical assertions, not verified live identities or transferred access. The original archive stays unchanged. You can add new work, export the edited canvas and restore that archive again. If confirmation fails after upload, check for the destination canvas before retrying; do not delete your archive.

Standalone restore, edited re-import and offline reopening have passed a local native test against an isolated service. Hosted restore and friend onboarding still require their own acceptance checks. The administrator continues to maintain off-VM service backups for service-wide recovery. Do not point an old app at a profile changed by a newer schema as an improvised rollback; ask the coordinator to use a verified compatible backup without replacing newer work.

## If something fails

- If no window appears, close any existing Komaphone process and try **Start-Komaphone-Safe-GPU.exe**, which uses the same saved work. Launcher failures open the local diagnostic log when possible. Share the error text with the coordinator, excluding credentials and artwork.
- If Windows reports a missing runtime, install the Microsoft runtime linked above and retry. The package is unsigned; if Windows or your organization blocks it, report the message to the coordinator rather than disabling security controls.
- **Connection paused** can be resumed with **Reconnect**. For an expired or signed-out session, choose **Sign in again**. Canceling login leaves local drawings intact.
- Retained edits after a conflict or access removal appear under **Saved edits needing review**. Keep those originals; reconnecting or being invited again does not silently publish an old revoked queue.
- If export, storage or upgrade fails, preserve the existing profile and any backup. Do not delete originals or reinstall over data to clear an error.

## Candidate notes and acceptance limits

This candidate prepares the application for K7 regression testing; its protocol 2 service upgrade is not claimed deployed or accepted here. K6.5 already demonstrated Google sign-in, both party modes and shared-canvas play on home broadband plus a cellular hotspot. That is not fresh onboarding from another household. K7 must also verify clean install/update, recovery, quotas/protocol mismatch, service rollback and the full feature path.

K4-LAUNCH-01 remains open until the affected laptop's exact launcher/inventory, full-path and relative-path export, and same-profile restart are checked. A successful launch on a different PC is not proof that report is resolved. The service uses one VM, so outages can delay sync; local work remains recoverable. Tours and exports have bounded sizes/durations, and imported raster images retain their original resolution.

Komaphone is an independent project based on PhotoCraft. Upstream copyright, licenses, contributors and asset attribution are retained under `candidate/`, without upstream endorsement. The `photocraft.exe` machine name is intentional. In this package, this guide is stored at `candidate/docs/komaphone/k65-friends.md` to preserve the launcher's verified inventory.
