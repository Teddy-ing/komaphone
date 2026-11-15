# K7 release acceptance

Started 2026-11-14 against baseline 1.4. **In progress; not a release approval.**
K6 and K6.5 are accepted with the qualifications in [status](status.md). The
owner reports that a fresh tester in another household is not yet available.
Keep independent preparation moving; never replace that gate with another
same-home hotspot test.

## Requirements and evidence review

The entries below point to accepted milestone evidence, not newly executed K7
tests. Recheck changed paths and pin the final package before release acceptance.

| Product IDs | Existing evidence location in status | K7 check |
|---|---|---|
| E1–E4 | K1 completion; K5 stroke-quality follow-up | Inspect final mouse workspace and reference-machine performance; retain Advanced tools and native/WASM boundaries. |
| E5, L1–L5 | K2/K3 completion; K6.5 hosting cases 1–3 | Repeat hosted invitations, phase restrictions, interruptions and retained drafts with the release package. |
| A1–A5 | K3 native acceptance; K4 completion; K6.5 cases 2–3 | Animation reveal, notes excluded from pixels, playback, GIF/frame/archive exports, reopen and remix. Retain exact two/three-player coverage. |
| C1–C4 | K3 boundary assembly/privacy; K4 completion; K6.5 case 2 | Corpse strip privacy, correct assembly, replay, PNG/archive exports and reopen. |
| R1–R4 | K1 durability; K4 archive restoration; K6.5 cases 3/5 | Failed upgrade, quota, corruption, protocol mismatch, client/service backup restore and rollback with originals retained. |
| G1–G2 | K4 gallery/remix; K6 world remix; K6.5 cases 2/4 | Preserve gallery originals, attribution and timing through local/world remix, restart and exports. |
| W1–W6 | K5/K6 final acceptance; K6.5 case 4 | Multiple canvases, deep strokes, offline/asynchronous convergence, personal undo, presence and bounded rendering. |
| W7–W8 | K6 tour/catch-up acceptance; K6.5 case 4 | Named tours, missing-stop handling, pinned GIF/PNG export, fixed review interval and persisted explicit review progress. |
| W9 | K6 membership/revocation fixtures; K6.5 case 4; K7 candidate2 native import | Independent owners/member lists, nonmember denial, more-than-four fixtures, revoked offline recovery and private restored copies. **Local portability gap closed:** candidate2 restores without the original service/source, preserves complete art/history/assets/credits, supports edited re-import and offline reopening. Repeat with the coordinated hosted release; imported credits never grant account access. |
| H1–H2, H4 | Accepted K6.5 independent ISP/cellular test | **Pending:** fresh Windows user in another household follows the guide, signs in with Google and completes both party and shared-canvas flows. |
| H3 | K6.5 hosted runbook and case 6 | Refresh full pre-credit costs/usage; verify backup/renewal/rollback. Preserve $20 target/~$30 acceptable range and qualified domain/tax figures. |

## Prepared local checks

- Launcher update-failure regression: a conflicting partially installed version
  must leave the old verified program and synthetic old-format data bytes intact;
  retry and returning to the old version must preserve those bytes. This checks
  packaging isolation, **not** database-schema migration compatibility.
- Exact command: `cargo test --manifest-path scripts/komaphone/k65-launcher/Cargo.toml --locked --offline`.
  On 2026-11-14 all **9 tests passed** outside the restricted sandbox. The initial
  sandbox run failed five filesystem tests because NTFS hard links/junctions were
  denied; preserve that diagnostic in `log/k7/root/launcher-tests.log`.
- `cargo clippy --manifest-path scripts/komaphone/k65-launcher/Cargo.toml --locked --offline --all-targets -- -D warnings`
  passed. Native test/lint logs are under `log/k7/root/`.
- Deployment activation now preserves pinned new systemd units and a root-only
  snapshot of prior installed units/environment files. Coordinator independently
  ran `test-rollback-preservation.py` (4 passed) and `test_deploy.py` (9 passed) in
  WSL. These verify local orchestration with fake service controls; the populated
  service rollback drill is separate. No deployment occurred.
- Real populated Linux service drill passed independently: two authors/two
  acknowledged strokes, room/member identity and seven content tables survive
  real backup/fresh restore; future schema is rejected unchanged; restored
  sessions/invitations stay quarantined. See status for exact artifacts. This
  injects failure into disposable data using the same compatible binary; actual
  release-to-release migration and production/systemd rollback remain separate.
- Actual Windows executable migrated a synthetic room schema1 fixture to schema2
  and reopened with stable profile/device/namespace and exact queued request.
  The pre-migration backup is readable and unchanged logically. Corrupt metadata
  and future schemas produce errors while independent local projects still save.
  This does not establish populated historical-art migration or Google linking.
- Separate historical populated-art checks now pass:18 drafts/30 revisions,
  two games, four byte-identical original exports, edited remix IDs and16 canvas
  strokes/131,072 samples preserved; populated future-schema refusal leaves
  the failed draft and readable original copies unchanged. See status for exact
  evidence and harness qualifications. Google/account migration is separate.
