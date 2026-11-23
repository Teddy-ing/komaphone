#![cfg(not(target_arch = "wasm32"))]

use std::collections::BTreeMap;

use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
use photocraft_format::{PcraftWriter, SaveOptions};
use photocraft_local_store::gallery::{GalleryKind, GalleryStore, decode_archive, encode_archive, read_archive, write_archive};
use photocraft_local_store::{Error, StoreOptions};
use photocraft_party::gallery::{GALLERY_SCHEMA, GalleryBundle, GalleryContent, GalleryManifest, RemixProvenance};
use photocraft_party::{AssetHash, AssetRef, DeviceId, Participant, ParticipantId, ProjectId};
use sha2::{Digest, Sha256};

fn project(value: u32) -> ProjectId {
    ProjectId::new(&format!("{value:032x}")).unwrap()
}

fn author() -> Participant {
    Participant {
        id: ParticipantId::new("11111111111111111111111111111111").unwrap(),
        device: DeviceId::new("22222222222222222222222222222222").unwrap(),
        display_name: "Test artist".into(),
    }
}

fn bundle(id: u32, depth: SampleType) -> GalleryBundle {
    let doc = Document::with_background("Original drawing", Size { width: 32, height: 24 }, ColorMode::Rgb, depth, Color::rgb(0.2, 0.4, 0.8));
    let (bytes, _) = PcraftWriter::new().save_zip(&doc, &SaveOptions::default()).unwrap();
    from_bytes(id, bytes)
}

fn from_bytes(id: u32, bytes: Vec<u8>) -> GalleryBundle {
    let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
    let asset = AssetRef { hash, bytes: bytes.len() as u64 };
    GalleryBundle {
        manifest: GalleryManifest {
            schema_version: GALLERY_SCHEMA,
            id: project(id),
            title: "Original drawing".into(),
            created_at: "2026-11-06T00:00:00.000Z".into(),
            updated_at: "2026-11-06T00:00:00.000Z".into(),
            authors: vec![author()],
            content: GalleryContent::Project { checkpoint: asset, replay: None, direct_assets: Vec::new() },
            provenance: None,
            notes: Vec::new(),
            fps: 6,
            looping: true,
        },
        objects: BTreeMap::from([(hash, bytes)]),
    }
}

fn incomplete_game() -> GalleryBundle {
    use photocraft_party::room::{Assignment, MemberView, Phase, RevealedEntry, RoomView};
    let first = author();
    let mut second = author();
    second.id = ParticipantId::new("33333333333333333333333333333333").unwrap();
    second.device = DeviceId::new("44444444444444444444444444444444").unwrap();
    second.display_name = "Second artist".into();
    let authors = [first, second];
    let game = photocraft_party::GameId::new("55555555555555555555555555555555").unwrap();
    let reveal = (0..2u8)
        .flat_map(|chain| {
            let authors = &authors;
            (0..3u8).map(move |round| RevealedEntry::Missing {
                assignment: Assignment {
                    id: photocraft_party::AssignmentId { game, chain, round },
                    player: authors[usize::from((chain + round) % 2)].id,
                    project: project(100 + u32::from(chain) * 3 + u32::from(round)),
                },
                reason: "Host ended incomplete game".into(),
            })
        })
        .collect();
    let room = RoomView {
        id: photocraft_party::RoomId::new("66666666666666666666666666666666").unwrap(),
        revision: 9,
        epoch: 2,
        phase: Phase::Archived,
        round: 0,
        mode: photocraft_party::Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: authors[0].id,
        members: authors.iter().map(|participant| MemberView { participant: participant.clone(), connected: true, ready: true }).collect(),
        waiting: Vec::new(),
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    };
    GalleryBundle {
        manifest: GalleryManifest::from_reveal(project(10), &room, authors[0].id, "2026-11-06T00:00:00.000Z".into()).unwrap(),
        objects: BTreeMap::new(),
    }
}

