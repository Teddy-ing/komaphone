use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::time::Duration;

use photocraft_scene::{Bookmark, Camera, CanvasId, CanvasInfo, OwnerId, RegionKey, SceneView, Stroke, StrokeId, ViewStats, Viewport};
use photocraft_scene::{MAX_BOOKMARKS, MAX_CANVASES, MAX_DEPTH, MAX_SPATIAL_CELLS, MAX_STROKES, MAX_SUMMARY_POINTS, MAX_VIEW_POINTS, MAX_VISIBLE_STROKES};
use photocraft_scene::{coordinates, render};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::{Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};

use super::{CANVAS_STORE_SCHEMA, MAX_CANVAS_INFO_BYTES, MAX_CANVAS_QUERY_ROWS, MAX_CANVAS_STROKE_BYTES, MAX_CANVAS_VIEW_BYTES};
use crate::{Error, Result, StoreOptions, Usage};

const APPLICATION_ID: u32 = 0x4b434e56;
const MAX_REVISION: u64 = i64::MAX as u64;

/// Deterministic boundaries for real process-interruption tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CanvasStage {
    BeforeCommit,
    AfterCommit,
}

pub struct CanvasStore {
    pub(super) db: Connection,
    options: StoreOptions,
    owner: OwnerId,
}

fn sql(error: rusqlite::Error) -> Error {
    match &error {
        rusqlite::Error::SqliteFailure(code, _) if code.extended_code & 0xff == 13 => Error::Full,
        _ => Error::Storage(error.to_string()),
    }
}
fn invalid(error: impl std::fmt::Display) -> Error {
    Error::Invalid(error.to_string())
}
fn corrupt(error: impl std::fmt::Display) -> Error {
    Error::Corrupt(error.to_string())
}
fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn index_digest(canvas: CanvasId, id: StrokeId, revision: u64, depth: u8, extent: f64) -> String {
    digest(format!("{}|{}|{revision}|{depth}|{}", canvas.as_str(), id.as_str(), extent.to_bits()).as_bytes())
}
fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
fn random_id() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|e| Error::Storage(format!("Cannot allocate canvas identity: {e}")))?;
    if bytes.iter().all(|b| *b == 0) {
        return Err(Error::Storage("Random source returned an empty canvas identity; retry".into()));
    }
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn fresh_stroke_id() -> Result<StrokeId> {
    StrokeId::new(&random_id()?).map_err(invalid)
}

