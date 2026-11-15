//! Independent worker-boundary regressions for capability-scoped gallery I/O.
//! The test adapter observes authorization and completion ordering; real path
//! confinement is tested by the automation adapter's filesystem tests.
#![cfg(not(target_arch = "wasm32"))]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use photocraft_engine::gallery::{GalleryAction, GalleryFileAccess, PreparedGalleryFiles};
use photocraft_engine::{EngineError, Result, Session, gallery_media};
use photocraft_local_store::StoreOptions;
use photocraft_party::room::{Action, Room};
use photocraft_party::{DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, RoomId};
use serde_json::{Value, json};

#[derive(Default)]
struct Observations {
    prepared: AtomicUsize,
    finished: AtomicUsize,
    staged: Mutex<Option<PathBuf>>,
}

struct ObservedAccess {
    observations: Arc<Observations>,
    target: PathBuf,
    fail_finish: bool,
    finishing: Option<mpsc::Sender<()>>,
    release: Mutex<Option<mpsc::Receiver<()>>>,
}

struct Prepared {
    observations: Arc<Observations>,
    stage: tempfile::TempDir,
    action: GalleryAction,
    target: PathBuf,
    fail_finish: bool,
    finishing: Option<mpsc::Sender<()>>,
    release: Option<mpsc::Receiver<()>>,
}

fn error(value: impl std::fmt::Display) -> EngineError {
    EngineError::Other(value.to_string())
}

impl GalleryFileAccess for ObservedAccess {
    fn prepare(&self, action: &GalleryAction) -> Result<Box<dyn PreparedGalleryFiles>> {
        self.observations.prepared.fetch_add(1, Ordering::SeqCst);
        let stage = tempfile::tempdir().map_err(error)?;
        *self.observations.staged.lock().unwrap() = Some(stage.path().to_path_buf());
        let path = stage.path().join("output.komarchive").to_string_lossy().into_owned();
        let GalleryAction::Export { format, chain, frame, fps, looping, .. } = action else {
            return Err(error("The observer only supports export"));
        };
        let action = GalleryAction::Export { format: format.clone(), path, chain: *chain, frame: *frame, fps: *fps, looping: *looping };
        Ok(Box::new(Prepared {
            observations: self.observations.clone(),
            stage,
            action,
            target: self.target.clone(),
            fail_finish: self.fail_finish,
            finishing: self.finishing.clone(),
            release: self.release.lock().unwrap().take(),
        }))
    }
}

impl PreparedGalleryFiles for Prepared {
    fn action(&self) -> GalleryAction {
        self.action.clone()
    }

    fn finish(&mut self) -> Result<()> {
        self.observations.finished.fetch_add(1, Ordering::SeqCst);
        if let Some(sender) = &self.finishing {
            sender.send(()).map_err(error)?;
        }
        if let Some(receiver) = &self.release {
            receiver.recv_timeout(Duration::from_secs(15)).map_err(error)?;
        }
        if self.fail_finish {
            return Err(error("Injected publication failure; existing destination preserved"));
        }
        // The engine really rendered/encoded the requested archive before this
        // point. Publication belongs to the adapter, not the gallery worker.
        let bytes = std::fs::read(self.stage.path().join("output.komarchive")).map_err(error)?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&self.target).map_err(error)?;
        std::io::Write::write_all(&mut file, &bytes).map_err(error)?;
        file.sync_all().map_err(error)
    }
}

fn access(target: PathBuf) -> (Arc<ObservedAccess>, Arc<Observations>) {
    let observations = Arc::new(Observations::default());
    let access = Arc::new(ObservedAccess { observations: observations.clone(), target, fail_finish: false, finishing: None, release: Mutex::new(None) });
    (access, observations)
}