#[test]
fn incomplete_games_survive_gallery_archive_and_keep_explicit_missing_parts() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = GalleryStore::open(dir.path().join("gallery.sqlite3"), StoreOptions::default()).unwrap();
    let original = incomplete_game();
    let saved = store.insert(&original).unwrap();
    assert_eq!(saved.kind, GalleryKind::ExquisiteCorpse);
    assert!(!saved.complete);
    assert_eq!(saved.authors.len(), 2);
    let restored = decode_archive(&encode_archive(&store.load(saved.id).unwrap()).unwrap()).unwrap();
    let GalleryContent::Game { room } = restored.manifest.content else { panic!("game expected") };
    assert_eq!(room.reveal.len(), 6);
    assert!(room.reveal.iter().all(|entry| matches!(entry, photocraft_party::room::RevealedEntry::Missing { .. })));
}

fn game_with_artwork(native: GalleryBundle) -> GalleryBundle {
    use photocraft_party::room::RevealedEntry;
    let GalleryContent::Project { checkpoint, .. } = native.manifest.content else { panic!("project expected") };
    let checkpoint =
        photocraft_party::CheckpointRef { id: photocraft_party::CheckpointId::new("77777777777777777777777777777777").unwrap(), asset: checkpoint };
    let mut game = incomplete_game();
    game.objects = native.objects;
    // The raster's opaque bytes are sufficient for this native-boundary regression;
    // the engine separately validates canonical image decoding and final pixels.
    let bytes = b"distinct rendered image asset".to_vec();
    let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap();
    let image = AssetRef { hash, bytes: bytes.len() as u64 };
    game.objects.insert(hash, bytes);
    let GalleryContent::Game { room } = &mut game.manifest.content else { panic!("game expected") };
    let RevealedEntry::Missing { assignment, .. } = room.reveal[0].clone() else { panic!("missing expected") };
    room.reveal[0] = RevealedEntry::Artwork {
        assignment,
        revision: 1,
        image,
        checkpoint,
        replay: Box::new(photocraft_party::ReplayRef {
            id: photocraft_party::ReplayId::new("88888888888888888888888888888888").unwrap(),
            manifest: None,
            baseline: checkpoint,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
        }),
        assets: vec![image, checkpoint.asset],
    };
    game.validate().unwrap();
    game
}

#[test]
fn native_game_checkpoint_dimensions_cannot_exceed_assignment_boundary() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = GalleryStore::open(dir.path().join("gallery.sqlite3"), StoreOptions::default()).unwrap();
    let game = game_with_artwork(bundle(1, SampleType::U8));
    assert!(store.insert(&game).unwrap_err().to_string().contains("dimensions"));
    assert!(store.list(true).unwrap().is_empty());
}

#[test]
fn explicitly_requested_notes_are_separate_durable_authorized_archive_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gallery.sqlite3");
    let doc = Document::with_background("Valid band", Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let (bytes, _) = PcraftWriter::new().save_zip(&doc, &SaveOptions::default()).unwrap();
    let game = game_with_artwork(from_bytes(1, bytes));
    let GalleryContent::Game { room } = &game.manifest.content else { panic!("game expected") };
    let photocraft_party::room::RevealedEntry::Artwork { assignment, .. } = &room.reveal[0] else { panic!("artwork expected") };
    let note = (assignment.id, "Keep the tiny red star above the hat".to_string());
    let mut store = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    store.insert(&game).unwrap();
    assert!(store.load(game.manifest.id).unwrap().manifest.notes.is_empty());
    store.set_notes(game.manifest.id, std::slice::from_ref(&note)).unwrap();
    let mut forbidden = note.clone();
    forbidden.0.round = 1;
    assert!(store.set_notes(game.manifest.id, &[forbidden]).is_err(), "a missing part has no authorized handoff note");
    assert!(store.set_notes(game.manifest.id, &[note.clone(), note.clone()]).is_err());
    assert_eq!(store.insert(&game).unwrap().id, game.manifest.id, "capture retries are independent of requested notes");
    let archive = dir.path().join("notes.komaphone");
    store.export_archive(game.manifest.id, &archive).unwrap();
    let backup = dir.path().join("notes.backup");
    store.backup(&backup).unwrap();
    drop(store);
    let store = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(store.load(game.manifest.id).unwrap().manifest.notes, vec![note.clone()]);
    assert_eq!(read_archive(&archive).unwrap().manifest.notes, vec![note.clone()]);
    let mut restored = GalleryStore::open(dir.path().join("restored.sqlite3"), StoreOptions::default()).unwrap();
    restored.restore_backup(&backup).unwrap();
    assert_eq!(restored.load(game.manifest.id).unwrap().manifest.notes, vec![note]);
    assert_eq!(restored.load(game.manifest.id).unwrap().objects, game.objects, "notes never alter native artwork or raster objects");
}