struct LimitedBytes {
    bytes: Vec<u8>,
    limit: usize,
}
impl std::io::Write for LimitedBytes {
    fn write(&mut self, input: &[u8]) -> std::io::Result<usize> {
        if self.bytes.len().saturating_add(input.len()) > self.limit {
            return Err(std::io::Error::other("canvas record exceeds its byte limit"));
        }
        self.bytes.extend_from_slice(input);
        Ok(input.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn encode(value: &impl Serialize, limit: usize) -> Result<Vec<u8>> {
    let mut writer = LimitedBytes { bytes: Vec::new(), limit };
    serde_json::to_writer(&mut writer, value).map_err(invalid)?;
    Ok(writer.bytes)
}
fn decode<T: DeserializeOwned>(bytes: &[u8], hash: &str, limit: usize) -> Result<T> {
    if bytes.is_empty() || bytes.len() > limit || hash.len() != 64 || digest(bytes) != hash {
        return Err(corrupt("canvas record size or checksum mismatch"));
    }
    serde_json::from_slice(bytes).map_err(corrupt)
}
fn valid_camera(camera: &Camera, canvas: CanvasId) -> Result<()> {
    if camera.canvas != canvas {
        return Err(invalid("camera belongs to a different canvas"));
    }
    coordinates::center(camera).map_err(invalid)?;
    coordinates::visible_regions(&Viewport { camera: camera.clone(), width: 1, height: 1 }, camera.region.depth, 4).map_err(invalid)?;
    Ok(())
}
fn valid_info(info: &CanvasInfo, id: CanvasId, owner: OwnerId) -> Result<()> {
    if info.id != id || info.owner != owner {
        return Err(corrupt("canvas identity or owner mismatch"));
    }
    photocraft_scene::validate_name(&info.name).map_err(invalid)?;
    valid_camera(&info.camera, id)?;
    if info.revision > MAX_REVISION || info.stroke_count > MAX_STROKES || info.undo_count.saturating_add(info.redo_count) > info.stroke_count {
        return Err(corrupt("canvas history counters"));
    }
    valid_bookmarks(&info.bookmarks, id)?;
    for stamp in [&info.created_at, &info.updated_at] {
        if stamp.len() > 64 || chrono::DateTime::parse_from_rfc3339(stamp).is_err() {
            return Err(corrupt("canvas timestamp"));
        }
    }
    Ok(())
}
fn valid_bookmarks(bookmarks: &[Bookmark], id: CanvasId) -> Result<()> {
    if bookmarks.len() > MAX_BOOKMARKS {
        return Err(invalid("canvas bookmark limit reached"));
    }
    for bookmark in bookmarks {
        photocraft_scene::validate_name(&bookmark.name).map_err(invalid)?;
        valid_camera(&bookmark.camera, id)?;
    }
    Ok(())
}
fn read_info(db: &Connection, id: CanvasId, owner: OwnerId) -> Result<CanvasInfo> {
    let row: Option<(Vec<u8>, String)> = db
        .query_row("SELECT substr(info,1,?2),substr(hash,1,65) FROM canvases WHERE id=?1", params![id.as_str(), (MAX_CANVAS_INFO_BYTES + 1) as i64], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(sql)?;
    let (bytes, hash) = row.ok_or(Error::NotFound)?;
    let info: CanvasInfo = decode(&bytes, &hash, MAX_CANVAS_INFO_BYTES)?;
    valid_info(&info, id, owner)?;
    Ok(info)
}
fn write_info(db: &Connection, info: &CanvasInfo) -> Result<()> {
    let bytes = encode(info, MAX_CANVAS_INFO_BYTES)?;
    db.execute("UPDATE canvases SET info=?2,hash=?3 WHERE id=?1", params![info.id.as_str(), bytes, digest(&bytes)]).map_err(sql)?;
    Ok(())
}
fn bump(info: &mut CanvasInfo) -> Result<()> {
    info.revision = info.revision.checked_add(1).filter(|n| *n <= MAX_REVISION).ok_or_else(|| invalid("canvas revision limit"))?;
    info.updated_at = now();
    Ok(())
}
fn expect_revision(info: &CanvasInfo, expected: u64) -> Result<()> {
    if info.revision != expected { Err(Error::Conflict) } else { Ok(()) }
}
fn active_id(db: &Connection) -> Result<Option<CanvasId>> {
    let value: Option<String> = db.query_row("SELECT substr(active,1,33) FROM identity WHERE singleton=1", [], |r| r.get(0)).map_err(sql)?;
    value.map(|value| CanvasId::new(&value).map_err(corrupt)).transpose()
}

fn save_outgoing(db: &Connection, owner: OwnerId, camera: Option<Camera>) -> Result<()> {
    if let Some(camera) = camera {
        if active_id(db)? != Some(camera.canvas) {
            return Err(invalid("outgoing camera is not the active canvas"));
        }
        valid_camera(&camera, camera.canvas)?;
        let mut previous = read_info(db, camera.canvas, owner)?;
        previous.camera = camera;
        bump(&mut previous)?;
        write_info(db, &previous)?;
    }
    Ok(())
}

impl CanvasStore {
    pub fn open(path: impl AsRef<Path>, options: StoreOptions) -> Result<Self> {
        if !(64 * 1024..=64 * 1024 * 1024 * 1024).contains(&options.quota_bytes) {
            return Err(invalid("canvas database quota must be 64 KiB through 64 GiB"));
        }
        let path = path.as_ref();
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent).map_err(|e| Error::Storage(e.to_string()))?;
        }
        let mut db = Connection::open(path).map_err(sql)?;
        db.busy_timeout(Duration::from_secs(2)).map_err(sql)?;
        let version: u32 = db.pragma_query_value(None, "user_version", |r| r.get(0)).map_err(sql)?;
        let application: u32 = db.pragma_query_value(None, "application_id", |r| r.get(0)).map_err(sql)?;
        if version != 0 && version != CANVAS_STORE_SCHEMA {
            return Err(Error::Version(version));
        }
        if application != 0 && application != APPLICATION_ID {
            return Err(corrupt("not a Komaphone canvas database"));
        }
        if version == 0 {
            let count: u32 =
                db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |r| r.get(0)).map_err(sql)?;
            if count != 0 {
                return Err(corrupt("unversioned nonempty canvas database; original retained"));
            }
        }
        db.pragma_update(None, "journal_mode", "DELETE").map_err(sql)?;
        db.pragma_update(None, "synchronous", "EXTRA").map_err(sql)?;
        db.pragma_update(None, "foreign_keys", "ON").map_err(sql)?;
        db.pragma_update(None, "trusted_schema", "OFF").map_err(sql)?;
        db.pragma_update(None, "cache_size", -256).map_err(sql)?;
        let journal: String = db.pragma_query_value(None, "journal_mode", |r| r.get(0)).map_err(sql)?;
        let synchronous: u32 = db.pragma_query_value(None, "synchronous", |r| r.get(0)).map_err(sql)?;
        let foreign: u32 = db.pragma_query_value(None, "foreign_keys", |r| r.get(0)).map_err(sql)?;
        if journal.to_lowercase() != "delete" || synchronous != 3 || foreign != 1 {
            return Err(Error::Storage("Canvas durability settings could not be enabled".into()));
        }
        let page_size: u32 = db.pragma_query_value(None, "page_size", |r| r.get(0)).map_err(sql)?;
        if page_size == 0 {
            return Err(corrupt("zero canvas database page size"));
        }
        let pages = options.quota_bytes / u64::from(page_size);
        db.pragma_update(None, "max_page_count", pages as i64).map_err(sql)?;
        let actual: u32 = db.pragma_query_value(None, "max_page_count", |r| r.get(0)).map_err(sql)?;
        if u64::from(actual) > pages {
            return Err(Error::Full);
        }
        if version == 0 {
            let owner = OwnerId::new(&random_id()?).map_err(invalid)?;
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
            tx.execute_batch("CREATE TABLE canvases(id TEXT PRIMARY KEY,info BLOB NOT NULL,hash TEXT NOT NULL);
                CREATE TABLE identity(singleton INTEGER PRIMARY KEY CHECK(singleton=1),owner TEXT NOT NULL,active TEXT REFERENCES canvases(id));
                CREATE TABLE strokes(canvas TEXT NOT NULL REFERENCES canvases(id),id TEXT NOT NULL,revision INTEGER NOT NULL,input_hash TEXT NOT NULL,payload BLOB NOT NULL,hash TEXT NOT NULL,summary BLOB NOT NULL,summary_hash TEXT NOT NULL,state INTEGER NOT NULL CHECK(state IN(0,1,2)),extent REAL NOT NULL,index_hash TEXT NOT NULL,PRIMARY KEY(canvas,id));
                CREATE INDEX stroke_history ON strokes(canvas,state,revision);
                CREATE TABLE cells(canvas TEXT NOT NULL,depth INTEGER NOT NULL,x TEXT NOT NULL,y TEXT NOT NULL,source_depth INTEGER NOT NULL,stroke TEXT NOT NULL,state INTEGER NOT NULL CHECK(state IN(0,1,2)),PRIMARY KEY(canvas,depth,x,y,source_depth,stroke),FOREIGN KEY(canvas,stroke) REFERENCES strokes(canvas,id));
                CREATE INDEX cells_view ON cells(canvas,depth,x,y,state,source_depth,stroke);
                CREATE INDEX cells_stroke ON cells(canvas,stroke);") .map_err(sql)?;
            tx.execute("INSERT INTO identity(singleton,owner,active) VALUES(1,?1,NULL)", [owner.as_str()]).map_err(sql)?;
            tx.pragma_update(None, "application_id", APPLICATION_ID).map_err(sql)?;
            tx.pragma_update(None, "user_version", CANVAS_STORE_SCHEMA).map_err(sql)?;
            tx.commit().map_err(sql)?;
        }
        let owner: String = db.query_row("SELECT substr(owner,1,33) FROM identity WHERE singleton=1", [], |r| r.get(0)).map_err(sql)?;
        let owner = OwnerId::new(&owner).map_err(corrupt)?;
        super::linking::verify_existing(&db, owner)?;
        Ok(Self { db, options, owner })
    }
    pub fn owner(&self) -> OwnerId {
        self.owner
    }
    pub fn usage(&self) -> Result<Usage> {
        let pages: u32 = self.db.pragma_query_value(None, "page_count", |r| r.get(0)).map_err(sql)?;
        let size: u32 = self.db.pragma_query_value(None, "page_size", |r| r.get(0)).map_err(sql)?;
        Ok(Usage {
            stored_bytes: u64::from(pages).checked_mul(u64::from(size)).ok_or_else(|| corrupt("canvas database size"))?,
            quota_bytes: self.options.quota_bytes,
        })
    }
    pub fn info(&self, id: CanvasId) -> Result<CanvasInfo> {
        read_info(&self.db, id, self.owner)
    }
    pub fn active(&self) -> Result<Option<CanvasInfo>> {
        active_id(&self.db)?.map(|id| self.info(id)).transpose()
    }
    pub fn list(&self) -> Result<Vec<CanvasInfo>> {
        let mut statement = self.db.prepare("SELECT substr(id,1,33) FROM canvases ORDER BY id LIMIT ?1").map_err(sql)?;
        let ids = statement.query_map([(MAX_CANVASES + 1) as i64], |r| r.get::<_, String>(0)).map_err(sql)?;
        let mut result = Vec::new();
        for id in ids {
            if result.len() >= MAX_CANVASES {
                return Err(corrupt("canvas count exceeds limit"));
            }
            result.push(self.info(CanvasId::new(&id.map_err(sql)?).map_err(corrupt)?)?);
        }
        Ok(result)
    }
    pub fn create(&mut self, name: &str) -> Result<CanvasInfo> {
        self.create_with_previous(name, None)
    }
    pub fn create_with_previous(&mut self, name: &str, previous_camera: Option<Camera>) -> Result<CanvasInfo> {
        self.create_observed(name, previous_camera, |_| {})
    }
    pub fn create_observed(&mut self, name: &str, previous_camera: Option<Camera>, mut observe: impl FnMut(CanvasStage)) -> Result<CanvasInfo> {
        photocraft_scene::validate_name(name).map_err(invalid)?;
        let id = CanvasId::new(&random_id()?).map_err(invalid)?;
        let camera = coordinates::target(id, coordinates::point(&RegionKey::default(), [512.0, 512.0]).map_err(invalid)?, 0, 0.75).map_err(invalid)?;
        let stamp = now();
        let info = CanvasInfo {
            id,
            owner: self.owner,
            name: name.into(),
            created_at: stamp.clone(),
            updated_at: stamp,
            revision: 0,
            camera,
            bookmarks: Vec::new(),
            stroke_count: 0,
            undo_count: 0,
            redo_count: 0,
        };
        let bytes = encode(&info, MAX_CANVAS_INFO_BYTES)?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let count: u32 = tx.query_row("SELECT count(*) FROM canvases", [], |r| r.get(0)).map_err(sql)?;
        if count as usize >= MAX_CANVASES {
            return Err(invalid("local canvas limit reached; existing canvases retained"));
        }
        save_outgoing(&tx, self.owner, previous_camera)?;
        tx.execute("INSERT INTO canvases(id,info,hash) VALUES(?1,?2,?3)", params![id.as_str(), bytes, digest(&bytes)]).map_err(sql)?;
        tx.execute("UPDATE identity SET active=?1 WHERE singleton=1", [id.as_str()]).map_err(sql)?;
        observe(CanvasStage::BeforeCommit);
        tx.commit().map_err(sql)?;
        observe(CanvasStage::AfterCommit);
        Ok(info)
    }
    pub fn rename(&mut self, id: CanvasId, expected: u64, name: &str) -> Result<CanvasInfo> {
        photocraft_scene::validate_name(name).map_err(invalid)?;
        self.change_info(id, expected, |info| {
            info.name = name.into();
            Ok(())
        })
    }
    fn change_info(&mut self, id: CanvasId, expected: u64, change: impl FnOnce(&mut CanvasInfo) -> Result<()>) -> Result<CanvasInfo> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let mut info = read_info(&tx, id, self.owner)?;
        expect_revision(&info, expected)?;
        change(&mut info)?;
        bump(&mut info)?;
        valid_info(&info, id, self.owner)?;
        write_info(&tx, &info)?;
        tx.commit().map_err(sql)?;
        Ok(info)
    }
    pub fn set_bookmarks(&mut self, id: CanvasId, expected: u64, bookmarks: Vec<Bookmark>) -> Result<CanvasInfo> {
        valid_bookmarks(&bookmarks, id)?;
        self.change_info(id, expected, |info| {
            info.bookmarks = bookmarks;
            Ok(())
        })
    }
    pub fn save_camera(&mut self, camera: Camera) -> Result<CanvasInfo> {
        valid_camera(&camera, camera.canvas)?;
        let info = self.info(camera.canvas)?;
        self.change_info(camera.canvas, info.revision, |info| {
            info.camera = camera;
            Ok(())
        })
    }
    pub fn switch(&mut self, id: CanvasId, previous_camera: Option<Camera>) -> Result<CanvasInfo> {
        self.switch_observed(id, previous_camera, |_| {})
    }
    pub fn switch_observed(&mut self, id: CanvasId, previous_camera: Option<Camera>, mut observe: impl FnMut(CanvasStage)) -> Result<CanvasInfo> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        read_info(&tx, id, self.owner)?;
        save_outgoing(&tx, self.owner, previous_camera)?;
        tx.execute("UPDATE identity SET active=?1 WHERE singleton=1", [id.as_str()]).map_err(sql)?;
        let info = read_info(&tx, id, self.owner)?;
        observe(CanvasStage::BeforeCommit);
        tx.commit().map_err(sql)?;
        observe(CanvasStage::AfterCommit);
        Ok(info)
    }
    pub fn append(&mut self, stroke: &Stroke, expected: u64) -> Result<CanvasInfo> {
        self.append_observed(stroke, expected, |_| {})
    }
    pub fn append_observed(&mut self, stroke: &Stroke, expected: u64, mut observe: impl FnMut(CanvasStage)) -> Result<CanvasInfo> {
        stroke.validate().map_err(invalid)?;
        if stroke.owner != self.owner || stroke.revision != 0 {
            return Err(invalid("stroke owner or uncommitted revision"));
        }
        let input_hash = digest(&encode(stroke, MAX_CANVAS_STROKE_BYTES)?);
        let cells = stroke_cells(stroke)?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let mut info = read_info(&tx, stroke.canvas, self.owner)?;
        let prior: Option<(String, Vec<u8>, String)> = tx
            .query_row(
                "SELECT substr(input_hash,1,65),substr(payload,1,?3),substr(hash,1,65) FROM strokes WHERE canvas=?1 AND id=?2",
                params![stroke.canvas.as_str(), stroke.id.as_str(), (MAX_CANVAS_STROKE_BYTES + 1) as i64],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(sql)?;
        if let Some((prior, bytes, hash)) = prior {
            let saved: Stroke = decode(&bytes, &hash, MAX_CANVAS_STROKE_BYTES)?;
            saved.validate().map_err(corrupt)?;
            if saved.canvas != stroke.canvas || saved.owner != self.owner || saved.id != stroke.id || saved.revision == 0 || saved.revision > info.revision {
                return Err(corrupt("stored retry stroke identity"));
            }
            // Compare the originally committed request bytes, not parsed float
            // equality: round trips must not turn a network/worker retry into a
            // new logical operation or depend on JSON decoder rounding details.
            return if prior == input_hash { Ok(info) } else { Err(Error::Conflict) };
        }
        expect_revision(&info, expected)?;
        if info.stroke_count >= MAX_STROKES {
            return Err(invalid("canvas stroke limit reached; saved work retained"));
        }
        bump(&mut info)?;
        let mut saved = stroke.clone();
        saved.revision = info.revision;
        let bytes = encode(&saved, MAX_CANVAS_STROKE_BYTES)?;
        let summary = encode(&render::decimate(&saved, MAX_SUMMARY_POINTS).map_err(invalid)?, MAX_CANVAS_STROKE_BYTES)?;
        tx.execute("UPDATE cells SET state=0 WHERE canvas=?1 AND stroke IN(SELECT id FROM strokes WHERE canvas=?1 AND state=2)", [stroke.canvas.as_str()])
            .map_err(sql)?;
        tx.execute("UPDATE strokes SET state=0 WHERE canvas=?1 AND state=2", [stroke.canvas.as_str()]).map_err(sql)?;
        tx.execute(
            "INSERT INTO strokes(canvas,id,revision,input_hash,payload,hash,summary,summary_hash,state,extent,index_hash) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,1,?9,?10)",
            params![
                stroke.canvas.as_str(),
                stroke.id.as_str(),
                saved.revision as i64,
                input_hash,
                bytes,
                digest(&bytes),
                summary,
                digest(&summary),
                stroke_extent(stroke),
                index_digest(stroke.canvas, stroke.id, saved.revision, stroke.anchor.depth, stroke_extent(stroke))
            ],
        )
        .map_err(sql)?;
        for cell in cells {
            tx.execute(
                "INSERT INTO cells(canvas,depth,x,y,source_depth,stroke,state) VALUES(?1,?2,?3,?4,?5,?6,1)",
                params![stroke.canvas.as_str(), cell.depth, cell.x.0.to_string(), cell.y.0.to_string(), stroke.anchor.depth, stroke.id.as_str()],
            )
            .map_err(sql)?;
        }
        info.stroke_count += 1;
        info.undo_count += 1;
        info.redo_count = 0;
        write_info(&tx, &info)?;
        observe(CanvasStage::BeforeCommit);
        tx.commit().map_err(sql)?;
        observe(CanvasStage::AfterCommit);
        Ok(info)
    }
    pub fn undo(&mut self, id: CanvasId, expected: u64) -> Result<CanvasInfo> {
        self.history(id, expected, false)
    }
    pub fn redo(&mut self, id: CanvasId, expected: u64) -> Result<CanvasInfo> {
        self.history(id, expected, true)
    }
    fn history(&mut self, id: CanvasId, expected: u64, redo: bool) -> Result<CanvasInfo> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
        let mut info = read_info(&tx, id, self.owner)?;
        expect_revision(&info, expected)?;
        let query = if redo {
            "SELECT substr(id,1,33) FROM strokes WHERE canvas=?1 AND state=2 ORDER BY revision ASC LIMIT 1"
        } else {
            "SELECT substr(id,1,33) FROM strokes WHERE canvas=?1 AND state=1 ORDER BY revision DESC LIMIT 1"
        };
        let stroke: Option<String> = tx.query_row(query, [id.as_str()], |r| r.get(0)).optional().map_err(sql)?;
        let stroke = stroke.ok_or_else(|| invalid(if redo { "nothing to redo" } else { "nothing to undo" }))?;
        StrokeId::new(&stroke).map_err(corrupt)?;
        tx.execute("UPDATE cells SET state=?3 WHERE canvas=?1 AND stroke=?2", params![id.as_str(), stroke, if redo { 1 } else { 2 }]).map_err(sql)?;
        tx.execute("UPDATE strokes SET state=?3 WHERE canvas=?1 AND id=?2", params![id.as_str(), stroke, if redo { 1 } else { 2 }]).map_err(sql)?;
        if redo {
            info.redo_count = info.redo_count.checked_sub(1).ok_or_else(|| corrupt("redo count"))?;
            info.undo_count += 1;
        } else {
            info.undo_count = info.undo_count.checked_sub(1).ok_or_else(|| corrupt("undo count"))?;
            info.redo_count += 1;
        }
        bump(&mut info)?;
        write_info(&tx, &info)?;
        tx.commit().map_err(sql)?;
        Ok(info)
    }
    pub fn view(&self, viewport: &Viewport) -> Result<SceneView> {
        let transaction = self.db.unchecked_transaction().map_err(sql)?;
        let view = load_view(&transaction, self.owner, viewport)?;
        transaction.commit().map_err(sql)?;
        Ok(view)
    }
}