fn settle(app: &mut Session) -> std::result::Result<(), String> {
    let started = Instant::now();
    loop {
        app.poll_local_projects();
        app.poll_gallery();
        if !app.gallery.busy() {
            return app.gallery.error.clone().map_or(Ok(()), Err);
        }
        if started.elapsed() >= Duration::from_secs(30) {
            return Err("Gallery did not finish".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn original(root: &Path) -> (Session, ProjectId) {
    let mut app = Session::new();
    app.enable_local_projects(root.join("drafts.sqlite3"), StoreOptions::default()).unwrap();
    app.execute("komaphone.project.new", json!({"width":32,"height":32,"name":"Scoped original"})).unwrap();
    app.execute("paint.stroke", json!({"points":[[5,16],[27,16]],"size":8,"color":"#2060c0"})).unwrap();
    app.flush_local_projects(Duration::from_secs(20)).unwrap();
    settle(&mut app).unwrap();
    let id = app.local_projects.status(app.active().unwrap().doc.id).project.unwrap();
    app.execute("komaphone.gallery.open", json!({"entry":id,"draft":true})).unwrap();
    settle(&mut app).unwrap();
    (app, id)
}

fn export_params() -> Value {
    json!({"format":"archive","path":"requested.komarchive","chain":0,"fps":6,"looping":true})
}

#[test]
fn invalid_scoped_commands_never_reach_file_preparation() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _) = original(root.path());
    let (files, observations) = access(root.path().join("published.komarchive"));
    for (id, params) in [
        ("file.new", json!({})),
        ("komaphone.gallery.remix", json!({"chain":0})),
        ("komaphone.gallery.export", json!({"format":"archive","path":"a","chain":0,"fps":0,"looping":true})),
        ("komaphone.gallery.export", json!({"format":"archive","path":"a","chain":0,"fps":25,"looping":true})),
        ("komaphone.gallery.export", json!({"format":"unknown","path":"a","chain":0,"fps":6,"looping":true})),
        ("komaphone.gallery.export", json!({"format":"archive","path":"","chain":0,"fps":6,"looping":true})),
        ("komaphone.gallery.export", json!({"format":"archive","path":"a","chain":0,"fps":6,"looping":true,"action":"backup"})),
        ("komaphone.gallery.import", json!({"path":7})),
        ("komaphone.gallery.backup", json!({"path":"a","unknown":true})),
        ("komaphone.gallery.restore", json!({"path":"a"})),
    ] {
        assert!(app.gallery_file_command(id, &params, files.clone()).is_err(), "accepted invalid {id}: {params}");
        assert!(!app.gallery.busy());
    }
    assert_eq!(observations.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(observations.finished.load(Ordering::SeqCst), 0);
}

fn participant(n: u16) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 100)).unwrap(),
        display_name: format!("Player {n}"),
    }
}

fn active_game(app: &mut Session, assigned: ProjectId) {
    let host = participant(1);
    let guest = participant(2);
    let room_id = RoomId::new("11111111111111111111111111111111").unwrap();
    let game_id = GameId::new("22222222222222222222222222222222").unwrap();
    let mut room = Room::new(room_id, host.clone(), Mode::ExquisiteCorpse, None).unwrap();
    room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest.clone() }).unwrap();
    for player in [&host, &guest] {
        room.apply(player.id, room.revision(), 0, Action::Ready { ready: true }).unwrap();
    }
    let mut projects: Vec<_> = (0..6u16).map(|index| ProjectId::new(&format!("{:032x}", 1000 + index)).unwrap()).collect();
    projects[0] = assigned;
    let next = projects[1];
    room.apply(host.id, room.revision(), 0, Action::Start { game: game_id, projects }).unwrap();
    let mut view = room.view(host.id).unwrap();
    view.assignment.as_mut().unwrap().assignment.project = next;
    app.rooms.status.profile = Some(host);
    app.rooms.status.room = Some(view);
    app.rooms.status.game_drafts = Some(photocraft_room_client::GameDrafts { room: room_id, game: game_id, projects: vec![assigned, next] });
}

#[test]
fn scoped_file_requests_keep_phase_and_hidden_source_authorization() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, assigned) = original(root.path());
    let (files, observations) = access(root.path().join("forbidden.komarchive"));
    active_game(&mut app, assigned);
    for (id, params) in [
        ("komaphone.gallery.export", export_params()),
        ("komaphone.gallery.import", json!({"path":"import.komarchive"})),
        ("komaphone.gallery.restore", json!({"path":"backup","destination":"restored"})),
    ] {
        assert!(app.gallery_file_command(id, &params, files.clone()).is_err(), "bypassed game authorization: {id}");
    }
    assert_eq!(observations.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(observations.finished.load(Ordering::SeqCst), 0);
    assert!(!root.path().join("forbidden.komarchive").exists());
}