#[test]
fn editable_originals_roundtrip_at_every_depth_after_restart_and_archive() {
    for (index, depth) in [SampleType::U8, SampleType::U16, SampleType::F32].into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let original = bundle(index as u32 + 1, depth);
        let database = dir.path().join("gallery.sqlite3");
        let archive = dir.path().join("drawing.komaphone");
        let mut store = GalleryStore::open(&database, StoreOptions::default()).unwrap();
        let ack = store.insert(&original).unwrap();
        assert_eq!(ack.kind, GalleryKind::Project);
        assert_eq!(ack.authors, vec![author()]);
        assert_eq!(store.insert(&original).unwrap(), ack);
        store.export_archive(ack.id, &archive).unwrap();
        drop(store);
        let reopened = GalleryStore::open(&database, StoreOptions::default()).unwrap();
        let loaded = reopened.load(ack.id).unwrap();
        assert_eq!(loaded.objects, original.objects);
        assert_eq!(loaded.manifest.authors, original.manifest.authors);
        let mut fresh = GalleryStore::open(dir.path().join("fresh.sqlite3"), StoreOptions::default()).unwrap();
        assert_eq!(fresh.import_archive(&archive).unwrap().id, ack.id);
        assert_eq!(fresh.load(ack.id).unwrap().objects, original.objects);
        let restored = read_archive(&archive).unwrap();
        let GalleryContent::Project { checkpoint, .. } = restored.manifest.content else { panic!("project expected") };
        let doc = photocraft_format::load_from_bytes(restored.objects.get(&checkpoint.hash).unwrap()).unwrap();
        assert_eq!(doc.depth, depth);
        assert_eq!(doc.size, Size { width: 32, height: 24 });
        assert_eq!(doc.layers.len(), 1);
    }
}

#[test]
fn rename_favorite_trash_restore_preserve_original_and_independent_remix() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gallery.sqlite3");
    let original = bundle(1, SampleType::U16);
    let mut remix = original.clone();
    remix.manifest.id = project(2);
    remix.manifest.title = "Remixed drawing".into();
    remix.manifest.provenance =
        Some(RemixProvenance { source_entry: project(1), source_game: None, source_revision: 1, chain: 0, frame: Some(0), authors: vec![author()] });
    let mut store = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    store.insert(&original).unwrap();
    store.insert(&remix).unwrap();
    store.update(project(1), Some("My original"), Some(true), Some(true)).unwrap();
    assert_eq!(store.list(false).unwrap().len(), 1);
    let all = store.list(true).unwrap();
    let original_info = all.iter().find(|entry| entry.id == project(1)).unwrap();
    assert_eq!(original_info.title, "My original");
    assert!(original_info.favorite && original_info.trashed);
    assert_eq!(store.load(project(2)).unwrap().objects, original.objects);
    assert_eq!(store.insert(&original).unwrap().title, "My original", "retry cannot replace renamed metadata");
    store.update(project(1), None, None, Some(false)).unwrap();
    store.update(project(2), None, None, Some(true)).unwrap();
    drop(store);
    let store = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(store.list(false).unwrap().len(), 1);
    assert_eq!(store.load(project(1)).unwrap().objects, original.objects);
    assert_eq!(store.load(project(2)).unwrap().manifest.provenance.unwrap().source_entry, project(1));
}

