//! QOL-010 integration and security tests for managed Komaphone Gallery exports.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use photocraft_codecs::{ChannelLayout, EncodeOptions, Format, Image};
use photocraft_doc::Document;
use photocraft_engine::Session;
use photocraft_engine::gallery_exports::{
    ExportFormat, MAX_EXPORT_STEM_CHARS, ManagedExportStage, sanitize_default_export_name, strip_windows_verbatim, validate_export_basename,
    validate_reveal_target,
};
use photocraft_format::{PcraftWriter, SaveOptions};
use photocraft_local_store::StoreOptions;
use photocraft_local_store::gallery::{GalleryKind, GalleryStore};
use photocraft_party::gallery::{GalleryBundle, GalleryManifest};
use photocraft_party::room::{Assignment, MemberView, Phase, RevealedEntry, RoomView};
use photocraft_party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RoomId,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn open_session(data_dir: &Path, explicit_exports: Option<PathBuf>) -> Session {
    std::fs::create_dir_all(data_dir).unwrap();
    let mut session = Session::new();
    session.enable_local_projects_with_exports(data_dir.join("drafts.sqlite3"), StoreOptions::default(), explicit_exports).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    wait_gallery(&mut session);
    session
}

fn wait_gallery(session: &mut Session) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while session.gallery.busy() {
        assert!(Instant::now() < deadline, "gallery worker timed out");
        session.poll_gallery();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn gallery(session: &mut Session, command: &str, params: Value) -> Result<Value, String> {
    let out = session.execute(command, params).map_err(|e| e.to_string())?;
    wait_gallery(session);
    if let Some(err) = session.gallery.error.take() {
        return Err(err);
    }
    Ok(out)
}

fn participant(seed: u64, name: &str) -> Participant {
    Participant { id: ParticipantId::new(&format!("{seed:032x}")).unwrap(), device: DeviceId::new(&format!("{seed:032x}")).unwrap(), display_name: name.into() }
}

fn make_real_drawing_asset(
    width: u32,
    height: u32,
    color_hex: &str,
    stroke_rgba: [u8; 4],
    title: &str,
) -> (AssetRef, CheckpointRef, BTreeMap<AssetHash, Vec<u8>>) {
    let mut session = Session::new();
    session.execute("file.new", json!({"width": width, "height": height, "background": "white"})).unwrap();
    let x1 = f64::from(width) * 0.15;
    let y1 = f64::from(height) * 0.25;
    let x2 = f64::from(width) * 0.85;
    let y2 = f64::from(height) * 0.75;
    session.execute("paint.stroke", json!({"points": [[x1, y1], [x2, y2]], "size": 16.0, "color": color_hex})).unwrap();
    let mut doc: Document = session.active().unwrap().doc.as_ref().clone();
    doc.name = title.into();
    let (cp_bytes, _) = PcraftWriter::new().save_zip(&doc, &SaveOptions::default()).unwrap();
    let cp_hash = AssetHash::new(&format!("{:x}", Sha256::digest(&cp_bytes))).unwrap();
    let cp_asset = AssetRef { hash: cp_hash, bytes: cp_bytes.len() as u64 };
    let checkpoint = CheckpointRef { id: CheckpointId::new(&cp_hash.as_str()[..32]).unwrap(), asset: cp_asset };

    let mut rgba = vec![248u8; (width * height * 4) as usize];
    for y in (height / 4)..(height * 3 / 4) {
        for x in (width / 6)..(width * 5 / 6) {
            let idx = ((y * width + x) * 4) as usize;
            rgba[idx..idx + 4].copy_from_slice(&stroke_rgba);
        }
    }
    let img = Image::from_u8(width, height, ChannelLayout::Rgba, rgba).unwrap();
    let png_bytes = photocraft_codecs::encode(&img, Format::Png, &EncodeOptions::default()).unwrap();
    let png_hash = AssetHash::new(&format!("{:x}", Sha256::digest(&png_bytes))).unwrap();
    let preview = AssetRef { hash: png_hash, bytes: png_bytes.len() as u64 };

    let mut objects = BTreeMap::new();
    objects.insert(cp_hash, cp_bytes);
    objects.insert(png_hash, png_bytes);
    (preview, checkpoint, objects)
}

fn save_drawing_to_gallery(session: &mut Session, color: &str, title: &str) -> ProjectId {
    session.execute("komaphone.project.new", json!({"width": 96, "height": 64})).unwrap();
    session.execute("paint.stroke", json!({"points": [[10.0, 14.0], [82.0, 50.0]], "size": 8.0, "color": color})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let id = session.local_projects.status(session.active().unwrap().doc.id).project.unwrap();
    gallery(session, "komaphone.gallery.save", json!({"entry": id, "title": title})).unwrap();
    gallery(session, "komaphone.gallery.open", json!({"entry": id, "draft": false})).unwrap();
    id
}

fn insert_game_to_gallery(data_dir: &Path, seed: u64, title: &str, mode: Mode) -> ProjectId {
    let p1 = participant(seed * 10 + 1, "Alice");
    let p2 = participant(seed * 10 + 2, "Bob");
    let rounds = mode.rounds(2).unwrap();
    let game_id = GameId::new(&format!("{seed:032x}")).unwrap();
    let mut objects = BTreeMap::new();
    let mut reveal = Vec::new();
    let palette = [
        ("#cc3322", [204, 51, 34, 255]),
        ("#2266cc", [34, 102, 204, 255]),
        ("#229955", [34, 153, 85, 255]),
        ("#aa44aa", [170, 68, 170, 255]),
        ("#dd8822", [221, 136, 34, 255]),
        ("#228899", [34, 136, 153, 255]),
    ];
    let (width, height) = match mode {
        Mode::Animation { .. } => (1536, 1024),
        Mode::ExquisiteCorpse => (1024, 512),
    };
    let mut idx = 0usize;
    for chain in 0..2u8 {
        for round in 0..rounds {
            let (hex, rgba) = palette[idx % palette.len()];
            let (preview, checkpoint, entry_objs) = make_real_drawing_asset(width, height, hex, rgba, &format!("{title}-{chain}-{round}"));
            objects.extend(entry_objs);
            let proj = ProjectId::new(&format!("{:032x}", seed * 100 + (idx as u64) + 1)).unwrap();
            let player = if (chain + round) % 2 == 0 { p1.id } else { p2.id };
            reveal.push(RevealedEntry::Artwork {
                assignment: Assignment { id: AssignmentId { game: game_id, chain, round }, player, project: proj },
                revision: 2,
                image: preview,
                checkpoint,
                replay: Box::new(ReplayRef {
                    id: ReplayId::new(proj.as_str()).unwrap(),
                    manifest: None,
                    baseline: checkpoint,
                    end_event: None,
                    event_count: 0,
                    duration_ms: 0,
                }),
                assets: vec![preview, checkpoint.asset],
                extent: None,
            });
            idx += 1;
        }
    }
    let room = RoomView {
        id: RoomId::new(&format!("{:032x}", seed + 5000)).unwrap(),
        revision: 12,
        epoch: 1,
        phase: Phase::Reveal,
        round: rounds,
        mode,
        timer_ms: None,
        moderator: p1.id,
        members: vec![MemberView { participant: p1.clone(), connected: true, ready: true }, MemberView { participant: p2, connected: true, ready: true }],
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    };
    let id = ProjectId::new(game_id.as_str()).unwrap();
    let mut manifest = GalleryManifest::from_reveal(id, &room, p1.id, "2026-11-22T12:00:00Z".into()).unwrap();
    manifest.title = title.into();
    let bundle = GalleryBundle { manifest, objects };
    let mut store = GalleryStore::open(data_dir.join("gallery.sqlite3"), StoreOptions::default()).unwrap();
    let info = store.insert(&bundle).unwrap();
    let expected = if matches!(mode, Mode::ExquisiteCorpse) { GalleryKind::ExquisiteCorpse } else { GalleryKind::Animation };
    assert_eq!(info.kind, expected);
    id
}

#[test]
fn managed_exports_cover_all_formats_collisions_normalization_and_cwd_independence() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let expected_exports = dir.path().join("Exports");
    let unrelated_cwd = dir.path().join("elsewhere");
    std::fs::create_dir_all(&unrelated_cwd).unwrap();

    let mut session = open_session(&data_dir, None);
    let drawing_id = save_drawing_to_gallery(&mut session, "#cc3344", "Sunset: Over / Ocean?");
    assert_eq!(sanitize_default_export_name("Sunset: Over / Ocean?"), "Sunset Over Ocean");

    // 1. Drawing exports (archive + png), plus applicability errors for gif / sequence.
    assert!(
        gallery(&mut session, "komaphone.gallery.exportManaged", json!({"format": "gif", "name": "Sunset", "chain": 0, "fps": 6, "looping": true})).is_err()
    );
    assert!(
        gallery(&mut session, "komaphone.gallery.exportManaged", json!({"format": "sequence", "name": "Sunset", "chain": 0, "fps": 6, "looping": true}))
            .is_err()
    );

    gallery(
        &mut session,
        "komaphone.gallery.exportManaged",
        json!({"format": "archive", "name": "  Sunset.komarchive  ", "chain": 0, "fps": 6, "looping": true}),
    )
    .unwrap();
    let last_archive = session.gallery.last_export.clone().unwrap();
    assert_eq!(last_archive.format, "archive");
    assert!(!last_archive.is_directory);
    assert_eq!(Path::new(&last_archive.path), expected_exports.join("Sunset.komarchive"));
    assert!(Path::new(&last_archive.path).is_file());
    assert!(std::fs::read_dir(&unrelated_cwd).unwrap().next().is_none(), "export must stay inside managed Exports folder regardless of CWD");

    // Redundant typed extension + trailing dots/spaces normalization, plus non-destructive collision numbering.
    gallery(
        &mut session,
        "komaphone.gallery.exportManaged",
        json!({"format": "png", "name": "v1.2.final.png.. ", "chain": 0, "frame": 0, "fps": 6, "looping": true}),
    )
    .unwrap();
    let first_png_path = PathBuf::from(&session.gallery.last_export.as_ref().unwrap().path);
    assert_eq!(first_png_path, expected_exports.join("v1.2.final.png"));
    let first_png_bytes = std::fs::read(&first_png_path).unwrap();
    assert!(first_png_bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

    gallery(&mut session, "komaphone.gallery.exportManaged", json!({"format": "png", "name": "v1.2.final", "chain": 0, "frame": 0, "fps": 6, "looping": true}))
        .unwrap();
    let second_png_path = PathBuf::from(&session.gallery.last_export.as_ref().unwrap().path);
    assert_eq!(second_png_path, expected_exports.join("v1.2.final (2).png"));
    assert_eq!(std::fs::read(&first_png_path).unwrap(), first_png_bytes, "collision numbering must preserve prior file");

    gallery(
        &mut session,
        "komaphone.gallery.exportManaged",
        json!({"format": "png", "name": "v1.2.final.PNG", "chain": 0, "frame": 0, "fps": 6, "looping": true}),
    )
    .unwrap();
    assert_eq!(PathBuf::from(&session.gallery.last_export.as_ref().unwrap().path), expected_exports.join("v1.2.final (3).png"));

    // Internal non-export dot extension kept intact.
    gallery(
        &mut session,
        "komaphone.gallery.exportManaged",
        json!({"format": "png", "name": "my.png.study", "chain": 0, "frame": 0, "fps": 6, "looping": true}),
    )
    .unwrap();
    assert_eq!(PathBuf::from(&session.gallery.last_export.as_ref().unwrap().path), expected_exports.join("my.png.study.png"));

    // Verify status serialization exposes lastExport, exportsDir, and exporting.
    let status = session.execute("komaphone.gallery.status", json!({})).unwrap();
    assert_eq!(status["lastExport"]["format"], "png");
    assert_eq!(status["exporting"], false);
    assert!(status["exportsDir"].as_str().unwrap().ends_with("Exports"));

    // 2. Animation and Exquisite Corpse game exports.
    verify_game_exports(&mut session, &data_dir, &expected_exports);

    // No stray staging dirs left inside Exports.
    let leftover_stage = std::fs::read_dir(&expected_exports).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".komaphone-export-"));
    assert!(!leftover_stage, "no temporary export staging dirs may remain");

    // Re-open original drawing still works identically.
    gallery(&mut session, "komaphone.gallery.open", json!({"entry": drawing_id, "draft": false})).unwrap();
}

