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
| E5, L1–L5 | K2/K3 completion; K6.5 hosting cases 1–3; K7 Candidate 3 Leave Room fix + B2b Step E | Candidate 3 (`9A54729E…` / app `1146466D…`) passed local two-client Leave Room UI acceptance (`log/k7/leave-room-fix-20261116/native-leave-ui-verification.json`) and hosted two-PC Step E (`acb2b8ad…` Exquisite Corpse + `db90b0ca…` Animation across watched `komaphone.service` restart with retained Round `0` drafts, `log/k7/checkpoint-b2b-20261116/b2b-watched-restart-verification.json`, `quiet-close-vm-ro.json`). Repeat fresh-household room flow when tester is available. |
| A1–A5 | K3 native acceptance; K4 completion; K6.5 cases 2–3 | Animation reveal, notes excluded from pixels, playback, GIF/frame/archive exports, reopen and remix. Retain exact two/three-player coverage. |
| C1–C4 | K3 boundary assembly/privacy; K4 completion; K6.5 case 2 | Corpse strip privacy, correct assembly, replay, PNG/archive exports and reopen. |
| R1–R4 | K1 durability; K4 archive restoration; K6.5 cases 3/5 | Failed upgrade, quota, corruption, protocol mismatch, client/service backup restore and rollback with originals retained. |
| G1–G2 | K4 gallery/remix; K6 world remix; K6.5 cases 2/4 | Preserve gallery originals, attribution and timing through local/world remix, restart and exports. |
| W1–W6 | K5/K6 final acceptance; K6.5 case 4 | Multiple canvases, deep strokes, offline/asynchronous convergence, personal undo, presence and bounded rendering. |
| W7–W8 | K6 tour/catch-up acceptance; K6.5 case 4 | Named tours, missing-stop handling, pinned GIF/PNG export, fixed review interval and persisted explicit review progress. |
| W9 | K6 membership/revocation fixtures; K6.5 case 4; K7 candidate2 native import + Candidate 3 B2b hosted import | Independent owners/member lists, nonmember denial, more-than-four fixtures, revoked offline recovery and private restored copies. **Local & hosted portability verified:** Candidate 2 passed standalone fresh-service import locally, and Candidate 3 restored `k7-portable-test.komworld` (`646fc2da…`) on live schema4 `komaphone-sync-1` with source `2995225c…` in Trash into owner-only canvas `bd541d6d…` (`rev = 4`, `83cea85b…`, `world_import_origins` `5ee013b6…`, `world_import_receipts` `0332f19d…`). Repeat in another-household onboarding. |
| H1–H2, H4 | Accepted K6.5 independent ISP/cellular test | **Sole remaining K7 acceptance gate (pending):** fresh Windows user in another household follows the guide, signs in with Google and completes both party and shared-canvas flows. |
| H3 | K6.5 hosted runbook and case 6; K7 Checkpoints B1 + B2a + B2b + C | Checkpoints B1, B2a, B2b, and C accepted (B2b and C accepted based on supplied evidence). B1/B2a verified live config, headroom, Caddy ARI, quiet checkpoint at shared rev `67`, pre-upgrade backup `20261117T003922Z-H26I1l0d` (`c6f25030…`), and receiptless session quarantine. B2b (`log/k7/checkpoint-b2b-20261116/`) activated K7 `f4d77aee…-df220b59…` (`user_version = 4`, closed-proxy `426`/`400` and 10-table preservation green), passed two-PC hosted regression + watched `1.46s` same-version `komaphone.service` restart, re-enabled `komaphone-backup.timer`, and verified schema4 backup `20261117T200321Z-PUHwATSi` (`a81d0335…`, gen `1794945802047482`) + isolated restore (`6` games, `5` canvases, unchanged `world_import_origins`/`world_import_receipts`, empty/actively cleaned `world_imports`/`world_import_assets`, `--backup-to` pass). Checkpoint C (`log/k7/checkpoint-c-20261117/cost-and-renewal-reconciliation.json`) reconciled live GCP/VM/GCS/traffic footprint (`96.14h` uptime, `8` pairs / `39,070,583` B = `0.114%` of `32 GiB`, `1.635 GiB` mixed `tx`, diagnostic `730h` upper bound `USD 20.22/month` alongside settled `USD 19.33`/`USD 24.73` forecasts and `OWNER-OBSERVED` Nov 1–13 `USD 0.08`/`USD 0.08`/`USD 0.00` Billing UI) and post-B2b Caddy ARI renewal readiness (`2027-01-11`..`2027-01-13`, `0` TLS errors). **Retained qualifications:** live Cloud Billing export, post-Nov-13 invoices, taxes, exact domain amortization, and credit balance/expiry remain `NOT VERIFIED`; natural production certificate renewal remains `NOT VERIFIED` until the January 2027 Let's Encrypt ARI window. |

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
- The concrete [operations plan](k7-operations.md) is prepared and Checkpoint A
  was accepted after Codex's focused receipt/artifact review. Candidate2's
  packaging (`komaphone-k7-candidate-2.zip`, SHA256 `70804C6C…`), isolated
  two-cycle package launcher smoke (`log/k7/launcher-candidate2/root-session/result.json`),
  all nine `cargo xtask perf --quick` synthetic benchmarks (`log/k7/root/perf-quick-verification.json`,
  `log/k7/root/perf-quick-results/`), and post-rustfmt source/binary provenance
  rebinding (`log/k7/root/native-rebind2.json`, exact binary equality on both
  `F8C397…` and `BB0260…`) pass locally.