#[test]
fn backup_restores_metadata_trash_shared_assets_and_draft_preferences_atomically() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = GalleryStore::open(dir.path().join("source.sqlite3"), StoreOptions::default()).unwrap();
    let original = bundle(1, SampleType::U8);
    let mut second = original.clone();
    second.manifest.id = project(2);
    store.insert(&original).unwrap();
    store.insert(&second).unwrap();
    store.update(project(2), Some("Remember this one"), Some(true), Some(true)).unwrap();
    store.set_playback(project(2), 12, false).unwrap();
    store.update_draft(project(5), Some("Private local draft"), Some(true), Some(true)).unwrap();
    store.set_draft_playback(project(5), 18, false).unwrap();
    let backup = dir.path().join("backup.komaphone-backup");
    store.backup(&backup).unwrap();
    let mut fresh = GalleryStore::open(dir.path().join("restored.sqlite3"), StoreOptions::default()).unwrap();
    let restored = fresh.restore_backup(&backup).unwrap();
    assert_eq!(restored.len(), 2);
    assert_eq!(fresh.list(true).unwrap(), store.list(true).unwrap());
    assert_eq!(fresh.draft_preferences().unwrap(), store.draft_preferences().unwrap());
    assert_eq!(fresh.draft_preferences().unwrap()[0].fps, Some(18));
    assert_eq!(fresh.draft_preferences().unwrap()[0].looping, Some(false));
    assert_eq!(fresh.load(project(1)).unwrap().objects, original.objects);
    assert_eq!(fresh.load(project(2)).unwrap().objects, original.objects);
    assert_eq!(fresh.load(project(2)).unwrap().manifest.fps, 12);
    assert!(!fresh.load(project(2)).unwrap().manifest.looping);
    assert!(fresh.restore_backup(&backup).is_err(), "never overwrite a nonempty store");
    let connection = rusqlite::Connection::open(dir.path().join("restored.sqlite3")).unwrap();
    let count: u32 = connection.query_row("SELECT count(*) FROM objects", [], |row| row.get(0)).unwrap();
    assert_eq!(count, 1, "shared immutable assets deduplicate without shared mutation");
    let bytes = std::fs::read(&backup).unwrap();
    let broken = dir.path().join("broken.backup");
    std::fs::write(&broken, &bytes[..bytes.len() - 3]).unwrap();
    let mut empty = GalleryStore::open(dir.path().join("failed.sqlite3"), StoreOptions::default()).unwrap();
    assert!(empty.restore_backup(&broken).is_err());
    assert!(empty.list(true).unwrap().is_empty());
    assert!(empty.draft_preferences().unwrap().is_empty());
    assert_eq!(empty.restore_backup(&backup).unwrap().len(), 2);
}

#[test]
fn conflicting_import_creates_independent_identity_and_preserves_both_versions() {
    let dir = tempfile::tempdir().unwrap();
    let original = bundle(1, SampleType::U8);
    let mut older = original.clone();
    older.manifest.title = "Older portable copy".into();
    let archive = dir.path().join("older.komaphone");
    write_archive(&older, &archive).unwrap();
    let mut store = GalleryStore::open(dir.path().join("gallery.sqlite3"), StoreOptions::default()).unwrap();
    store.insert(&original).unwrap();
    let imported = store.import_archive(&archive).unwrap();
    assert_ne!(imported.id, original.manifest.id);
    assert_eq!(imported.imported_from, Some(original.manifest.id));
    assert_eq!(store.load(project(1)).unwrap().manifest.title, "Original drawing");
    assert_eq!(store.load(imported.id).unwrap().manifest.title, "Older portable copy");
    assert_eq!(store.list(true).unwrap().len(), 2);
    store.update(imported.id, Some("Renamed imported original"), None, None).unwrap();
    let retry = store.import_archive(&archive).unwrap();
    assert_eq!(retry.id, imported.id, "retries after a conflict do not create another copy");
    assert_eq!(retry.title, "Renamed imported original");
    assert_eq!(store.list(true).unwrap().len(), 2);
}