fn stroke_extent(stroke: &Stroke) -> f64 {
    let mut min = [f64::INFINITY; 2];
    let mut max = [f64::NEG_INFINITY; 2];
    for point in &stroke.points {
        min[0] = min[0].min(point[0]);
        min[1] = min[1].min(point[1]);
        max[0] = max[0].max(point[0]);
        max[1] = max[1].max(point[1]);
    }
    (max[0] - min[0]).max(max[1] - min[1]) + stroke.width
}
fn stroke_cells(stroke: &Stroke) -> Result<BTreeSet<RegionKey>> {
    let mut all = BTreeSet::new();
    for cell in render::spatial_cells(stroke, MAX_SPATIAL_CELLS).map_err(invalid)? {
        all.insert(cell);
        all.extend(coordinates::ancestors(&cell).map_err(invalid)?);
        if all.len() > MAX_SPATIAL_CELLS * (usize::from(MAX_DEPTH) + 1) {
            return Err(invalid("stroke spatial index limit"));
        }
    }
    Ok(all)
}

fn load_view(db: &Connection, owner: OwnerId, viewport: &Viewport) -> Result<SceneView> {
    let info = read_info(db, viewport.camera.canvas, owner)?;
    valid_camera(&viewport.camera, info.id)?;
    let mut stats = ViewStats::default();
    let mut candidates = BTreeMap::new();
    // The normalized camera may sit near the upper end of its local zoom range.
    // Use finer index cells when they fit, rather than fetching every tiny child
    // in a mostly offscreen parent tile. Coarse authored marks remain separate.
    coordinates::visible_regions(viewport, viewport.camera.region.depth, MAX_SPATIAL_CELLS).map_err(invalid)?;
    let mut query_depth = viewport.camera.region.depth;
    while query_depth < MAX_DEPTH && coordinates::visible_regions(viewport, query_depth + 1, MAX_SPATIAL_CELLS).is_ok() {
        query_depth += 1;
    }
    for depth in 0..=query_depth {
        let cells = coordinates::visible_regions(viewport, depth, MAX_SPATIAL_CELLS).map_err(invalid)?;
        stats.queried_cells = stats.queried_cells.saturating_add(cells.len());
        for cell in cells {
            let lower = depth;
            let upper = if depth == query_depth { MAX_DEPTH } else { depth };
            let mut statement=db.prepare("SELECT substr(s.id,1,33),s.revision,c.source_depth,s.extent,substr(s.index_hash,1,65) FROM cells c JOIN strokes s ON s.canvas=c.canvas AND s.id=c.stroke WHERE c.canvas=?1 AND c.depth=?2 AND c.x=?3 AND c.y=?4 AND c.state=1 AND c.source_depth BETWEEN ?5 AND ?6 AND s.state=1 LIMIT ?7").map_err(sql)?;
            let rows = statement
                .query_map(
                    params![info.id.as_str(), depth, cell.x.0.to_string(), cell.y.0.to_string(), lower, upper, (MAX_CANVAS_QUERY_ROWS + 1) as i64],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, u8>(2)?, r.get::<_, f64>(3)?, r.get::<_, String>(4)?)),
                )
                .map_err(sql)?;
            for row in rows {
                stats.candidate_rows = stats.candidate_rows.saturating_add(1);
                if stats.candidate_rows > MAX_CANVAS_QUERY_ROWS {
                    return Err(invalid("canvas view index budget reached; zoom into a smaller area"));
                }
                let (id, revision, source_depth, extent, index_hash) = row.map_err(sql)?;
                let id = StrokeId::new(&id).map_err(corrupt)?;
                if revision <= 0 || !extent.is_finite() || extent <= 0.0 || extent > 2.0 * photocraft_scene::MAX_STROKE_LOCAL + 2048.0 {
                    return Err(corrupt("stroke index metadata"));
                }
                if index_digest(info.id, id, revision as u64, source_depth, extent) != index_hash {
                    return Err(corrupt("stroke index metadata checksum"));
                }
                candidates.insert((revision, id), (source_depth, extent));
                if candidates.len() > MAX_VISIBLE_STROKES {
                    return Err(invalid("canvas view stroke budget reached; zoom into a smaller area"));
                }
            }
        }
    }
    let mut strokes = Vec::with_capacity(candidates.len());
    for ((revision, id), (depth, extent)) in candidates {
        let scale = render::scale_ratio(depth, &viewport.camera).map_err(invalid)?;
        let summary = extent * scale <= 2.0;
        let query = if summary {
            "SELECT substr(summary,1,?3),substr(summary_hash,1,65) FROM strokes WHERE canvas=?1 AND id=?2"
        } else {
            "SELECT substr(payload,1,?3),substr(hash,1,65) FROM strokes WHERE canvas=?1 AND id=?2"
        };
        let (bytes, hash): (Vec<u8>, String) =
            db.query_row(query, params![info.id.as_str(), id.as_str(), (MAX_CANVAS_STROKE_BYTES + 1) as i64], |r| Ok((r.get(0)?, r.get(1)?))).map_err(sql)?;
        stats.loaded_bytes = stats.loaded_bytes.saturating_add(bytes.len());
        if stats.loaded_bytes > MAX_CANVAS_VIEW_BYTES {
            return Err(invalid("canvas view byte budget reached; zoom into a smaller area"));
        }
        let stroke: Stroke = decode(&bytes, &hash, MAX_CANVAS_STROKE_BYTES)?;
        stroke.validate().map_err(corrupt)?;
        if stroke.canvas != info.id || stroke.owner != owner || stroke.id != id || stroke.revision != revision as u64 || stroke.anchor.depth != depth {
            return Err(corrupt("canvas stroke identity/index mismatch"));
        }
        stats.points = stats.points.saturating_add(stroke.points.len());
        if stats.points > MAX_VIEW_POINTS {
            return Err(invalid("canvas view point budget reached; zoom into a smaller area"));
        }
        if summary {
            if stroke.points.len() > MAX_SUMMARY_POINTS {
                return Err(corrupt("oversized canvas summary"));
            }
            stats.summary_strokes += 1;
        } else {
            stats.full_strokes += 1;
        }
        strokes.push(stroke);
    }
    Ok(SceneView { viewport: viewport.clone(), revision: info.revision, strokes, stats })
}
