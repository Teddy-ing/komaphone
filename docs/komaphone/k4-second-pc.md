# Komaphone on a second Windows PC

This procedure uses a portable K4 test candidate. The other PC does not need
Rust, Git, Visual Studio, or the source repository. A source push is unnecessary.
The package is unsigned and is not an installer. The current K4 build completed
both games and the other private home-LAN checks on two physical Windows PCs.
On 2026-11-07 the owner accepted the remaining laptop startup/export-path issue
as a known K4 limitation. See [current status](status.md) for the exact package
and evidence. This does not establish access from friends on other networks.

## Prepare the other PC

1. Use Windows 10/11 on an Intel or AMD x64 PC with a working graphics driver.
2. Install Microsoft's [Visual C++ v14 x64 runtime](https://aka.ms/vc14/vc_redist.x64.exe)
   if needed. The current app needs version 14.44.35229 or later. See
   [Microsoft's runtime guidance](https://learn.microsoft.com/en-us/cpp/windows/latest-supported-vc-redist/).
   The launcher has its own runtime built in; the app still needs this prerequisite.
3. Put both PCs on the same normal home LAN, using Wi-Fi or Ethernet. Keep both
   awake and use a mouse for the drawing checks. A guest Wi-Fi network may isolate
   devices from one another.
4. Transfer the supplied ZIP privately, then use **Extract All** into a local
   folder. Keep the complete extracted package together.

## Launch and connect

Double-click **Start-Komaphone.exe**. It verifies the pinned file hashes, copies
the candidate into a package-specific folder under `%LOCALAPPDATA%\Komaphone`,
and launches the native app with separate data and configuration directories.
It preserves that profile on later launches and refuses conflicting existing
files. Local work does not live in the extracted ZIP or in OneDrive.

The package's `README.txt` identifies its exact endpoint, local folder and
certificate expiry. The launcher provides that public certificate only to this
app process and prefills the private service address. It does not modify global
Windows trust, start the server, or change firewall/router settings.

The coordinator must start the service on the host PC before you connect. Open
**Private room**, choose a display name, verify the prefilled server address, and
choose **Connect**. Use the private invitation supplied for this test to join.
Keep invitations and credentials out of shared screenshots and logs.

If no app window appears, inspect `launcher-log.txt` in the local folder listed
in `README.txt` and check the runtime installation. If graphics startup fails,
close the app and try **Start-Komaphone-Safe-GPU.exe**. It uses the same profile
and may be slower. Do not run both launchers at once.

For this accepted build, use a **full output path** in a folder you created outside
the program folder. For example, create an `Exports` folder in your Windows
Pictures folder, copy its full path from Explorer, then append `\animation.gif`
or another new filename. For PNG frames, append a new subfolder name instead.
Keep every export outside `candidate`; leave its verified program files unchanged.

The corrected launcher saves relative export filenames in the profile's `exports`
folder. Full output paths still select their named destinations. Keep the verified
`candidate` program folder unchanged. The initial 2026-11-07 launcher used that
program folder as its working directory, so a relative export could cause its next
launch to report `Unexpected candidate file`. Preserve any identified extra export
outside `candidate`, then use the corrected package with the same profile; do not
delete drawings or reinstall the data/config folders.

The latest laptop no-start report after trying the corrected package has not been
diagnosed. The owner accepted deferring it as **K4-LAUNCH-01**; successful local
regression checks do not prove that the laptop issue is fixed. This limitation
and its preservation/retest steps remain tracked for K7 hardening.

The host address must still match the package. If DHCP changes it, or the public
certificate expires, ask the coordinator to prepare a new matching package.
Replacing pinned files by hand causes the launcher to refuse them. Keep your
old data and verified backups; do not delete or clone a live profile to migrate.

## The assisted acceptance test

The user operates the second PC; the coordinator operates the host and checks
service/client evidence. There is no assumed remote-screen access. Report what
appears and provide screenshots or logs when needed.

The coordinator will guide one complete Animation game and one Exquisite Corpse
game, a client reconnect and service restart, then saving to Gallery, reopening,
both replay modes where applicable, exporting, creating an independent remix,
and importing an archive into a fresh data directory. These are actual private
HTTPS and separate-Windows-environment checks. Multiple local app processes
cannot substitute for this gate.

The current package has the K4 owner acceptance and known limitation described
above. Its bundled README/status snapshot predates that decision; retain the
verified ZIP bytes and consult the current tracked status for the later record.
World drawing, guided zoom tours, and catch-up playback belong to later K5/K6
work. Upstream PhotoCraft copyright, licenses, contributors, and asset attribution
remain in `candidate/`; Komaphone is independent and does not imply endorsement.

## Package preparation and verification

The coordinator first uses `scripts/komaphone/package-k4-candidate.ps1` with the
tested app/service hashes. `scripts/komaphone/package-k4-lan.ps1` then requires
that exact candidate manifest, app/service hashes, a public certificate hash,
an explicit private HTTPS endpoint and a fresh isolated profile namespace.
It validates the certificate's SAN/validity, builds a launcher with those pins,
copies an exact allowlist, and reads every ZIP entry back to verify its hash.
It never includes the service's private key, invitations, credentials or data.

The coordinator separately verifies extraction and the actual launcher/app
startup, records the ZIP SHA256 and retained screenshots, and gives the user the
finished artifact. The service remains stopped until the assisted test begins;
any necessary host firewall permission is prepared and reviewed separately.