#[test]
fn source_cache_recovers_legacy_metadata_without_reading_artwork_and_rejects_false_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gallery.sqlite3");
    let mut original = bundle(2, SampleType::U8);
    original.manifest.provenance =
        Some(RemixProvenance { source_entry: project(1), source_game: None, source_revision: 1, chain: 0, frame: None, authors: vec![author()] });
    let mut store = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(store.insert(&original).unwrap().source_projects, vec![project(1), project(2)]);
    let db = rusqlite::Connection::open(&path).unwrap();
    let bytes: Vec<u8> = db.query_row("SELECT info FROM entries WHERE id=?1", [project(2).as_str()], |row| row.get(0)).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    legacy.as_object_mut().unwrap().remove("source_projects");
    let legacy = serde_json::to_vec(&legacy).unwrap();
    db.execute("UPDATE entries SET info=?1,info_hash=?2 WHERE id=?3", rusqlite::params![legacy, format!("{:x}", Sha256::digest(&legacy)), project(2).as_str()])
        .unwrap();
    assert_eq!(store.list(true).unwrap()[0].source_projects, vec![project(1), project(2)]);
    assert_eq!(store.update(project(2), None, Some(true), None).unwrap().source_projects, vec![project(1), project(2)]);

    let archive = encode_archive(&original).unwrap();
    let length = u64::from_le_bytes(archive[12..20].try_into().unwrap()) as usize;
    let mut entry: serde_json::Value = serde_json::from_slice(&archive[52..52 + length]).unwrap();
    // Recompute framing and hash: validation must reject a structurally valid
    // archive whose new cache conceals the local remix source identity.
    entry["info"]["source_projects"] = serde_json::json!([project(2)]);
    let metadata = serde_json::to_vec(&entry).unwrap();
    let mut changed = archive[..12].to_vec();
    changed.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
    changed.extend_from_slice(&Sha256::digest(&metadata));
    changed.extend_from_slice(&metadata);
    changed.extend_from_slice(&archive[52 + length..]);
    assert!(decode_archive(&changed).is_err());
    let false_cache = dir.path().join("false-source.komarchive");
    std::fs::write(&false_cache, changed).unwrap();
    let mut fresh = GalleryStore::open(dir.path().join("fresh.sqlite3"), StoreOptions::default()).unwrap();
    assert!(fresh.import_archive(&false_cache).is_err());
    assert!(fresh.list(true).unwrap().is_empty());

    // Old archives omit the field and are upgraded from the complete manifest.
    entry["info"].as_object_mut().unwrap().remove("source_projects");
    let metadata = serde_json::to_vec(&entry).unwrap();
    let mut legacy_archive = archive[..12].to_vec();
    legacy_archive.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
    legacy_archive.extend_from_slice(&Sha256::digest(&metadata));
    legacy_archive.extend_from_slice(&metadata);
    legacy_archive.extend_from_slice(&archive[52 + length..]);
    let old = dir.path().join("legacy.komarchive");
    std::fs::write(&old, legacy_archive).unwrap();
    assert_eq!(fresh.import_archive(&old).unwrap().source_projects, vec![project(1), project(2)]);

    // Restore the old metadata and damage an artwork chunk. Listing must still
    // recover the authorization cache without decoding/loading any artwork.
    db.execute("UPDATE entries SET info=?1,info_hash=?2 WHERE id=?3", rusqlite::params![legacy, format!("{:x}", Sha256::digest(&legacy)), project(2).as_str()])
        .unwrap();
    db.execute("DELETE FROM chunks", []).unwrap();
    assert_eq!(store.list(true).unwrap()[0].source_projects, vec![project(1), project(2)]);
    assert!(store.load(project(2)).is_err());
}

#[test]
fn checkpoint_only_conflict_copies_keep_original_source_through_reimport_and_backup() {
    let dir = tempfile::tempdir().unwrap();
    let original = bundle(1, SampleType::U8);
    let mut portable = original.clone();
    portable.manifest.title = "First portable variation".into();
    let archive = dir.path().join("source.komarchive");
    write_archive(&portable, &archive).unwrap();
    let mut source = GalleryStore::open(dir.path().join("source.sqlite3"), StoreOptions::default()).unwrap();
    source.insert(&original).unwrap();
    let copy = source.import_archive(&archive).unwrap();
    assert_eq!(copy.imported_from, Some(original.manifest.id));
    assert!(copy.source_projects.contains(&original.manifest.id));
    assert!(copy.source_projects.contains(&copy.id));
    let exported = dir.path().join("copy.komarchive");
    source.export_archive(copy.id, &exported).unwrap();
    let mut destination = GalleryStore::open(dir.path().join("destination.sqlite3"), StoreOptions::default()).unwrap();
    let mut conflicting = source.load(copy.id).unwrap();
    conflicting.manifest.title = "Different destination original".into();
    destination.insert(&conflicting).unwrap();
    let twice = destination.import_archive(&exported).unwrap();
    assert_ne!(twice.id, copy.id);
    assert_eq!(twice.imported_from, Some(original.manifest.id), "conflict copies must retain the first source identity");
    assert!(twice.source_projects.contains(&original.manifest.id));
    assert!(twice.source_projects.contains(&twice.id));
    assert_eq!(destination.import_archive(&exported).unwrap().id, twice.id, "root source retention must not break retry idempotence");
    let backup = dir.path().join("backup.komabackup");
    destination.backup(&backup).unwrap();
    let mut restored = GalleryStore::open(dir.path().join("restored.sqlite3"), StoreOptions::default()).unwrap();
    restored.restore_backup(&backup).unwrap();
    assert_eq!(restored.list(true).unwrap().into_iter().find(|info| info.id == twice.id).unwrap().source_projects, twice.source_projects);
}