#[test]
fn worker_refuses_stale_selected_source_before_file_preparation() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, _) = original(root.path());
    let (files, observations) = access(root.path().join("forbidden.komarchive"));
    // The worker still holds the actually opened original. Simulate a stale
    // shell-side selection to exercise its independent identity check.
    let mut stale = (**app.gallery.active.as_ref().unwrap()).clone();
    stale.id = ProjectId::new("33333333333333333333333333333333").unwrap();
    app.gallery.active = Some(Arc::new(stale));
    app.gallery_file_command("komaphone.gallery.export", &export_params(), files).unwrap();
    assert!(settle(&mut app).unwrap_err().contains("selection changed"));
    assert_eq!(observations.prepared.load(Ordering::SeqCst), 0);
    assert_eq!(observations.finished.load(Ordering::SeqCst), 0);
}

#[test]
fn scoped_export_is_pending_until_final_publication_and_keeps_exact_artwork() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, id) = original(root.path());
    let canonical = app.gallery.image.as_ref().unwrap().clone();
    let target = root.path().join("published.komarchive");
    let observations = Arc::new(Observations::default());
    let (finishing_tx, finishing_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let files = Arc::new(ObservedAccess {
        observations: observations.clone(),
        target: target.clone(),
        fail_finish: false,
        finishing: Some(finishing_tx),
        release: Mutex::new(Some(release_rx)),
    });
    let prior_notice = app.gallery.notice.clone();
    let queued = app.gallery_file_command("komaphone.gallery.export", &export_params(), files).unwrap();
    assert_eq!(queued["pending"], true);
    assert_eq!(queued["saved"], false);
    finishing_rx.recv_timeout(Duration::from_secs(20)).unwrap();
    app.poll_gallery();
    assert!(app.gallery.busy(), "staging success is not publication success");
    assert_eq!(app.gallery.notice, prior_notice);
    assert!(!target.exists());
    release_tx.send(()).unwrap();
    settle(&mut app).unwrap();
    assert_eq!(observations.finished.load(Ordering::SeqCst), 1);
    let bundle = photocraft_local_store::gallery::read_archive(target).unwrap();
    assert_eq!(bundle.manifest.id, id);
    assert_eq!(gallery_media::render_frame(&bundle, 0, 0).unwrap(), *canonical);
    assert!(app.gallery.notice.as_ref().unwrap().contains("requested.komarchive"));
    assert!(!app.gallery.notice.as_ref().unwrap().contains("output.komarchive"));
    assert!(!observations.staged.lock().unwrap().as_ref().unwrap().exists());
}

#[test]
fn publication_failure_is_not_saved_and_retains_existing_file_and_original() {
    let root = tempfile::tempdir().unwrap();
    let (mut app, id) = original(root.path());
    let canonical = app.gallery.image.as_ref().unwrap().clone();
    let target = root.path().join("existing.komarchive");
    std::fs::write(&target, b"Existing user file").unwrap();
    let observations = Arc::new(Observations::default());
    let files =
        Arc::new(ObservedAccess { observations: observations.clone(), target: target.clone(), fail_finish: true, finishing: None, release: Mutex::new(None) });
    let prior_notice = app.gallery.notice.clone();
    app.gallery_file_command("komaphone.gallery.export", &export_params(), files).unwrap();
    assert!(settle(&mut app).unwrap_err().contains("Injected publication failure"));
    assert_eq!(app.gallery.notice, prior_notice);
    assert_eq!(std::fs::read(target).unwrap(), b"Existing user file");
    assert_eq!(app.gallery.active.as_ref().unwrap().id, id);
    assert_eq!(app.gallery.image.as_deref(), Some(canonical.as_ref()));
    assert_eq!(observations.prepared.load(Ordering::SeqCst), 1);
    assert_eq!(observations.finished.load(Ordering::SeqCst), 1);
    assert!(!observations.staged.lock().unwrap().as_ref().unwrap().exists());
}