- Read-only hosted Checkpoint B1 (`log/k7/checkpoint-b1-20261115/b1-readiness-summary.json`)
  verified the live `d30065dd…-df220b59…` release/unit/env/Caddy pins (zero drop-ins),
  schema3 state (shared canvas `b6fdd25b…` at revision `57` / `53` strokes, `0`
  active unexpired sessions), `19.47 GiB` disk and `99.70%` quota headroom
  (`backup_staging_count=1` leaving zero spare failure slots), 5 GCS backup pairs
  (`18,257,706` bytes, latest `20261115T051516Z-2rPNqvkt`), qualified usage/billing
  observations, Caddy ARI renewal scheduling (`2027-01-11`..`2027-01-13`), the
  required participant quiet checkpoint, and exact B2 backup/restore/upgrade/rollback
  steps without mutating live state.
- Pre-upgrade Checkpoint B2a (`log/k7/checkpoint-b2a-20261116/b2a-summary.json`)
  verified both PCs saved/synced at shared revision `67` / `63` strokes
  (`21765d22…`, superseding historical revision `57`), took and generation-read-back
  pre-upgrade backup `20261117T003922Z-H26I1l0d` (`c6f25030…`, generation
  `1794875962461168`, bucket then `7` pairs / `31,218,184` bytes), passed populated
  isolated `restore-drill.py` across all `4` games and `3` canvases, and passed
  receiptless known-session quarantine (`200 → 403 nonmember → 401/session → 200`,
  zero VM session receipts exported) while production stayed running untouched.
- Hosted Checkpoint B2b (`log/k7/checkpoint-b2b-20261116/`) upgraded
  `komaphone-sync-1` to K7 `f4d77aee…-df220b59…` (`user_version = 4`, closed-proxy
  `426`/`400`/`401`/`403` and 10-table preservation green; prior release and
  `acf352d7…` / `346d7d2d…` schema3 backups retained), completed two-PC hosted
  regression across Candidate 2 + Candidate 3 (shared canvas `b6fdd25b…` `67 → 75`,
  trashed source `2995225c…` + portable import `bd541d6d…` at rev `4`, Exquisite
  Corpse `acb2b8ad…` `6` entries, Animation `db90b0ca…` `8` entries across watched
  `1.46s` same-version `komaphone.service` restart), re-enabled
  `komaphone-backup.timer`, and verified schema4 backup `20261117T200321Z-PUHwATSi`
  (`a81d0335…`, gen `1794945802047482`, bucket now `8` pairs / `39,070,583` bytes)
  plus isolated restore (`6` games, `5` canvases, unchanged `world_import_origins`
  `5ee013b6…` and `world_import_receipts` `0332f19d…`, empty and actively cleaned
  `world_imports`/`world_import_assets`, full `--backup-to` pass; accepted based
  on supplied evidence).