#[test]
fn caller_validation_runs_inside_import_restore_transaction_and_rolls_back_everything() {
    let dir = tempfile::tempdir().unwrap();
    let original = bundle(1, SampleType::U8);
    let archive = dir.path().join("portable.komaphone");
    write_archive(&original, &archive).unwrap();
    let mut store = GalleryStore::open(dir.path().join("gallery.sqlite3"), StoreOptions::default()).unwrap();
    let mut invoked = false;
    let failure = store.import_archive_checked(&archive, |loaded| {
        invoked = true;
        assert_eq!(loaded.objects, original.objects);
        Err(Error::Invalid("canonical pixels differ".into()))
    });
    assert!(invoked && failure.is_err());
    assert!(store.list(true).unwrap().is_empty());
    store.import_archive(&archive).unwrap();
    assert!(store.import_archive_checked(&archive, |_| Err(Error::Invalid("idempotent imports are validated too".into()))).is_err());
    let backup = dir.path().join("portable.backup");
    store.backup(&backup).unwrap();
    let mut empty = GalleryStore::open(dir.path().join("empty.sqlite3"), StoreOptions::default()).unwrap();
    assert!(empty.restore_backup_checked(&backup, |_| Err(Error::Invalid("canonical pixels differ".into()))).is_err());
    assert!(empty.list(true).unwrap().is_empty());
    empty.restore_backup(&backup).unwrap();
    assert_eq!(empty.load(original.manifest.id).unwrap().objects, original.objects);
}

#[test]
fn draft_playback_preferences_never_modify_same_identity_immutable_original() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gallery.sqlite3");
    let original = bundle(1, SampleType::U8);
    let mut store = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    store.insert(&original).unwrap();
    store.set_playback(original.manifest.id, 9, true).unwrap();
    store.set_draft_playback(original.manifest.id, 17, false).unwrap();
    assert!(store.set_draft_playback(original.manifest.id, 0, true).is_err());
    assert!(store.set_draft_playback(original.manifest.id, 25, true).is_err());
    store.update_draft(original.manifest.id, Some("Renamed private draft"), Some(true), None).unwrap();
    drop(store);
    let reopened = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    let saved = reopened.load(original.manifest.id).unwrap();
    assert_eq!(saved.manifest.fps, 9);
    assert!(saved.manifest.looping);
    assert_eq!(saved.manifest.title, "Original drawing");
    let preferences = reopened.draft_preferences().unwrap();
    assert_eq!(preferences[0].fps, Some(17));
    assert_eq!(preferences[0].looping, Some(false));
    assert_eq!(preferences[0].title.as_deref(), Some("Renamed private draft"));
}

#[test]
fn malformed_truncated_wrong_hash_future_version_and_trailing_archives_are_rejected() {
    let original = bundle(1, SampleType::U8);
    let encoded = encode_archive(&original).unwrap();
    assert_eq!(decode_archive(&encoded).unwrap().objects, original.objects);
    for length in [0, 7, 12, encoded.len() / 2, encoded.len() - 1] {
        assert!(decode_archive(&encoded[..length]).is_err());
    }
    let mut changed = encoded.clone();
    changed[7] = 2;
    assert!(decode_archive(&changed).is_err());
    let mut changed = encoded.clone();
    changed[12..20].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(decode_archive(&changed).is_err());
    let mut changed = encoded.clone();
    let byte = changed.len() - 9;
    changed[byte] ^= 1;
    assert!(decode_archive(&changed).is_err());
    let mut changed = encoded;
    changed.push(0);
    assert!(decode_archive(&changed).is_err());
}