fn verify_game_exports(session: &mut Session, data_dir: &Path, expected_exports: &Path) {
    let anim_id = insert_game_to_gallery(data_dir, 20, "Bouncing Lantern", Mode::Animation { rotations: 1 });
    gallery(session, "komaphone.gallery.open", json!({"entry": anim_id, "draft": false})).unwrap();
    assert!(session.gallery.last_export.is_none(), "opening another gallery item must clear last_export");

    for (fmt, frame, expected_name) in
        [("archive", None, "Bouncing Lantern.komarchive"), ("png", Some(0), "Bouncing Lantern.png"), ("gif", None, "Bouncing Lantern.gif")]
    {
        gallery(
            session,
            "komaphone.gallery.exportManaged",
            json!({"format": fmt, "name": "Bouncing Lantern", "chain": 0, "frame": frame, "fps": 6, "looping": true}),
        )
        .unwrap();
        let last = session.gallery.last_export.as_ref().unwrap();
        assert!(!last.is_directory);
        assert_eq!(Path::new(&last.path), expected_exports.join(expected_name));
        assert!(Path::new(&last.path).is_file());
        if fmt == "gif" {
            let gif = std::fs::read(&last.path).unwrap();
            assert!(gif.starts_with(b"GIF89a") || gif.starts_with(b"GIF87a"));
        }
    }

    gallery(session, "komaphone.gallery.exportManaged", json!({"format": "sequence", "name": "Bouncing Lantern", "chain": 0, "fps": 6, "looping": true}))
        .unwrap();
    let seq1 = session.gallery.last_export.clone().unwrap();
    assert!(seq1.is_directory);
    assert_eq!(seq1.format, "sequence");
    assert_eq!(Path::new(&seq1.path), expected_exports.join("Bouncing Lantern"));
    assert!(Path::new(&seq1.path).is_dir());
    let seq1_count = std::fs::read_dir(&seq1.path).unwrap().count();
    assert!(seq1_count >= 2);

    // Repeating sequence export creates `Bouncing Lantern (2)` cleanly without clobbering `Bouncing Lantern`.
    gallery(session, "komaphone.gallery.exportManaged", json!({"format": "sequence", "name": "Bouncing Lantern", "chain": 0, "fps": 6, "looping": true}))
        .unwrap();
    let seq2 = session.gallery.last_export.clone().unwrap();
    assert!(seq2.is_directory);
    assert_eq!(Path::new(&seq2.path), expected_exports.join("Bouncing Lantern (2)"));
    assert_eq!(std::fs::read_dir(&seq1.path).unwrap().count(), seq1_count);
    assert_eq!(std::fs::read_dir(&seq2.path).unwrap().count(), seq1_count);

    // Exquisite Corpse supports archive, png, and sequence, and cleanly rejects gif.
    let corpse_id = insert_game_to_gallery(data_dir, 30, "Midnight Robot", Mode::ExquisiteCorpse);
    gallery(session, "komaphone.gallery.open", json!({"entry": corpse_id, "draft": false})).unwrap();
    assert!(gallery(session, "komaphone.gallery.exportManaged", json!({"format": "gif", "name": "Robot", "chain": 0, "fps": 6, "looping": true})).is_err());
    for fmt in ["archive", "png", "sequence"] {
        gallery(session, "komaphone.gallery.exportManaged", json!({"format": fmt, "name": "Robot", "chain": 0, "fps": 6, "looping": true})).unwrap();
        let last = session.gallery.last_export.as_ref().unwrap();
        assert_eq!(last.is_directory, fmt == "sequence");
        assert!(Path::new(&last.path).exists());
    }
}

