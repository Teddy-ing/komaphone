# Frozen K4 friend-play setup

Prepared 2026-11-07, independently of active K5 work. This setup uses the tested K4 app and service, a separately built standalone launcher, and fresh client/service data. It does not include K5 features or modify K5 source/builds. K4-LAUNCH-01 remains the owner-accepted export/startup limitation.

## Package and connection

- [Windows friend ZIP](../../log/k4/Komaphone-K4-Friends-Windows-2026-11-07-v2.zip), SHA256 `44853EF43F4D61950CD3FF4F14E150FE8AE08751FDA7A2EBD3BF20B54ED14306`.
- Server: `https://100.79.210.40:43137`, bound only to this host's Tailscale address.
- Client profile: `%LOCALAPPDATA%/Komaphone/k4-lan-friends-20261107`, separate from the previous laptop-test profile and K5. Existing drawings are preserved in their original profiles; no migration is performed.
- Host service/data: `%LOCALAPPDATA%/Komaphone/K4-friends-20261107`, protected for the owner and SYSTEM. The service is intentionally running for friend play. The private TLS key stays there and is never packaged.
- The unsigned portable app needs Windows x64 and Microsoft's Visual C++ v14 x64 runtime >=14.44.35229. The bundled README links the official installer. No Rust or source checkout is needed.

## Host: invite friends and play

1. Keep Tailscale connected and this PC awake. In the [Tailscale Machines page](https://login.tailscale.com/admin/machines), find this PC at `100.79.210.40`, choose its menu, then **Share**. Generate a single-use sharing link for each friend and send it privately. Do not publish the invitation or share your login. [Official machine-sharing instructions](https://tailscale.com/docs/features/sharing).
2. Privately send the friend ZIP above. Each friend installs Tailscale, signs in with their own account, accepts the machine-sharing invitation, and extracts the ZIP. This grants network access; it is separate from the Komaphone room invitation.
3. Everyone, including the host, runs `Start-Komaphone.exe` from this package. Open **Private room**. Whenever a Server field is shown and you press Connect, replace its default with `https://100.79.210.40:43137`, enter your display name, and click **Connect**. The frozen K4 app's automatic address fill only accepts LAN addresses; VPN entry is deliberately manual. Existing paired-room reconnect was tested separately.
4. Host chooses Animation or Exquisite Corpse and **Create room**. Use **Copy room ID** and **Create private invitation** to send the game's credentials privately. Friends enter those under **Join room**. All players select **Ready**, then the host starts. Party rooms support two or three players total.
5. Draw, wait for **Saved locally**, then Submit each round. Save the reveal to Gallery. Export using a full path into a folder outside `candidate`, with a new filename or sequence subfolder. Keep originals and backups.

The server is already running. After restarting Windows, run [Start-K4-Server.cmd](../../log/k4/K4-Friends-Host/Start-K4-Server.cmd). [Status-K4-Server.cmd](../../log/k4/K4-Friends-Host/Status-K4-Server.cmd) checks its address and certificate-verified health. After games finish and everyone has saved/closed their client, [Stop-K4-Server.cmd](../../log/k4/K4-Friends-Host/Stop-K4-Server.cmd) stops only this service and retains data. Stop terminates the process; avoid using it during submissions. No automatic Windows startup or public hosting was installed.

The firewall rule `Komaphone-K4-Friends-Tailscale-20261107` permits only the pinned K4 server executable, Tailscale interface, local `100.79.210.40`, TCP43137, from `100.64.0.0/10`. Tailscale membership/sharing and policy still govern peer access. There is no router forwarding, global certificate-trust change, or automation-port exposure. The owner approved Windows elevation. The exact rule can be inspected or removed using the prepared `friends-firewall.ps1 -Action Status|Remove`; removal requires administrator elevation.

## Evidence and limits

Evidence folder: `log/k4/friends-preparation-12e941bafe2f48fa98c2c7e0f094de58/`. [Independent review](../../log/k4/friends-preparation-12e941bafe2f48fa98c2c7e0f094de58/friends-review.json), [launcher checks](../../log/k4/friends-preparation-12e941bafe2f48fa98c2c7e0f094de58/launcher-v2-checks.json), [package result](../../log/k4/friends-preparation-12e941bafe2f48fa98c2c7e0f094de58/package-v2-result.json), and [visual review](../../log/k4/friends-preparation-12e941bafe2f48fa98c2c7e0f094de58/visual-review.json).

- Frozen app SHA256 `5D31D715BD637F78BFDD3A26927ACF07C85FE93AD422294DA20A8D74413C6B78`; service `C185937AC66B9500341F2E9941236339764565C4ACF733B3E582A7D09ABFBA4E`.
- Launcher `EAC8F4A326FAD358AA23546AD92830BCA56F9B5E43321331795F5E037393976C`: 15 native tests, strict all-target Clippy, formatting, static-CRT release and PE dependency checks passed. Only the isolated snapshot changed: RFC6598 endpoint validation and removal of the unsupported VPN environment-prefill convenience; HTTPS trust and strict file inventory remain intact.
- Public certificate SHA256 `C8F486903FFB3A54213D51C8B2DA9D5D60DE78C485D21C9896A377911D158A66`. Its exact expiry is in the ZIP README and transfer metadata. Renew/repackage before expiry or if the VPN address changes; do not edit pinned files.
- Native command: `& ./log/k4/friends-preparation-12e941bafe2f48fa98c2c7e0f094de58/verify-friends-package.ps1 -ZipPath ./log/k4/Komaphone-K4-Friends-Windows-2026-11-07-v2.zip -ExpectedZipSha256 44853EF43F4D61950CD3FF4F14E150FE8AE08751FDA7A2EBD3BF20B54ED14306`. Result [fafc88d4648f4122aaada5bb39741806](../../log/k4/package-launch-fafc88d4648f4122aaada5bb39741806/result.json) passed: observed lobby fields typed through the native app's control seam, Connect clicked, HTTPS pairing, private lobby creation, same-identity/lobby SafeGPU reconnect, duplicate-profile refusal, four zero exits, and isolated credential cleanup. Root inspected fresh/connected screenshots. SafeGPU's inherited CPU-renderer notice remains visible.
- Earlier failed runs are retained: v1 rejected VPN prefill; the first v2 verifier wrongly expected an identity-only profile with no room to show a live connection after restart. The stronger final test used a real saved lobby and passed. No failed run was relabeled. The final result contains a correction to its inherited startup-only scope label.
- Firewall preview passed; application succeeded but the first readback comparison rejected Windows' equivalent `100.64.0.0/255.192.0.0` notation. Exact filter inspection and the corrected read-only Status check passed without broadening or changing the installed rule.
- Host start/stop/restart and idempotent Start passed with certificate-verified `/health`. Test credentials were removed; real user profiles and K5 processes were untouched. Build/artifact isolation is recorded in the review.

**Not yet verified:** a friend accepting the Tailscale share and connecting from another home. The successful check used this host's real VPN address and TLS, not a second remote machine. A friend's first Connect is the next external check. This package is ready to share for that step; no public service or WAN completion claim is made.