#[test]
fn corrupt_native_checkpoint_or_missing_dependency_cannot_receive_ack() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = GalleryStore::open(dir.path().join("gallery.sqlite3"), StoreOptions::default()).unwrap();
    let original = bundle(1, SampleType::U8);
    store.insert(&original).unwrap();
    let corrupt = from_bytes(2, b"not a native document despite a matching hash".to_vec());
    assert!(store.insert(&corrupt).is_err());
    assert!(encode_archive(&corrupt).is_err());
    let mut missing = original.clone();
    missing.manifest.id = project(3);
    missing.objects.clear();
    assert!(store.insert(&missing).is_err());
    assert_eq!(store.list(true).unwrap().len(), 1);
    assert_eq!(store.load(project(1)).unwrap().objects, original.objects);
}

#[test]
fn full_quota_rolls_back_new_entry_without_losing_previous_original() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gallery.sqlite3");
    let original = bundle(1, SampleType::U8);
    let mut store = GalleryStore::open(&path, StoreOptions { quota_bytes: 128 * 1024 }).unwrap();
    store.insert(&original).unwrap();
    let mut rejected = false;
    for id in 2..100 {
        let mut candidate = original.clone();
        candidate.manifest.id = project(id);
        match store.insert(&candidate) {
            Ok(_) => {}
            Err(Error::Full) => {
                rejected = true;
                assert!(store.load(project(id)).is_err());
                break;
            }
            Err(error) => panic!("unexpected error: {error}"),
        }
    }
    assert!(rejected);
    drop(store);
    let reopened = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(reopened.load(project(1)).unwrap().objects, original.objects);
}

#[test]
fn failed_exports_never_overwrite_existing_destination_and_leave_no_temporary_files() {
    let dir = tempfile::tempdir().unwrap();
    let original = bundle(1, SampleType::U8);
    let target = dir.path().join("kept.komaphone");
    std::fs::write(&target, b"existing user data").unwrap();
    assert!(write_archive(&original, &target).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"existing user data");
    assert!(write_archive(&original, dir.path().join("missing").join("failed.komaphone")).is_err());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn future_foreign_and_unversioned_databases_remain_unchanged() {
    for (index, pragma) in ["PRAGMA user_version=9;", "PRAGMA application_id=999;", "CREATE TABLE unexpected(value TEXT);"].into_iter().enumerate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(format!("unreadable-{index}.sqlite3"));
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection.execute_batch(pragma).unwrap();
        drop(connection);
        let before = std::fs::read(&path).unwrap();
        assert!(GalleryStore::open(&path, StoreOptions::default()).is_err());
        assert_eq!(std::fs::read(path).unwrap(), before);
    }
}

#[test]
fn corrupt_chunks_metadata_and_dates_are_actionable_without_overwriting_original() {
    let dir = tempfile::tempdir().unwrap();
    let original = bundle(1, SampleType::U8);
    let path = dir.path().join("gallery.sqlite3");
    let mut store = GalleryStore::open(&path, StoreOptions::default()).unwrap();
    store.insert(&original).unwrap();
    let mut bad_date = original.clone();
    bad_date.manifest.id = project(2);
    bad_date.manifest.created_at = "not a date".into();
    assert!(store.insert(&bad_date).is_err());
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute("UPDATE chunks SET data=x'00'", []).unwrap();
    assert!(store.load(project(1)).is_err());
    assert!(store.insert(&original).is_err(), "idempotent retry must not hide corruption");
    assert!(store.export_archive(project(1), dir.path().join("corrupt.komaphone")).is_err());
    assert!(!dir.path().join("corrupt.komaphone").exists());
    assert_eq!(store.list(true).unwrap().len(), 1, "damaged original is retained for recovery");
}

