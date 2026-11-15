#![cfg(not(target_arch = "wasm32"))]
use photocraft_local_store::{
    Error, Result, StoreOptions,
    canvas::{
        CanvasStage, CanvasStore,
        linking::{LinkAuthority, LinkCredentials, LinkTarget},
    },
};
use photocraft_party::AssetHash;
use photocraft_scene::{Bookmark, OwnerId, RegionKey, Srgba, Stroke, StrokeId, shared::DeviceId};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

#[derive(Default)]
struct Credentials {
    values: BTreeMap<(String, String), String>,
    fail: bool,
}
impl LinkCredentials for Credentials {
    fn get(&mut self, namespace: &str, key: &str) -> Result<Option<String>> {
        Ok(self.values.get(&(namespace.into(), key.into())).cloned())
    }
    fn set(&mut self, namespace: &str, key: &str, secret: &str) -> Result<()> {
        if self.fail {
            return Err(Error::Storage("injected protected-store failure".into()));
        }
        self.values.insert((namespace.into(), key.into()), secret.into());
        Ok(())
    }
}
struct Authority {
    owner: OwnerId,
    target: LinkTarget,
    allow: bool,
    nonces: BTreeSet<String>,
}
impl LinkAuthority for Authority {
    fn authorize(&mut self, owner: OwnerId, nonce: &str) -> Result<LinkTarget> {
        if !self.allow || self.owner != owner || !self.nonces.insert(nonce.into()) || AssetHash::new(nonce).is_err() {
            return Err(Error::Invalid("test-only authority denied".into()));
        }
        Ok(self.target)
    }
}
fn authority(owner: OwnerId) -> Authority {
    Authority {
        owner,
        target: LinkTarget {
            service: AssetHash::new(&format!("{:064x}", 77)).unwrap(),
            profile: OwnerId::new(&format!("{:032x}", 88)).unwrap(),
            device: DeviceId::new(&format!("{:032x}", 99)).unwrap(),
        },
        allow: true,
        nonces: BTreeSet::new(),
    }
}
fn fixture(path: &Path) -> CanvasStore {
    let mut store = CanvasStore::open(path, StoreOptions::default()).unwrap();
    for n in 1..=2 {
        let mut info = store.create("Same local title").unwrap();
        for sequence in 0..3 {
            info = store
                .append(
                    &Stroke {
                        id: StrokeId::new(&format!("{:032x}", n * 10 + sequence)).unwrap(),
                        canvas: info.id,
                        owner: info.owner,
                        revision: 0,
                        anchor: RegionKey::default(),
                        points: vec![[500., 500.], [520., 520.]],
                        width: 3.,
                        color: Srgba { rgba: [0.2, 0.4, 0.6, 1.] },
                        erase: sequence == 2,
                    },
                    info.revision,
                )
                .unwrap();
        }
        info = store.undo(info.id, info.revision).unwrap();
        let camera = info.camera.clone();
        store.set_bookmarks(info.id, info.revision, vec![Bookmark { name: "Home".into(), camera }]).unwrap();
    }
    store
}
/// Compare every preexisting art/history/index/selection cell, not only counts.
fn art_rows(path: &Path) -> Vec<Vec<rusqlite::types::Value>> {
    let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut result = Vec::new();
    for query in [
        "SELECT * FROM identity ORDER BY singleton",
        "SELECT * FROM canvases ORDER BY id",
        "SELECT * FROM strokes ORDER BY canvas,id",
        "SELECT * FROM cells ORDER BY canvas,depth,x,y,source_depth,stroke",
    ] {
        let mut statement = db.prepare(query).unwrap();
        let columns = statement.column_count();
        let rows = statement.query_map([], |row| (0..columns).map(|index| row.get(index)).collect::<rusqlite::Result<Vec<_>>>()).unwrap();
        result.extend(rows.map(|row| row.unwrap()));
    }
    result
}

