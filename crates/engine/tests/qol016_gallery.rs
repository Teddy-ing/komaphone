//! QOL-016 engine acceptance tests: explicit Save to gallery, Edit in Workroom round-trip,
//! permanent Trash delete & Empty Trash, Clear Temp, safe auto-pruning of untouched blanks
//! and saved-game round drafts, and legacy recovery checkpoint deduplication.
use std::path::Path;
use std::time::Duration;

use photocraft_engine::{Session, gallery_media};
use photocraft_format::{PcraftWriter, SaveOptions};
use photocraft_local_store::gallery::{GalleryKind, GalleryStore};
use photocraft_local_store::{Store, StoreOptions};
use photocraft_party::gallery::{GalleryBundle, GalleryManifest};
use photocraft_party::room::{Assignment, MemberView, Phase, RevealedEntry, RoomView};
use photocraft_party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RoomId,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn open_session(root: &Path) -> Session {
    let mut session = Session::new();
    session.enable_local_projects(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    wait_gallery(&mut session);
    session
}

fn wait_gallery(session: &mut Session) {
    let deadline = std::time::Instant::now() + Duration::from_secs(20);
    while session.gallery.busy() {
        assert!(std::time::Instant::now() < deadline, "gallery timed out");
        session.poll_gallery();
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn gallery(session: &mut Session, id: &str, params: Value) -> Result<Value, String> {
    let result = session.execute(id, params).map_err(|e| e.to_string())?;
    wait_gallery(session);
    if let Some(error) = session.gallery.error.take() {
        return Err(error);
    }
    Ok(result)
}

fn new_drawn_project(session: &mut Session, color: &str, name: &str) -> ProjectId {
    session.execute("komaphone.project.new", json!({"width": 64, "height": 64, "name": name})).unwrap();
    session.execute("paint.stroke", json!({"points": [[8, 12], [48, 48]], "size": 6, "color": color})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let doc_id = session.active().unwrap().doc.id;
    session.local_projects.status(doc_id).project.unwrap()
}

fn participant(seed: u8, name: &str) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{seed:02x}").repeat(16)).unwrap(),
        device: DeviceId::new(&format!("{seed:02x}").repeat(16)).unwrap(),
        display_name: name.into(),
    }
}

#[test]
fn save_to_gallery_and_edit_in_workroom_roundtrip_updates_in_place_without_duplicates() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let mut session = open_session(&root);

    let id = new_drawn_project(&mut session, "#2266dd", "Sketch A");
    gallery(&mut session, "komaphone.gallery.save", json!({})).unwrap();
    assert_eq!(session.gallery.items.iter().filter(|i| i.id == id && !i.draft).count(), 1);
    let open_draft = session.gallery.items.iter().find(|i| i.id == id && i.draft).unwrap();
    assert!(open_draft.saved_matches_head, "open draft immediately after save matches Saved head");
    assert!(!open_draft.has_unsaved_edits);

    // Draw another stroke in Workroom: draft diverges from Saved copy.
    session.execute("paint.stroke", json!({"points": [[10, 52], [54, 10]], "size": 5, "color": "#dd3322"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    gallery(&mut session, "komaphone.gallery.list", json!({})).unwrap();
    let diverged_draft = session.gallery.items.iter().find(|i| i.id == id && i.draft).unwrap();
    let saved_before = session.gallery.items.iter().find(|i| i.id == id && !i.draft).unwrap();
    assert!(diverged_draft.has_unsaved_edits);
    assert!(!diverged_draft.saved_matches_head);
    assert!(saved_before.has_unsaved_edits);

    // Save again with a new title: updates the SAME Saved entry in place (no duplicate).
    let expected_pixels = gallery_media::render_document(&session.active().unwrap().doc).unwrap();
    gallery(&mut session, "komaphone.gallery.save", json!({"title": "Hero Cat"})).unwrap();
    assert_eq!(session.gallery.items.iter().filter(|i| i.id == id && !i.draft).count(), 1);
    let saved_after = session.gallery.items.iter().find(|i| i.id == id && !i.draft).unwrap();
    assert_eq!(saved_after.title, "Hero Cat");
    assert_eq!(saved_after.kind, "project");
    assert!(!saved_after.has_unsaved_edits);

    // Closing the Workroom tab auto-prunes the redundant synced draft from drafts.sqlite3.
    session.close(0).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let local_store = Store::open(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    assert!(local_store.list().unwrap().is_empty(), "synced closed draft should be auto-pruned");
    drop(local_store);

    // Edit in Workroom restores full lossless replay history under the exact same ProjectId.
    gallery(&mut session, "komaphone.gallery.openEditable", json!({"entry": id})).unwrap();
    assert!(session.gallery.opened_document);
    assert_eq!(session.documents().len(), 1);
    let doc_id = session.active().unwrap().doc.id;
    let status = session.local_projects.status(doc_id);
    assert_eq!(status.project, Some(id));
    assert_eq!(status.local_revision, 3, "baseline + 2 strokes restored intact");
    assert_eq!(gallery_media::render_document(&session.active().unwrap().doc).unwrap(), expected_pixels);

    // Calling openEditable while already open activates the existing tab without duplicating.
    session.gallery.opened_document = false;
    gallery(&mut session, "komaphone.gallery.openEditable", json!({"entry": id})).unwrap();
    assert!(session.gallery.opened_document);
    assert_eq!(session.documents().len(), 1);

    // Promote a closed Temp draft from Gallery: immediately deletes its draft row and moves it to Saved.
    session.close(0).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let temp_id = new_drawn_project(&mut session, "#22aa44", "Temp Tree");
    session.close(0).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    gallery(&mut session, "komaphone.gallery.save", json!({"entry": temp_id})).unwrap();
    assert!(session.gallery.items.iter().any(|i| i.id == temp_id && !i.draft));
    assert!(!session.gallery.items.iter().any(|i| i.id == temp_id && i.draft), "closed promoted draft removed from Temp");
}

#[test]
fn trash_restore_permanent_delete_and_empty_trash_gc_and_protect_open_drafts() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let mut session = open_session(&root);

    let saved_id = new_drawn_project(&mut session, "#cc3311", "Saved Sunset");
    gallery(&mut session, "komaphone.gallery.save", json!({})).unwrap();
    session.close(0).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();

    let closed_temp_id = new_drawn_project(&mut session, "#11aa77", "Closed Doodle");
    session.close(0).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();

    let open_temp_id = new_drawn_project(&mut session, "#8844dd", "Open Doodle");

    // Permanent delete MUST refuse items that are not in Trash.
    assert!(gallery(&mut session, "komaphone.gallery.delete", json!({"entry": saved_id, "draft": false})).is_err());
    assert!(gallery(&mut session, "komaphone.gallery.delete", json!({"entry": closed_temp_id, "draft": true})).is_err());

    // Trash the saved entry, restore it, verify intact, then trash again.
    gallery(&mut session, "komaphone.gallery.update", json!({"entry": saved_id, "draft": false, "trashed": true})).unwrap();
    assert!(session.gallery.items.iter().find(|i| i.id == saved_id && !i.draft).unwrap().trashed);
    gallery(&mut session, "komaphone.gallery.update", json!({"entry": saved_id, "draft": false, "trashed": false})).unwrap();
    assert!(!session.gallery.items.iter().find(|i| i.id == saved_id && !i.draft).unwrap().trashed);
    gallery(&mut session, "komaphone.gallery.update", json!({"entry": saved_id, "draft": false, "trashed": true})).unwrap();

    // Trash both drafts (closed and open).
    gallery(&mut session, "komaphone.gallery.update", json!({"entry": closed_temp_id, "draft": true, "trashed": true})).unwrap();
    gallery(&mut session, "komaphone.gallery.update", json!({"entry": open_temp_id, "draft": true, "trashed": true})).unwrap();

    // Permanent delete of open trashed draft is refused until closed in Workroom.
    let err = gallery(&mut session, "komaphone.gallery.delete", json!({"entry": open_temp_id, "draft": true})).unwrap_err();
    assert!(err.contains("Close this drawing"), "unexpected error: {err}");

    // Single permanent delete of closed trashed draft succeeds.
    gallery(&mut session, "komaphone.gallery.delete", json!({"entry": closed_temp_id, "draft": true})).unwrap();
    assert!(!session.gallery.items.iter().any(|i| i.id == closed_temp_id));

    // Empty trash permanently deletes the trashed Saved item while honestly preserving the open trashed draft.
    gallery(&mut session, "komaphone.gallery.emptyTrash", json!({})).unwrap();
    assert!(!session.gallery.items.iter().any(|i| i.id == saved_id));
    assert!(session.gallery.items.iter().any(|i| i.id == open_temp_id && i.draft && i.trashed));
    let notice = session.gallery.notice.as_deref().unwrap_or("");
    assert!(notice.contains("Kept 1 open drawing"), "notice should explain skipped open draft: {notice}");

    // After closing the open draft, Empty Trash deletes it completely and leaves both stores empty.
    session.close(0).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    gallery(&mut session, "komaphone.gallery.emptyTrash", json!({})).unwrap();
    assert!(session.gallery.items.is_empty());

    let local_store = Store::open(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    assert!(local_store.list().unwrap().is_empty());
    let gallery_store = GalleryStore::open(root.join("gallery.sqlite3"), StoreOptions::default()).unwrap();
    assert!(gallery_store.list(true).unwrap().is_empty());
    assert!(gallery_store.draft_preferences().unwrap().is_empty());
}

#[test]
fn untouched_blanks_completed_game_rounds_and_clear_temp_keep_gallery_clean() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let mut session = open_session(&root);

    // 1. Blank `file.new` is marked untouched_blank while open, and auto-pruned when closed.
    session.execute("komaphone.project.new", json!({"width": 64, "height": 64})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let blank_id = session.local_projects.status(session.active().unwrap().doc.id).project.unwrap();
    gallery(&mut session, "komaphone.gallery.list", json!({})).unwrap();
    assert!(session.gallery.items.iter().find(|i| i.id == blank_id && i.draft).unwrap().untouched_blank);
    session.close(0).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    gallery(&mut session, "komaphone.gallery.list", json!({})).unwrap();
    assert!(!session.gallery.items.iter().any(|i| i.id == blank_id), "closed untouched blank auto-pruned");

    // 2. Clean-exit pruning removes an open untouched blank but preserves an open edited drawing.
    session.execute("komaphone.project.new", json!({"width": 64, "height": 64})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let exit_blank = session.local_projects.status(session.active().unwrap().doc.id).project.unwrap();
    let edited_id = new_drawn_project(&mut session, "#3355cc", "Real Work");
    session.prune_local_projects_on_exit(Duration::from_secs(10));
    let local_store = Store::open(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    let remaining: Vec<ProjectId> = local_store.list().unwrap().into_iter().map(|d| d.project).collect();
    assert!(!remaining.contains(&exit_blank), "exit prune removes untouched blank");
    assert!(remaining.contains(&edited_id), "exit prune preserves edited drawing");
    drop(local_store);

    // 3. Round draft covered by a saved Completed game is auto-pruned once closed.
    session.close(1).unwrap(); // close Real Work
    session.close(0).unwrap(); // close exit_blank tab
    let round_id = new_drawn_project(&mut session, "#dd7722", "Room drawing");
    let round_doc = session.active().unwrap().doc.clone();
    // Build a valid 2-player 1-rotation Game bundle referencing `round_id`.
    let mut objects = std::collections::BTreeMap::new();
    let mut round_doc_sized = round_doc.as_ref().clone();
    round_doc_sized.size = photocraft_doc::Size { width: 1536, height: 1024 };
    let (cp_bytes, _) = PcraftWriter::new().save_zip(&round_doc_sized, &SaveOptions::default()).unwrap();
    let cp_hash = AssetHash::new(&format!("{:x}", Sha256::digest(&cp_bytes))).unwrap();
    let cp_asset = AssetRef { hash: cp_hash, bytes: cp_bytes.len() as u64 };
    objects.insert(cp_hash, cp_bytes);
    let checkpoint = CheckpointRef { id: CheckpointId::new(&cp_hash.as_str()[..32]).unwrap(), asset: cp_asset };
    let preview_bytes = b"\x89PNG\r\n\x1a\nplaceholder".to_vec();
    let preview_hash = AssetHash::new(&format!("{:x}", Sha256::digest(&preview_bytes))).unwrap();
    let preview = AssetRef { hash: preview_hash, bytes: preview_bytes.len() as u64 };
    objects.insert(preview_hash, preview_bytes);
    let game_id = GameId::new(&"aa".repeat(16)).unwrap();
    let p1 = participant(1, "Alice");
    let p2 = participant(2, "Bob");
    let make_entry = |chain: u8, round: u8, player: ParticipantId, project: ProjectId| RevealedEntry::Artwork {
        assignment: Assignment { id: AssignmentId { game: game_id, chain, round }, player, project },
        revision: 2,
        image: preview,
        checkpoint,
        replay: Box::new(ReplayRef {
            id: ReplayId::new(project.as_str()).unwrap(),
            manifest: None,
            baseline: checkpoint,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
        }),
        assets: vec![preview, checkpoint.asset],
        extent: None,
    };
    let other_p1 = ProjectId::new(&"11".repeat(16)).unwrap();
    let other_p2 = ProjectId::new(&"22".repeat(16)).unwrap();
    let other_p3 = ProjectId::new(&"33".repeat(16)).unwrap();
    let room = RoomView {
        id: RoomId::new(&"bb".repeat(16)).unwrap(),
        revision: 10,
        epoch: 1,
        phase: Phase::Reveal,
        round: 1,
        mode: Mode::Animation { rotations: 1 },
        timer_ms: None,
        moderator: p1.id,
        members: vec![
            MemberView { participant: p1.clone(), connected: true, ready: true },
            MemberView { participant: p2.clone(), connected: true, ready: true },
        ],
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal: vec![
            make_entry(0, 0, p1.id, round_id),
            make_entry(0, 1, p2.id, other_p1),
            make_entry(1, 0, p2.id, other_p2),
            make_entry(1, 1, p1.id, other_p3),
        ],
    };
    let manifest = GalleryManifest::from_reveal(ProjectId::new(game_id.as_str()).unwrap(), &room, p1.id, "2026-11-22T12:00:00Z".into()).unwrap();
    let game_bundle = GalleryBundle { manifest, objects };
    {
        let mut gstore = GalleryStore::open(root.join("gallery.sqlite3"), StoreOptions::default()).unwrap();
        let info = gstore.insert(&game_bundle).unwrap();
        assert_eq!(info.kind, GalleryKind::Animation);
    }
    // While `round_id` is still open in Workroom, refresh keeps it in drafts.sqlite3 but tags covered_by_game = true.
    gallery(&mut session, "komaphone.gallery.list", json!({})).unwrap();
    assert!(session.gallery.items.iter().find(|i| i.id == round_id && i.draft).unwrap().covered_by_game);
    // Closing `round_id` and refreshing auto-prunes it completely.
    session.close(0).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    gallery(&mut session, "komaphone.gallery.list", json!({})).unwrap();
    assert!(!session.gallery.items.iter().any(|i| i.id == round_id && i.draft), "completed game round draft pruned");

    // 4. Clear temp moves closed unsaved working drafts to Trash reversibly while keeping open drafts.
    let open_keep = new_drawn_project(&mut session, "#00aa99", "Keep Open");
    gallery(&mut session, "komaphone.gallery.clearTemp", json!({})).unwrap();
    let edited_item = session.gallery.items.iter().find(|i| i.id == edited_id && i.draft).unwrap();
    assert!(edited_item.trashed, "Clear Temp moves closed unsaved draft to Trash reversibly");
    let open_item = session.gallery.items.iter().find(|i| i.id == open_keep && i.draft).unwrap();
    assert!(!open_item.trashed, "Clear Temp never trashes open Workroom drawing");
    // Restore from Trash succeeds.
    gallery(&mut session, "komaphone.gallery.update", json!({"entry": edited_id, "draft": true, "trashed": false})).unwrap();
    assert!(!session.gallery.items.iter().find(|i| i.id == edited_id && i.draft).unwrap().trashed);
}

#[test]
fn recovery_dedup_rebinds_identical_checkpoint_instead_of_cloning() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let mut session = open_session(&root);

    let orig_id = new_drawn_project(&mut session, "#ff2266", "Original Art");
    session.execute("paint.stroke", json!({"points": [[4, 4], [40, 20]], "size": 4, "color": "#2244ff"})).unwrap();
    session.execute("paint.stroke", json!({"points": [[20, 4], [20, 50]], "size": 4, "color": "#22aa44"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let orig_doc = session.active().unwrap().doc.as_ref().clone();
    assert_eq!(session.local_projects.status(orig_doc.id).local_revision, 4);
    session.close(0).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();

    // Simulate legacy `config/Recovery/` launch recovery of the exact same document.
    let mut recovered_doc = orig_doc.clone();
    recovered_doc.id = photocraft_doc::DocId::fresh();
    session.add_recovered_document(recovered_doc, None);
    session.flush_local_projects(Duration::from_secs(20)).unwrap();

    let rec_doc_id = session.active().unwrap().doc.id;
    let status = session.local_projects.status(rec_doc_id);
    assert_eq!(status.project, Some(orig_id), "recovered doc must bind to existing ProjectId without cloning");
    assert_eq!(status.local_revision, 4, "recovered doc inherits existing revision depth");

    let local_store = Store::open(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    assert_eq!(local_store.list().unwrap().len(), 1, "zero duplicate rows minted");
    drop(local_store);

    // Drawing another stroke continues seamlessly at revision 5 on the exact same project.
    session.execute("paint.stroke", json!({"points": [[5, 50], [50, 50]], "size": 3, "color": "#000000"})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(session.local_projects.status(rec_doc_id).local_revision, 5);
    let local_store = Store::open(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    assert_eq!(local_store.list().unwrap().len(), 1);
    assert_eq!(local_store.replay_page(orig_id, 0, 16).unwrap().len(), 5);
    drop(local_store);

    // A genuinely unseen recovered document still saves normally as a new draft.
    let brand_new = photocraft_doc::Document::new(
        "Orphan Recovery",
        photocraft_doc::Size { width: 32, height: 32 },
        photocraft_doc::ColorMode::Rgb,
        photocraft_doc::SampleType::U8,
    );
    session.add_recovered_document(brand_new, None);
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let local_store = Store::open(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    assert_eq!(local_store.list().unwrap().len(), 2);
}

#[test]
fn new_gallery_commands_reject_invalid_params_and_active_party_drawings() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("data");
    let mut session = open_session(&root);

    // Reject unknown fields, control chars, empty/overlong titles without panicking.
    for (cmd, params) in [
        ("komaphone.gallery.save", json!({"title": ""})),
        ("komaphone.gallery.save", json!({"title": "bad\ntitle"})),
        ("komaphone.gallery.save", json!({"title": "a".repeat(97)})),
        ("komaphone.gallery.save", json!({"unknown": 1})),
        ("komaphone.gallery.openEditable", json!({})),
        ("komaphone.gallery.openEditable", json!({"entry": "not-an-id"})),
        ("komaphone.gallery.delete", json!({})),
        ("komaphone.gallery.emptyTrash", json!({"extra": true})),
        ("komaphone.gallery.clearTemp", json!({"extra": true})),
    ] {
        assert!(session.execute(cmd, params).is_err(), "{cmd} should reject invalid params");
    }

    // Active party-room assignment cannot be saved as a solo gallery drawing.
    let assign_proj = new_drawn_project(&mut session, "#222222", "Room drawing");
    let p1 = participant(1, "Alice");
    let game_id = GameId::new(&"cc".repeat(16)).unwrap();
    let room_id = RoomId::new(&"dd".repeat(16)).unwrap();
    session.rooms.status.profile = Some(p1.clone());
    session.rooms.status.room = Some(RoomView {
        id: room_id,
        revision: 2,
        epoch: 1,
        phase: Phase::Drawing,
        round: 0,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: p1.id,
        members: vec![MemberView { participant: p1.clone(), connected: true, ready: true }],
        waiting: vec![],
        paused: None,
        deadline_ms: None,
        assignment: Some(photocraft_party::room::AssignmentView {
            assignment: Assignment { id: AssignmentId { game: game_id, chain: 0, round: 0 }, player: p1.id, project: assign_proj },
            submitted: false,
            reference: None,
        }),
        reveal: vec![],
    });
    let err = session.execute("komaphone.gallery.save", json!({})).unwrap_err().to_string();
    assert!(err.contains("Party room drawings save together"), "unexpected error: {err}");
}