#[test]
fn hostile_names_invalid_paths_and_cancelled_or_failed_exports_preserve_everything() {
    let dir = tempfile::tempdir().unwrap();
    let data_dir = dir.path().join("data");
    let custom_exports = dir.path().join("CustomExports");
    let mut session = open_session(&data_dir, Some(custom_exports.clone()));
    save_drawing_to_gallery(&mut session, "#2255aa", "Baseline");

    let overlong = "a".repeat(MAX_EXPORT_STEM_CHARS + 1);
    for bad in [
        "",
        "   ",
        ".",
        "..",
        "a/b",
        "a\\b",
        "../escape",
        "C:foo",
        r"C:\Temp\evil",
        r"\\server\share\x",
        "name:stream",
        "bad*name",
        "bad?name",
        "bad\"name",
        "bad<name",
        "bad>name",
        "bad|name",
        "bad\0name",
        "bad\nname",
        "CON",
        "con.txt",
        "PRN",
        "AUX",
        "NUL",
        "COM1",
        "LPT9",
        &overlong,
    ] {
        assert!(validate_export_basename(bad).is_err(), "expected validation failure for {bad:?}");
        assert!(
            gallery(&mut session, "komaphone.gallery.exportManaged", json!({"format": "png", "name": bad, "chain": 0, "fps": 6, "looping": true})).is_err(),
            "expected command failure for {bad:?}"
        );
    }

    // Export one clean file, then verify cancellation before dispatch / error cleanup preserves it and cleans staging.
    gallery(&mut session, "komaphone.gallery.exportManaged", json!({"format": "png", "name": "KeepMe", "chain": 0, "fps": 6, "looping": true})).unwrap();
    let kept = custom_exports.join("KeepMe.png");
    let kept_bytes = std::fs::read(&kept).unwrap();

    session.execute("komaphone.gallery.cancelExport", json!({})).unwrap();
    let err = gallery(&mut session, "komaphone.gallery.exportManaged", json!({"format": "archive", "name": "Bad/Name", "chain": 0, "fps": 6, "looping": true}))
        .unwrap_err();
    assert!(!err.is_empty());
    assert_eq!(std::fs::read(&kept).unwrap(), kept_bytes);

    // Prepare a ManagedExportStage and drop without publishing (simulates worker cancellation mid-write).
    {
        let stage = ManagedExportStage::prepare(&custom_exports, "Aborted", ExportFormat::Png).unwrap();
        std::fs::write(stage.staged_output_path(), b"partial-not-published").unwrap();
    }
    assert!(!custom_exports.join("Aborted.png").exists());
    let leftover = std::fs::read_dir(&custom_exports).unwrap().flatten().any(|e| e.file_name().to_string_lossy().starts_with(".komaphone-export-"));
    assert!(!leftover, "dropped stage must remove temporary directory");
}