- Checkpoint C (`log/k7/launcher-candidate3/result.json`,
  `log/k7/checkpoint-c-20261117/`) completed isolated three-cycle Candidate 3
  launcher smoke (`Start-Komaphone.exe` normal + same-profile reopen +
  `Start-Komaphone-Safe-GPU.exe`, all `exit 0`, `3` visually inspected PNGs,
  `photocraft.exe` `1146466D…` verified at `138,468,352` bytes; Safe GPU
  `"PhotoCraft"` modal wording queued as `QOL-001` in
  [adjustments-backlog.md](adjustments-backlog.md) without modifying or
  rebuilding Candidate 3 now), reconciled release-time GCP/VM/GCS usage, cost
  bounds and post-B2b Caddy ARI renewal readiness
  (`cost-and-renewal-reconciliation.json`), and was **accepted based on supplied
  evidence**. For **K4-LAUNCH-01**, the owner explicitly waived the remaining
  affected-laptop relative-export/reopen subcheck for K7 while retaining
  Attempt 1's partial results (`OWNER-SUPPLIED`: `19` intact files `9E519980…`,
  full-path PNG export `72AE060D…` `57,148` B, preserved
  `gallery.sqlite3`/`drafts.sqlite3`, two `exit 0` reopens), the unverified
  relative-export limitation (`not` relabeled a pass), and full-path export
  guidance outside `candidate/` (`checkpoint-c-owner-acceptance-waiver.json`).
  **Fresh friend onboarding from another household (`H1`, `H2`, `H4`) is the
  sole remaining K7 acceptance gate.**

## Cost review — 2026-11-14 (reconciled 2026-11-17 in Checkpoint C)