#[test]
fn enrollment_link_restart_and_rollback_preserve_every_original_art_row() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("canvas.sqlite3");
    let mut store = fixture(&path);
    let owner = store.owner();
    let original = art_rows(&path);
    assert!(store.local_binding().unwrap().is_none());
    let mut credentials = Credentials::default();
    let mut authority = authority(owner);
    let receipt = store.link_identity(&mut credentials, &mut authority, root.path().join("backups")).unwrap();
    assert!(receipt.binding.active);
    assert_eq!(receipt.binding.local_owner, owner);
    assert_ne!(receipt.binding.local_owner, receipt.binding.target.profile);
    assert_eq!(art_rows(&path), original);
    let before = receipt.backup_path.unwrap();
    let old = CanvasStore::open(&before, StoreOptions::default()).unwrap();
    assert!(old.local_binding().unwrap().is_none());
    assert_eq!(art_rows(&before), original);
    drop(old);
    let secret = credentials.values.values().next().unwrap().clone();
    assert!(!std::fs::read(&path).unwrap().windows(secret.len()).any(|part| part == secret.as_bytes()));
    let repeated = store.link_identity(&mut credentials, &mut authority, root.path().join("backups")).unwrap();
    assert_eq!(repeated.binding.revision, 1);
    assert!(repeated.backup_path.is_none());
    drop(store);
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    assert_eq!(store.binding_for(authority.target).unwrap(), Some(receipt.binding));
    assert_eq!(art_rows(&path), original);
    let rolled = store.rollback_link(&mut credentials, &mut authority, root.path().join("backups")).unwrap();
    assert!(!rolled.binding.active);
    assert_eq!(rolled.binding.revision, 2);
    assert!(store.binding_for(authority.target).unwrap().is_none());
    assert_eq!(art_rows(&path), original);
    let restored = CanvasStore::open(rolled.backup_path.unwrap(), StoreOptions::default()).unwrap();
    assert!(restored.local_binding().unwrap().unwrap().active);
    let relink = store.link_identity(&mut credentials, &mut authority, root.path().join("backups")).unwrap();
    assert_eq!(relink.binding.revision, 3);
    assert_eq!(art_rows(&path), original);
}

#[test]
fn wrong_account_writer_service_or_local_credential_never_reassigns_the_installation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("canvas.sqlite3");
    let mut store = fixture(&path);
    let original = art_rows(&path);
    let mut credentials = Credentials::default();
    let mut auth = authority(store.owner());
    let bound = store.link_identity(&mut credentials, &mut auth, root.path().join("backups")).unwrap().binding;
    for variant in 0..3 {
        let mut wrong = authority(store.owner());
        match variant {
            0 => wrong.target.profile = OwnerId::new(&format!("{:032x}", 111)).unwrap(),
            1 => wrong.target.device = DeviceId::new(&format!("{:032x}", 112)).unwrap(),
            _ => wrong.target.service = AssetHash::new(&format!("{:064x}", 113)).unwrap(),
        };
        assert!(store.link_identity(&mut credentials, &mut wrong, root.path().join("backups")).is_err());
        assert!(store.rollback_link(&mut credentials, &mut wrong, root.path().join("backups")).is_err());
        assert!(store.binding_for(wrong.target).unwrap().is_none());
    }
    let mut missing = Credentials::default();
    assert!(store.link_identity(&mut missing, &mut auth, root.path().join("backups")).is_err());
    let mut wrong_secret = Credentials { values: credentials.values.keys().map(|key| (key.clone(), format!("{:064x}", 700))).collect(), ..Default::default() };
    assert!(store.link_identity(&mut wrong_secret, &mut auth, root.path().join("backups")).is_err());
    assert_eq!(store.local_binding().unwrap(), Some(bound));
    assert_eq!(art_rows(&path), original);
    store.rollback_link(&mut credentials, &mut auth, root.path().join("backups")).unwrap();
    let mut other = authority(store.owner());
    other.target.profile = OwnerId::new(&format!("{:032x}", 200)).unwrap();
    assert!(store.link_identity(&mut credentials, &mut other, root.path().join("backups")).is_err());
    assert_eq!(art_rows(&path), original);
    // Signing out or losing login credentials never disables local editing.
    let local = store.create("Still works offline").unwrap();
    assert_eq!(local.owner, store.owner());
}