#[test]
fn save_project_updates_in_place_gcs_old_blobs_and_never_overwrites_games() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gallery.sqlite3");
    let mut store = GalleryStore::open(&path, StoreOptions::default()).unwrap();

    let first = bundle(1, SampleType::U8);
    let saved = store.save_project(&first, Some("My Sketch")).unwrap();
    assert_eq!(saved.title, "My Sketch");
    assert_eq!(saved.kind, GalleryKind::Project);
    assert!(!saved.trashed);

    // Move to Trash, then explicitly re-saving with updated pixels restores out of Trash
    // while keeping `created_at` and updating content in place without creating a second row.
    store.update(project(1), None, Some(true), Some(true)).unwrap();
    let mut updated_doc = Document::with_background("My Sketch", Size { width: 32, height: 24 }, ColorMode::Rgb, SampleType::U8, Color::rgb(0.9, 0.2, 0.1));
    updated_doc.layers[0].name = "Updated artwork".into();
    let (updated_bytes, _) = PcraftWriter::new().save_zip(&updated_doc, &SaveOptions::default()).unwrap();
    let second_version = from_bytes(1, updated_bytes);
    let resaved = store.save_project(&second_version, None).unwrap();
    assert_eq!(resaved.id, project(1));
    assert_eq!(resaved.title, "My Sketch");
    assert_eq!(resaved.created_at, saved.created_at);
    assert!(resaved.favorite);
    assert!(!resaved.trashed);
    assert_eq!(store.list(true).unwrap().len(), 1);
    assert_eq!(store.load(project(1)).unwrap().objects, second_version.objects);

    // Old unreferenced checkpoint blob was garbage-collected.
    let db = rusqlite::Connection::open(&path).unwrap();
    let obj_count: i64 = db.query_row("SELECT count(*) FROM objects", [], |r| r.get(0)).unwrap();
    assert_eq!(obj_count, 1);
    drop(db);

    // `save_project` must refuse to overwrite an existing Game entry.
    let game = incomplete_game();
    store.insert(&game).unwrap();
    let mut colliding = first.clone();
    colliding.manifest.id = game.manifest.id;
    assert!(matches!(store.save_project(&colliding, None), Err(Error::Conflict)));
}

#[test]
fn permanent_delete_requires_trash_preserves_shared_blobs_and_tracks_game_and_project_refs() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("gallery.sqlite3");
    let mut store = GalleryStore::open(&path, StoreOptions::default()).unwrap();

    let shared_a = bundle(1, SampleType::U8);
    let mut shared_b = shared_a.clone();
    shared_b.manifest.id = project(2);
    let unique_doc = Document::with_background("Unique", Size { width: 32, height: 24 }, ColorMode::Rgb, SampleType::U8, Color::rgb(0.1, 0.8, 0.3));
    let (unique_bytes, _) = PcraftWriter::new().save_zip(&unique_doc, &SaveOptions::default()).unwrap();
    let unique_c = from_bytes(3, unique_bytes);

    store.insert(&shared_a).unwrap();
    store.insert(&shared_b).unwrap();
    store.insert(&unique_c).unwrap();

    let checkpoints = store.saved_project_checkpoints().unwrap();
    assert_eq!(checkpoints.len(), 3);
    assert!(checkpoints.contains_key(&project(1)));

    // Untrashed entry cannot be permanently deleted directly.
    assert!(store.delete_entry(project(1)).is_err());

    // Trash `shared_a` and `unique_c`; permanently delete `shared_a`: shared blob stays for `shared_b`.
    store.update(project(1), None, None, Some(true)).unwrap();
    store.update(project(3), None, None, Some(true)).unwrap();
    assert!(store.delete_entry(project(1)).unwrap());
    assert_eq!(store.list(true).unwrap().len(), 2);
    assert_eq!(store.load(project(2)).unwrap().objects, shared_b.objects);

    // Empty trash removes `unique_c` and GCs its unique blob while keeping `shared_b` intact.
    let deleted = store.delete_trashed_entries().unwrap();
    assert_eq!(deleted, vec![project(3)]);
    assert_eq!(store.list(true).unwrap().len(), 1);
    let db = rusqlite::Connection::open(&path).unwrap();
    let obj_count: i64 = db.query_row("SELECT count(*) FROM objects", [], |r| r.get(0)).unwrap();
    assert_eq!(obj_count, 1);
    drop(db);

    // Covered game projects includes only Artwork entries from non-trashed games.
    let doc = Document::with_background("Band", Size { width: 1024, height: 512 }, ColorMode::Rgb, SampleType::U8, Color::WHITE);
    let (band_bytes, _) = PcraftWriter::new().save_zip(&doc, &SaveOptions::default()).unwrap();
    let game = game_with_artwork(from_bytes(50, band_bytes));
    store.insert(&game).unwrap();
    let covered = store.covered_game_projects().unwrap();
    assert_eq!(covered.len(), 1);
    assert!(covered.contains(&project(100)));
    // Trashing the game excludes it from `covered_game_projects`.
    store.update(game.manifest.id, None, None, Some(true)).unwrap();
    assert!(store.covered_game_projects().unwrap().is_empty());
}