- Current native capacity and deep-zoom recovery checks pass, with frame p95
  24.2465ms and5.3488ms respectively against33ms. All28 deep-zoom screenshots
  were independently inspected. These pin the unchanged candidate1 application;
  future native changes require affected revalidation.
- The baseline service's real quota/corruption checks pass, but canvas protocol
  mismatch incorrectly returns400/invalid rather than426/version. A service fix
  and regression tests are in progress. No fixed artifact has been deployed.
- Final corrected service now passes111 Windows tests,13 focused Linux tests,
  strict linting/formatting/layers, actual quota/corruption/mismatch drills and
  independently repeated real K6.5 schema3→4 upgrade/old-reader refusal/preupgrade
  restore. Domain archive checks pass74 tests plus Clippy/WASM. Exact final pins,
  run paths and qualifications are in status. These supersede the baseline
  mismatch failure above; no corrected production deployment is claimed.
- Candidate2's full native shared-canvas regression passes on protocol2: eight
  members, actual pointer drawing, offline queue/restart, review progress,
  tours/exports, game media/remix, portable import, access removal and Trash
  restoration.629 frame intervals measure p95 5.2995ms against33ms. Standalone
  fresh-service import separately preserves all16 original events and19 edited
  re-import events with byte-identical assets and offline canonical hashes.
  Exact hashes/results and visual-review scope are in status. No hosted claim.
- Final engine/client tests total3,797 passes/22ignored; strict engine/client/UI
  Clippy,36-crate layering,28-crate WASM plus HEIF,660/660 parity and the ignored
  adversarial command gate pass. A concurrent UI latency failure remains recorded;
  its unchanged executable passes alone at43.47ms p95 against50ms.
- The concrete [operations plan](k7-operations.md) is prepared. Final native
  performance/quick benchmarks, new packaging and public-hosted operational
  acceptance remain open.

## Cost review — 2026-11-14

The existing forecast remains **$19.33/month light use** and **$24.73/month at
50 GiB outbound traffic**, before tax/credits and including a provisional $1
domain share. This is a projection, not a measured monthly bill. No resources
were created or resized. The established inventory is in the
[hosted runbook](k65-hosted-setup.md#refreshed-cost-and-limits).

Rechecked public rates: e2-small $0.016752855/hour from Google's indexed
[E2 price table](https://cloud.google.com/products/compute/pricing/general-purpose?hl=en)
(direct retrieval timed out); in-use IPv4 $0.005/hour and North America premium
egress $0.12/GiB from [network pricing](https://cloud.google.com/vpc/network-pricing);
standard disk $0.000054795/GiB-hour from
[disk pricing](https://cloud.google.com/compute/disks-image-pricing); Iowa Standard
GCS $0.000027397/GiB-hour from [storage pricing](https://cloud.google.com/storage/pricing).
Using 730 hours, 30 GiB disks and 32 GiB backup allowance reproduces the existing
rounded forecast. The $0.01 request allowance remains an estimate. Free-tier
allowances and credits are excluded; actual domain share/taxes, current usage,
backup retention and billing observation still need release-time reconciliation.

## When a tester is available

1. Supply the newly verified candidate ZIP and hash, its quick-start, and a private
   service invitation. Arrange Google test-audience access privately if still
   required. Record package/app/service hashes and Windows/network arrangement;
   do not collect passwords, tokens, or unnecessary personal details.
2. Have the tester extract and start the package by following the guide on a
   fresh Windows profile from another household. Record any assistance required,
   normal and safe-GPU launch results where relevant, Google login and normal
   room/canvas invitations. No VPN or certificate import.
3. Finish both modes, interrupt/reconnect, save/reopen Gallery, replay and export.
   Exercise private and shared canvases, simultaneous drawing, asynchronous
   handoff, offline recovery, tours/export, catch-up and remix. Record preserved
   artwork/revisions and denied nonmember access.
4. On the affected K4 laptop, retain its existing work and launcher inventory/log;
   export once with a relative filename and once to a full path, then close and
   reopen the same profile. Confirm both exports and originals survive. Candidate5
   startup alone does not close **K4-LAUNCH-01**.
5. Test the prepared upgrade with isolated old-format data and retain a usable
   original backup on failure. Complete isolated service rollback/restore and
   hosted renewal/restart checks before public activation of changed service code.
6. Reconcile every row above with exact evidence and remaining qualifications.
   Update status and release notes; only then mark K7 complete.

Existing candidate5 remains the accepted K6.5 artifact. Do not relabel or modify
that historical ZIP to imply K7 acceptance. K7 candidate1 is now separately
packaged at `log/k7/packages/komaphone-k7-candidate-1.zip`, SHA256
`DFF0BFEB8B061D8346895EA4CD1A3E826A5D8EA301B83B8BDC00B00655DF1C05`.
It contains the unchanged accepted native application and a newly verified
launcher/guide, explicitly labeled acceptance pending. Final native UI and
performance checks, operational drills and external acceptance remain pending.
