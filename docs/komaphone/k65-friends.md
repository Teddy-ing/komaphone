# Try Komaphone with friends

This is a **K6.5 hosted test candidate**, not the final release. Use the package supplied for the current test and keep its files together.

1. Extract the entire Windows x64 ZIP into a local folder. Double-click **Start-Komaphone.exe**.
2. Open **Private room** and enter your display name. For your first sign-in, expand **First-time service invitation** and enter the private admission invitation supplied by the host **before** clicking **Sign in with Google**.
3. Finish sign-in in your normal browser, then return to Komaphone. The service is already configured as `https://komaphone.tedsoftware.org`. Google identifies your account; separate room and canvas invitations determine where you can play. Never share your Google password or the app's stored credentials.
4. For a party game, accept the room invitation, select **Ready**, and let the host start Animation or Exquisite Corpse. Rooms have two or three players. Draw with the mouse, wait for **Saved locally**, then submit.
5. At the reveal, save the result to Gallery. You can replay it, export it or make an independent remix. **Shared canvases** lets you accept a canvas invitation and draw with its members; each canvas has its own member list.

Friends need no VPN, certificate import, router changes, developer tools or service-token copying. The coordinator will guide the hosted tests; a successful login alone does not establish that every flow has passed.

Google sign-in setup is included in the verified package. Keep its files together and launch through **Start-Komaphone.exe**; no configuration download or manual copying is needed.

## Your drawings and updates

Your local data, settings, exports and launcher log live under:

```text
%LOCALAPPDATA%\Komaphone\k65-hosted
```

The launcher copies each program version into its own folder and keeps the same drawing/profile directories when you update. Close Komaphone before starting another version. Keep old K4/K5/K6 drawing folders and backups; this test package does not move or delete them. Do not copy a live profile to another computer. Use archive export/import for portable artwork and Google sign-in plus invitations for shared access.

Relative export names go into the profile's `exports` folder. A full path to a chosen existing folder also works. Drawings remain local when you sign out or the service is unavailable. **Saved locally** and **Synced** are different: wait for sync before expecting a friend to see new work. Switching accounts preserves the previous account's pending work separately.

## If launch or sign-in fails

- If Windows reports a missing runtime, install Microsoft's latest supported **Visual C++ v14 x64 Redistributable** from its [official download page](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/), then retry. The app needs Windows 10/11 x64 and a working graphics driver; no Rust toolchain is needed.
- If the normal window does not appear, close any existing Komaphone process and try **Start-Komaphone-Safe-GPU.exe**. It uses the same saved work and may be slower. Launcher failures open the local diagnostic log when possible; send the error text to the coordinator, without credentials or artwork.
- If Google says the app is limited to test users, tell the coordinator which account you intend to use privately so it can be added to the test audience. Do not disable browser security or change your Google password to work around it.
- Canceling login or losing the network leaves local drawings intact. Retry sign-in when ready. Use **Sign out** before switching accounts; use the account controls to keep their work separate.
- If **Private room** shows **Connection paused**, choose **Reconnect** to resume the saved session and queued work. This also recovers a paused account left by an older test build. If the session has expired or was signed out, use **Sign in again** instead.

The earlier K4 laptop startup report remains tracked as K4-LAUNCH-01 until that machine passes the new launcher/restart test. Local launcher tests do not claim that particular laptop is fixed.

Komaphone is an independent project based on PhotoCraft. The package retains upstream copyright, licenses, contributors and asset attribution under `candidate/`; it does not imply upstream endorsement. The existing `photocraft.exe` machine name is intentional.