The existing forecast remains **\$19.33/month light use** and **\$24.73/month at
50 GiB outbound traffic**, before tax/credits and including a provisional \$1
domain share. This is a projection, not a measured monthly bill. No resources
were created or resized. The established inventory is in the
[hosted runbook](k65-hosted-setup.md#refreshed-cost-and-limits).

Rechecked public rates: e2-small \$0.016752855/hour from Google's indexed
[E2 price table](https://cloud.google.com/products/compute/pricing/general-purpose?hl=en)
(direct retrieval timed out); in-use IPv4 \$0.005/hour and North America premium
egress \$0.12/GiB from [network pricing](https://cloud.google.com/vpc/network-pricing);
standard disk \$0.000054795/GiB-hour from
[disk pricing](https://cloud.google.com/compute/disks-image-pricing); Iowa Standard
GCS \$0.000027397/GiB-hour from [storage pricing](https://cloud.google.com/storage/pricing).
Using 730 hours, 30 GiB disks and 32 GiB backup allowance reproduces the existing
rounded forecast. The \$0.01 request allowance remains an estimate. Free-tier
allowances and credits are excluded.

Checkpoint C release-time reconciliation (`log/k7/checkpoint-c-20261117/cost-and-renewal-reconciliation.json`,
`2026-11-17T20:31Z`, strictly read-only) verified unchanged GCP inventory (`1`
standard non-Spot `e2-small`, `30 GiB` `pd-standard` disks, `1` `IN_USE` static
IPv4, `0` Cloud Logging agents / `16.0 MiB` local systemd journals, `0` BigQuery
datasets, `cloudbilling`/`billingbudgets` APIs disabled), `8` completed backup
pairs (`16` live objects, `39,070,583` bytes = `0.114%` of `32 GiB` allowance, `0`
soft-deleted under `8`-day Delete + `7`-day soft-delete rules), `96.14h`
continuous host uptime (`komaphone.service` RSS `66.77 MiB`, `caddy.service` RSS
`38.19 MiB`, `10.02s` idle non-idle CPU `0.90%`, `19.38 GiB` data-disk free), and
cumulative `ens4` `rx = 0.8036 GiB` / `tx = 1.6350 GiB` (a `730h` diagnostic
extrapolation of mixed upgrade/backup/play `tx` yields `12.41 GiB`, or `USD 20.22/month`
upper bound if 100% billed at `USD 0.12/GiB` + `USD 1` domain share — within the
settled `≤ USD 20` target / `~USD 30` ceiling). Nov 1–13 Billing UI (`Cost USD 0.08`,
`Savings USD 0.08`, `Total cost USD 0.00`) remains `OWNER-OBSERVED`; live Billing
export, post-Nov-13 invoices, taxes, exact domain amortization, and credit
balance/expiry remain `NOT VERIFIED`.

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
4. Affected K4 laptop **K4-LAUNCH-01** (`log/k7/checkpoint-c-20261117/{laptop-k4-launch-01-disposition,checkpoint-c-owner-acceptance-waiver}.json`):
   Attempt 1 (`OWNER-SUPPLIED` on `Windows NT 10.0.22621.0`) verified `19` intact
   Candidate 3 program files (`9E519980…`, app `1146466D…`), full-path PNG export
   (`72AE060D…`, `57,148` B), preserved `gallery.sqlite3`/`drafts.sqlite3`, and
   two prior `exit 0` same-profile launches (`pid 22852`, `pid 46380`), while
   `relExport` (`exports\k7-laptop-rel.png`) returned `exists: false` and `pid 3956`
   was still open. On follow-up, the owner encountered an unspecified issue,
   declined further laptop troubleshooting, and **explicitly waived the remaining
   `K4-LAUNCH-01` relative-export/reopen subcheck for K7** (`2026-11-17`). Keep
   the unverified relative-export/reopen subcheck recorded as a limitation (**not**
   relabeled a pass), retain full-path export guidance outside `candidate/` (new
   output filename or sequence subfolder), and do not investigate further unless
   the owner requests it.
5. Test the prepared upgrade with isolated old-format data and retain a usable
   original backup on failure. Complete isolated service rollback/restore and
   hosted renewal/restart checks before public activation of changed service code.
6. Reconcile every row above with exact evidence and remaining qualifications.
   Update status and release notes; only then mark K7 complete.

Existing candidate5 remains the accepted K6.5 artifact. Do not relabel or modify
that historical ZIP to imply K7 acceptance. K7 candidate1 remains preserved at
`log/k7/packages/komaphone-k7-candidate-1.zip`, SHA256
`DFF0BFEB8B061D8346895EA4CD1A3E826A5D8EA301B83B8BDC00B00655DF1C05` (historical
pre-import package). Historical K7 candidate2 remains preserved untouched at
`log/k7/packages/komaphone-k7-candidate-2.zip`, SHA256
`70804C6C3EE7AB919236A8BA25A00E611E76C83E35FE879E0D2EEAA14A2FDD2B` (app
`F8C39773F7B47B34FEC7D94491F775F67B3CC2DF24B63FEC5E89EFE6E6BDAEAF`, launcher
`167538D7BA71E924DB66F03A4AAAFDDABF37EE8DCA34DA07FBE77DC18A0B7DE2`). Pending K7
candidate3 (fixing the bounded Leave Room / pending-request loop with zero server
change) is packaged separately at `log/k7/packages/komaphone-k7-candidate-3.zip`,
SHA256 `9A54729E2035499FE4339E208B80DD210B3D8922218248552E827E1C0B2C90F9`,
containing app `1146466D207B6554012DE41871565792813A6008F1C9C0792804829E305330EF`
(`138,468,352` bytes), manifest
`9E519980D7B6E1D548D82C3A19D8B65E3DEA9FDC8C0F8716BC50C01BC2FAF8EA`, and launcher
`505FDCEA16E92D67A7C8D26552449C46668CDD99AD2FD28E1EF91F79192159C6`, explicitly
labeled acceptance pending (`QOL-001` Safe GPU `"PhotoCraft"` modal wording is
queued in [adjustments-backlog.md](adjustments-backlog.md) without changing or
rebuilding Candidate 3 now). Checkpoints B2b and C are accepted based on supplied
evidence, and the owner explicitly waived the remaining **K4-LAUNCH-01**
relative-export/reopen subcheck for K7 while retaining full-path export guidance
outside `candidate/` (not relabeled a pass). Billing (`OWNER-OBSERVED` Nov 1–13
Billing UI + settled `\$19.33`/`\$24.73` pre-credit/pre-tax forecasts; live
Billing export, post-Nov-13 invoices, taxes, exact domain amortization and credit
balance/expiry `NOT VERIFIED`) and natural production certificate renewal (`NOT
VERIFIED` until the January 2027 Let's Encrypt ARI window) remain explicit
qualifications. **Fresh friend onboarding from another household (`H1`, `H2`,
`H4`) is the sole remaining K7 acceptance gate.**