#[cfg(windows)]
#[test]
fn windows_junctions_cannot_escape_managed_exports_or_reveal_targets() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    let secret = outside.join("secret.png");
    std::fs::write(&secret, b"secret").unwrap();

    // 1. Exports folder itself as a junction must be refused before creating staging or writing anything.
    let junction_exports = dir.path().join("ExportsJunction");
    let mk = std::process::Command::new("cmd").args(["/C", "mklink", "/J", &junction_exports.to_string_lossy(), &outside.to_string_lossy()]).output().unwrap();
    if !mk.status.success() {
        return;
    }

    let data_dir = dir.path().join("data");
    let mut session = open_session(&data_dir, Some(junction_exports.clone()));
    save_drawing_to_gallery(&mut session, "#aa2233", "JunctionTest");
    let err = gallery(&mut session, "komaphone.gallery.exportManaged", json!({"format": "png", "name": "JunctionTest", "chain": 0, "fps": 6, "looping": true}))
        .unwrap_err();
    assert!(!err.is_empty(), "junction Exports folder must fail cleanly");
    assert!(!outside.join("JunctionTest.png").exists(), "junction escape wrote outside file");

    // 2. Real Exports folder with an interior junction must reject reveal target validation.
    let real_exports = dir.path().join("RealExports");
    std::fs::create_dir_all(&real_exports).unwrap();
    let inner_junction = real_exports.join("EscapeDir");
    let mk2 = std::process::Command::new("cmd").args(["/C", "mklink", "/J", &inner_junction.to_string_lossy(), &outside.to_string_lossy()]).output().unwrap();
    assert!(mk2.status.success());

    assert!(validate_reveal_target(&real_exports, &inner_junction, true).is_err());
    assert!(validate_reveal_target(&real_exports, &inner_junction.join("secret.png"), false).is_err());
    assert!(validate_reveal_target(&real_exports, &secret, false).is_err());

    let clean_file = real_exports.join("Valid.png");
    std::fs::write(&clean_file, b"ok").unwrap();
    let canonical = validate_reveal_target(&real_exports, &clean_file, false).unwrap();
    let display = strip_windows_verbatim(&canonical);
    assert!(!display.starts_with(r"\\?\"));
    assert!(display.ends_with("Valid.png"));
}