#[test]
fn denied_authority_and_protected_store_failure_leave_unlinked_drawing_recoverable() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("canvas.sqlite3");
    let mut store = fixture(&path);
    let original = art_rows(&path);
    let mut auth = authority(store.owner());
    let mut credentials = Credentials::default();
    auth.allow = false;
    assert!(store.link_identity(&mut credentials, &mut auth, root.path().join("backups")).is_err());
    assert!(credentials.values.is_empty());
    assert!(!root.path().join("backups").exists());
    auth.allow = true;
    credentials.fail = true;
    assert!(store.link_identity(&mut credentials, &mut auth, root.path().join("backups")).is_err());
    assert!(store.local_binding().unwrap().is_none());
    assert_eq!(art_rows(&path), original);
    credentials.fail = false;
    store.link_identity(&mut credentials, &mut auth, root.path().join("backups")).unwrap();
    assert_eq!(art_rows(&path), original);
}

#[test]
fn linking_transaction_failure_retains_last_binding_and_never_rolls_back_drawing() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("canvas.sqlite3");
    let mut store = fixture(&path);
    let mut auth = authority(store.owner());
    let mut credentials = Credentials::default();
    let initial = store.link_identity(&mut credentials, &mut auth, root.path().join("backups")).unwrap().binding;
    let extra = store.create("Created after linking").unwrap();
    let original = art_rows(&path);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch("CREATE TRIGGER fail_link BEFORE INSERT ON canvas_link_journal BEGIN SELECT RAISE(ABORT,'injected failure'); END;").unwrap();
    assert!(store.rollback_link(&mut credentials, &mut auth, root.path().join("backups")).is_err());
    assert_eq!(store.local_binding().unwrap(), Some(initial));
    assert_eq!(art_rows(&path), original);
    db.execute_batch("DROP TRIGGER fail_link").unwrap();
    store.rollback_link(&mut credentials, &mut auth, root.path().join("backups")).unwrap();
    assert_eq!(store.info(extra.id).unwrap(), extra);
    assert_eq!(art_rows(&path), original);
}

#[test]
fn future_or_corrupt_link_journal_is_rejected_without_overwriting_the_source() {
    for corrupt in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("canvas.sqlite3");
        let mut store = fixture(&path);
        let mut auth = authority(store.owner());
        store.link_identity(&mut Credentials::default(), &mut auth, root.path().join("backups")).unwrap();
        drop(store);
        let db = rusqlite::Connection::open(&path).unwrap();
        if corrupt {
            db.execute("UPDATE canvas_link_journal SET hash='bad'", []).unwrap();
        } else {
            db.execute_batch("PRAGMA ignore_check_constraints=ON; UPDATE canvas_link_schema SET version=999").unwrap();
        }
        drop(db);
        let original = std::fs::read(&path).unwrap();
        assert!(CanvasStore::open(&path, StoreOptions::default()).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}

#[test]
fn linking_process_child() {
    let Some(path) = std::env::var_os("K6_LINK_CHILD") else { return };
    let path = std::path::PathBuf::from(path);
    let phase = std::env::var("K6_LINK_PHASE").unwrap();
    let mut store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
    let mut auth = authority(store.owner());
    store
        .link_identity_observed(&mut Credentials::default(), &mut auth, path.parent().unwrap().join("backups"), |stage| {
            if (phase == "before" && stage == CanvasStage::BeforeCommit) || (phase == "after" && stage == CanvasStage::AfterCommit) {
                std::fs::write(path.with_extension("boundary"), b"ready").unwrap();
                loop {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                }
            }
        })
        .unwrap();
}

#[test]
fn process_kill_before_and_after_migration_commit_preserves_art_and_exact_binding_boundary() {
    for phase in ["before", "after"] {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("canvas.sqlite3");
        drop(fixture(&path));
        let original = art_rows(&path);
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "linking_process_child", "--nocapture"])
            .env("K6_LINK_CHILD", &path)
            .env("K6_LINK_PHASE", phase)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let start = std::time::Instant::now();
        while !path.with_extension("boundary").exists() && start.elapsed() < std::time::Duration::from_secs(15) {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let reached = path.with_extension("boundary").exists();
        let _ = child.kill();
        child.wait().unwrap();
        assert!(reached, "migration never reached {phase} commit boundary");
        let store = CanvasStore::open(&path, StoreOptions::default()).unwrap();
        assert_eq!(store.local_binding().unwrap().is_some(), phase == "after");
        assert_eq!(art_rows(&path), original);
    }
}
