//! Lossless local drawing boundaries, independent of the portable Actions journal.
//!
//! The recorder never acknowledges storage. It retains immutable COW documents until the
//! native project worker takes them; that worker acknowledges only its SQLite commit.
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;

use photocraft_doc::{DocId, Document};
use photocraft_local_store::{LOCAL_SCHEMA_VERSION, ReplayKind, ReplayRecord};
use serde_json::{Value, json};

use crate::{DocState, EngineError, Result, Session};

pub const MAX_PENDING_EDITS: usize = 64;
pub const MAX_PENDING_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = 8 * 1024 * 1024;
const MAX_RECORD_DEPTH: usize = 48;

/// One completed edit. Its result is authoritative even for unsupported command playback.
#[derive(Clone, Debug)]
pub struct CapturedEdit {
    pub document: Arc<Document>,
    pub editor_revision: u64,
    pub record: ReplayRecord,
    retained_bytes: usize,
}

#[derive(Clone)]
struct Seen {
    document: Arc<Document>,
    elapsed_ms: u64,
    clock: web_time::Instant,
}

/// A command scope, preserved before Actions rewrite IDs, presets or keystrokes.
pub struct CaptureContext {
    previous: Option<ReplayRecord>,
    before: HashMap<DocId, Seen>,
    first_sequence: u64,
}

#[derive(Default)]
pub struct Recorder {
    enabled: bool,
    pending: VecDeque<(u64, CapturedEdit)>,
    seen: HashMap<DocId, Seen>,
    excluded: HashMap<DocId, String>,
    current: Option<ReplayRecord>,
    next_sequence: u64,
    pending_bytes: usize,
    error: Option<String>,
}

impl Recorder {
    pub fn enable(&mut self) {
        self.enabled = true;
    }

    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Native worker polling must not publish tentative compound-command edits.
    pub fn in_command(&self) -> bool {
        self.current.is_some()
    }

    pub fn pending_len(&self) -> usize {
        self.pending.len()
    }

    pub fn pending_for(&self, doc: DocId) -> usize {
        self.pending.iter().filter(|(_, edit)| edit.document.id == doc).count()
    }

    pub fn error(&self) -> Option<String> {
        self.error.clone()
    }

    /// Large legacy documents remain ordinary editor files with their existing Save flow.
    pub fn excluded_reason(&self, document: DocId) -> Option<&str> {
        self.excluded.get(&document).map(String::as_str)
    }

    pub fn peek(&self) -> Option<&CapturedEdit> {
        self.pending.front().map(|(_, edit)| edit)
    }

    /// Let an independent project save while another retains a failed request. Skipping
    /// blocked IDs never changes the event order within any individual project.
    pub fn peek_unblocked(&self, blocked: &HashSet<DocId>) -> Option<&CapturedEdit> {
        self.pending.iter().find(|(_, edit)| !blocked.contains(&edit.document.id)).map(|(_, edit)| edit)
    }

    /// Transfer ownership to a bounded native worker. The caller must retain it on failure.
    pub fn pop(&mut self) -> Option<CapturedEdit> {
        self.pop_index(0)
    }

    pub fn pop_unblocked(&mut self, blocked: &HashSet<DocId>) -> Option<CapturedEdit> {
        let index = self.pending.iter().position(|(_, edit)| !blocked.contains(&edit.document.id))?;
        self.pop_index(index)
    }

    fn pop_index(&mut self, index: usize) -> Option<CapturedEdit> {
        let (_, edit) = self.pending.remove(index)?;
        self.pending_bytes = self.pending_bytes.saturating_sub(edit.retained_bytes);
        if self.pending.len() < MAX_PENDING_EDITS && self.pending_bytes < MAX_PENDING_BYTES {
            self.error = None;
        }
        Some(edit)
    }

    /// Reopening a durable project establishes its existing endpoint, not invented history.
    pub fn adopt(&mut self, state: &DocState, elapsed_ms: u64) {
        self.excluded.remove(&state.doc.id);
        self.seen.insert(state.doc.id, Seen { document: state.doc.clone(), elapsed_ms, clock: web_time::Instant::now() });
    }

    /// Fork a recovered/imported current document without dropping queued original edits.
    pub fn forget(&mut self, document: DocId) {
        self.seen.remove(&document);
        self.excluded.remove(&document);
    }

    pub fn is_current(&self, state: &DocState) -> bool {
        self.seen.get(&state.doc.id).is_some_and(|seen| Arc::ptr_eq(&seen.document, &state.doc))
    }

    pub fn is_managed(&self, document: DocId) -> bool {
        self.enabled && self.seen.contains_key(&document)
    }

    fn capacity(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.pending.len() >= MAX_PENDING_EDITS || self.pending_bytes >= MAX_PENDING_BYTES {
            return Err(EngineError::Other(
                "Local save queue is full. Wait for saving or retry the failed save before drawing more; existing edits are retained.".into(),
            ));
        }
        Ok(())
    }

    fn capture(&mut self, document: Arc<Document>, editor_revision: u64, fallback: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }
        if self.excluded.contains_key(&document.id) {
            return Ok(());
        }
        let seen = self.seen.get(&document.id);
        if seen.is_some_and(|seen| Arc::ptr_eq(&seen.document, &document)) {
            return Ok(());
        }
        let baseline = seen.is_none();
        if baseline && let Some(reason) = local_document_limit(&document) {
            self.excluded.insert(document.id, reason);
            return Ok(());
        }
        let elapsed_ms = seen.map_or(0, |s| s.elapsed_ms.saturating_add(u64::try_from(s.clock.elapsed().as_millis()).unwrap_or(u64::MAX)));
        let mut record = self.current.clone().unwrap_or_else(|| checkpoint(fallback));
        record.elapsed_ms = elapsed_ms;
        if baseline {
            record.kind = ReplayKind::Baseline;
            record.command = if record.command == "file.new" { "file.new" } else { "legacy.import" }.into();
            record.params = json!({"baseline": "current document; no earlier drawing history"});
        } else if matches!(fallback, "edit.undo" | "edit.redo") {
            record.command = fallback.into();
            record.kind = if fallback == "edit.undo" { ReplayKind::Undo } else { ReplayKind::Redo };
        }
        // Count unique tiles in this snapshot, not pixels across the full canvas. Summing
        // snapshots deliberately overestimates sharing; it is a conservative queue budget.
        let retained_bytes = snapshot_retained_bytes(&document).saturating_add(value_bytes(&record.params)?);
        let edit = CapturedEdit { document: document.clone(), editor_revision, record, retained_bytes };
        self.next_sequence = self.next_sequence.saturating_add(1);
        self.pending.push_back((self.next_sequence, edit));
        self.pending_bytes = self.pending_bytes.saturating_add(retained_bytes);
        self.seen.insert(document.id, Seen { document, elapsed_ms, clock: web_time::Instant::now() });
        // A mutation that already completed must never be thrown away. A single unexpectedly
        // large result stays queued; the next mutation is refused until it has been saved.
        if let Err(error) = self.capacity() {
            self.error = Some(error.to_string());
        }
        Ok(())
    }
}

impl Session {
    pub fn check_new_local_replay_capacity(&self) -> Result<()> {
        if self.local_replay.enabled && self.local_projects.failure_budget_exhausted() {
            return Err(EngineError::Other(
                "Retained failed drafts reached the memory limit. Save or retry those drafts before opening another drawing.".into(),
            ));
        }
        self.local_replay.capacity()
    }

    pub fn check_local_replay_capacity(&self) -> Result<()> {
        if self.active().is_some_and(|state| self.local_replay.excluded.contains_key(&state.doc.id)) {
            return Ok(());
        }
        if self.local_replay.enabled {
            if self.local_projects.failure_budget_exhausted() {
                return Err(EngineError::Other(
                    "Retained failed drafts reached the memory limit. Save or retry those drafts before creating more edits.".into(),
                ));
            }
            if let Some(error) = self.active().and_then(|state| self.local_projects.status(state.doc.id).error) {
                return Err(EngineError::Other(format!("This draft's last save failed: {error} Retry its retained save before editing further.")));
            }
        }
        self.local_replay.capacity()
    }

    /// Validate the candidate before committing it to an already managed local drawing.
    /// Standalone engine sessions and large external editor documents retain upstream limits.
    pub fn validate_local_replay_document(&self, document: &Document) -> Result<()> {
        if self.local_replay.is_managed(document.id) {
            validate_local_document(document)?;
        }
        Ok(())
    }

    /// Called before running command code, with admitted (channel-target-resolved) params.
    pub fn begin_local_replay(&mut self, id: &str, params: &Value) -> Result<Option<CaptureContext>> {
        if !self.local_replay.enabled || id.starts_with("komaphone.project.") {
            return Ok(None);
        }
        if !matches!(id, "file.new" | "file.open") && self.active().is_some_and(|state| self.local_replay.excluded.contains_key(&state.doc.id)) {
            return Ok(None);
        }
        self.capture_local_fallback()?;
        let record = if crate::commands::find(id).is_some_and(|spec| !spec.journal) {
            // Queries and view controls remain usable at the save limit. A rare direct
            // mutation still receives a result checkpoint, without recording query params.
            checkpoint(id)
        } else {
            resolve_record(self, id, params)?
        };
        // Opening a document does not use `edit_with`, so reserve its baseline here.
        if matches!(id, "file.new" | "file.open" | "edit.undo" | "edit.redo") {
            if matches!(id, "file.new" | "file.open") {
                self.check_new_local_replay_capacity()?;
            } else {
                self.check_local_replay_capacity()?;
            }
        }
        let before = self.local_replay.seen.clone();
        let previous = self.local_replay.current.replace(record);
        Ok(Some(CaptureContext { previous, before, first_sequence: self.local_replay.next_sequence }))
    }

    /// Finish before the portable Actions journal rewrites or folds the command.
    pub fn end_local_replay(&mut self, context: Option<CaptureContext>, succeeded: bool) -> Result<()> {
        let Some(context) = context else { return Ok(()) };
        // An entirely rolled-back failed command has no drawing history. Partial failures
        // remain recoverable transitions, explicitly represented by their checkpoints.
        // External documents have no recorder endpoint and cannot participate in this test.
        let mut managed_documents = self.documents().iter().filter(|state| !self.local_replay.excluded.contains_key(&state.doc.id));
        let rolled_back = !succeeded
            && context.before.len() == managed_documents.clone().count()
            && managed_documents.all(|st| context.before.get(&st.doc.id).is_some_and(|seen| Arc::ptr_eq(&seen.document, &st.doc)));
        if rolled_back {
            self.local_replay.pending.retain(|(seq, _)| *seq <= context.first_sequence);
            self.local_replay.pending_bytes = self.local_replay.pending.iter().map(|(_, edit)| edit.retained_bytes).fold(0usize, usize::saturating_add);
            self.local_replay.seen = context.before;
            if self.local_replay.capacity().is_ok() {
                self.local_replay.error = None;
            }
        }
        let result = if rolled_back { Ok(()) } else { self.capture_local_fallback() };
        self.local_replay.current = context.previous;
        result
    }

    /// Called by the shared edit boundary and history transitions, after a completed mutation.
    pub fn capture_local_edit(&mut self, label: &str) -> Result<()> {
        let Some(state) = self.active() else { return Ok(()) };
        let document = state.doc.clone();
        let revision = state.revision;
        self.local_replay.capture(document, revision, label)
    }

    /// Captures direct document mutations (including import/profile/background-job paths).
    /// It examines document identity, never scans unchanged canvas pixels every frame.
    pub fn capture_local_fallback(&mut self) -> Result<()> {
        if !self.local_replay.enabled {
            return Ok(());
        }
        let changed: Vec<_> = self
            .documents()
            .iter()
            .filter(|st| {
                !self.local_replay.excluded.contains_key(&st.doc.id)
                    && self.local_replay.seen.get(&st.doc.id).is_none_or(|seen| !Arc::ptr_eq(&seen.document, &st.doc))
            })
            .map(|st| (st.doc.clone(), st.revision))
            .collect();
        for (document, revision) in changed {
            self.local_replay.capture(document, revision, "checkpoint.externalEdit")?;
        }
        let open: Vec<_> = self.documents().iter().map(|st| st.doc.id).collect();
        self.local_replay.seen.retain(|id, _| open.contains(id));
        self.local_replay.excluded.retain(|id, _| open.contains(id));
        Ok(())
    }

    pub fn capture_local_replay_baseline(&mut self) -> Result<()> {
        self.capture_local_edit("legacy.import")
    }
}

pub fn validate_local_document(document: &Document) -> Result<()> {
    if let Some(reason) = local_document_limit(document) {
        return Err(EngineError::Other(format!("This edit exceeds the local draft limits: {reason} The drawing is unchanged.")));
    }
    Ok(())
}

fn local_document_limit(document: &Document) -> Option<String> {
    let extent = photocraft_party::contracts::ImageExtent { width: document.size.width, height: document.size.height };
    if extent.decoded_bytes(1).is_err() {
        return Some("Local drafts support at most 8192 pixels per side and 16 megapixels. Use File Save for this external document.".into());
    }
    if document.layer_count() > 4096 || document.max_group_depth() > photocraft_format::MAX_GROUP_DEPTH {
        return Some("The document exceeds the local draft layer/depth limit. Use File Save for this external document.".into());
    }
    if snapshot_retained_bytes(document) > MAX_PENDING_BYTES {
        return Some("The document exceeds the 512 MiB local draft memory budget. Use File Save for this external document.".into());
    }
    None
}

/// Conservative retained snapshot accounting. History counts shared tiles and embedded smart
/// sources; local persistence additionally retains original metadata, profiles and layer blobs.
/// Repeated references deliberately overcount rather than understate the bounded save queue.
pub fn snapshot_retained_bytes(document: &Document) -> usize {
    use photocraft_doc::LayerContent;
    let blob = |bytes: &Option<Arc<Vec<u8>>>| bytes.as_ref().map_or(0, |b| b.capacity());
    let text = |bytes: &Option<Arc<str>>| bytes.as_ref().map_or(0, |b| b.len());
    let mut total = photocraft_ops::History::default().pixel_bytes(document).saturating_add(std::mem::size_of::<Document>());
    let mut add = |bytes| total = total.saturating_add(bytes);
    add(document.name.capacity());
    document.for_each_surface(|surface| {
        add(std::mem::size_of::<photocraft_raster::Surface>());
        add(surface.default_bytes().len());
        // Tile bytes are counted above; also retain each surface's map entries and Arc headers.
        add(surface.tiles().count().saturating_mul(128));
    });
    add(blob(&document.icc_profile));
    add(text(&document.metadata.xmp));
    add(blob(&document.metadata.exif));
    add(auxiliary_bytes(&document.variables));
    add(text_styles_bytes(&document.text_styles));
    add(auxiliary_bytes(&document.measurement));
    add(auxiliary_bytes(&document.slices));
    add(auxiliary_bytes(&document.timeline));
    add(auxiliary_bytes(&document.guides));
    add(auxiliary_bytes(&document.clipping_path));
    add(auxiliary_bytes(&document.color_table));
    if let Some(duotone) = &document.duotone {
        add(auxiliary_bytes(&duotone.inks));
        add(duotone.psd_raw.as_ref().map_or(0, |bytes| bytes.len()));
    }
    for comp in document.layer_comps.iter().chain(document.last_document_state.iter()) {
        add(comp_bytes(comp));
    }
    for channel in document.channels.iter().chain(document.quick_mask.iter()) {
        add(std::mem::size_of::<photocraft_doc::AlphaChannel>().saturating_add(channel.name.capacity()));
    }
    for pattern in &document.patterns {
        add(std::mem::size_of::<photocraft_doc::Pattern>().saturating_add(pattern.id.capacity()).saturating_add(pattern.name.capacity()));
    }
    for (key, value) in &document.metadata.text {
        add(key.capacity().saturating_add(value.capacity()).saturating_add(64));
    }
    for (_, name, bytes) in &document.metadata.psd_resources {
        add(name.capacity().saturating_add(bytes.capacity()).saturating_add(64));
    }
    for (_, _, bytes) in &document.metadata.psd_global_blocks {
        add(bytes.capacity().saturating_add(64));
    }
    for path in &document.paths {
        add(path.name.capacity().saturating_add(blob(&path.psd_raw)).saturating_add(path_bytes(&path.path)).saturating_add(128));
    }
    if let Some(path) = &document.work_path {
        add(path_bytes(path));
    }
    for note in &document.notes {
        add(note.author.capacity().saturating_add(note.text.capacity()).saturating_add(note.modified.capacity()).saturating_add(256));
    }
    for (_, _, layer) in document.walk() {
        add(std::mem::size_of::<photocraft_doc::Layer>().saturating_add(layer.name.capacity()));
        add(text(&layer.xmp));
        add(effects_bytes(&layer.effects));
        add(auxiliary_bytes(&layer.blend_if));
        if let Some(cache) = &layer.fill_cache {
            add(auxiliary_bytes(&cache.fill));
        }
        if let Some(video) = &layer.video {
            add(auxiliary_bytes(&video.source));
        }
        for (_, bytes) in &layer.psd_blocks {
            add(bytes.capacity().saturating_add(32));
        }
        if let Some(mask) = &layer.vector_mask {
            add(path_bytes(&mask.path));
        }
        match &layer.content {
            LayerContent::Text(value) => {
                add(value.text.capacity().saturating_add(value.font_family.capacity()).saturating_add(blob(&value.psd_raw)));
                add(auxiliary_bytes(&value.runs));
                add(auxiliary_bytes(&value.paragraphs));
                add(auxiliary_bytes(&value.shape));
            }
            LayerContent::Shape(value) => {
                add(blob(&value.psd_raw).saturating_add(path_bytes(&value.path)));
                add(auxiliary_bytes(&value.fill));
                add(auxiliary_bytes(&value.stroke));
            }
            LayerContent::Smart(value) => {
                add(blob(&value.psd_raw));
                add(auxiliary_bytes(&value.warp));
                match &value.source {
                    photocraft_doc::SmartSource::Linked { path } => add(path.capacity()),
                    photocraft_doc::SmartSource::Embedded { file_name, bytes } => {
                        // History counts the source length, but its Arc retains spare capacity too.
                        add(file_name.capacity().saturating_add(bytes.capacity().saturating_sub(bytes.len())));
                    }
                }
                if let Some(preview) = &value.vector_preview {
                    add(preview.svg.capacity());
                }
                for filter in &value.smart_filters {
                    add(std::mem::size_of::<photocraft_doc::SmartFilter>()
                        .saturating_add(filter.command.capacity())
                        .saturating_add(json_metadata_bytes(&filter.params)));
                }
            }
            LayerContent::Adjustment(value) => add(adjustment_bytes(value)),
            LayerContent::Fill(value) => add(auxiliary_bytes(value)),
            LayerContent::Group(value) => {
                add(auxiliary_bytes(&value.artboard));
                add(auxiliary_bytes(&value.frame_stroke));
            }
            LayerContent::Raster(_) => {}
        }
    }
    total
}

fn effects_bytes(effects: &photocraft_doc::Effects) -> usize {
    auxiliary_bytes(&effects.items).saturating_add(effects.psd_raw.as_ref().map_or(0, |bytes| bytes.capacity()))
}

fn comp_bytes(comp: &photocraft_doc::LayerComp) -> usize {
    let base = std::mem::size_of::<photocraft_doc::LayerComp>()
        .saturating_add(comp.name.capacity())
        .saturating_add(comp.comment.capacity())
        .saturating_add(comp.states.capacity().saturating_mul(std::mem::size_of::<photocraft_doc::CompLayerState>()));
    comp.states.iter().fold(base, |bytes, state| bytes.saturating_add(state.appearance.as_ref().map_or(0, |appearance| effects_bytes(&appearance.effects))))
}

fn adjustment_bytes(adjustment: &photocraft_doc::Adjustment) -> usize {
    match adjustment {
        photocraft_doc::Adjustment::ColorLookup { name, lut, source, .. } => name
            .capacity()
            .saturating_add(lut.as_ref().map_or(0, |values| values.capacity().saturating_mul(std::mem::size_of::<f32>())))
            .saturating_add(source.as_ref().map_or(0, |source| source.bytes.capacity())),
        photocraft_doc::Adjustment::Unsupported { psd_key, raw } => psd_key.capacity().saturating_add(raw.len()),
        _ => auxiliary_bytes(adjustment),
    }
}

fn text_styles_bytes(styles: &photocraft_doc::TextStyles) -> usize {
    let attrs = |attrs: &serde_json::Map<String, Value>| {
        attrs.iter().fold(0usize, |bytes, (key, value)| bytes.saturating_add(key.capacity()).saturating_add(64).saturating_add(json_metadata_bytes(value)))
    };
    let mut bytes = styles.psd_raw.as_ref().map_or(0, json_metadata_bytes);
    for style in &styles.character {
        bytes = bytes.saturating_add(std::mem::size_of_val(style)).saturating_add(style.name.capacity()).saturating_add(attrs(&style.attrs));
    }
    for style in styles.paragraph.iter().chain(std::iter::once(&styles.basic)) {
        bytes = bytes
            .saturating_add(std::mem::size_of_val(style))
            .saturating_add(style.name.capacity())
            .saturating_add(attrs(&style.para_attrs))
            .saturating_add(attrs(&style.char_attrs));
    }
    bytes
}

/// Walk arbitrary PSD/style JSON with bounded stack depth and no serialization or subtree clone.
fn json_metadata_bytes(root: &Value) -> usize {
    enum Children<'a> {
        Array(std::slice::Iter<'a, Value>),
        Object(serde_json::map::Iter<'a>),
    }
    let mut stack = Vec::new();
    let mut next = Some(root);
    let mut bytes = 0usize;
    loop {
        if let Some(value) = next.take() {
            bytes = bytes.saturating_add(std::mem::size_of::<Value>());
            match value {
                Value::String(value) => bytes = bytes.saturating_add(value.capacity()),
                Value::Array(values) => {
                    bytes = bytes.saturating_add(values.capacity().saturating_mul(std::mem::size_of::<Value>()));
                    stack.push(Children::Array(values.iter()));
                }
                Value::Object(values) => {
                    bytes = bytes.saturating_add(values.len().saturating_mul(128));
                    stack.push(Children::Object(values.iter()));
                }
                _ => {}
            }
            if bytes > MAX_PENDING_BYTES || stack.len() > 128 {
                return MAX_PENDING_BYTES.saturating_add(1);
            }
        }
        let Some(children) = stack.last_mut() else { return bytes };
        next = match children {
            Children::Array(values) => values.next(),
            Children::Object(values) => values.next().map(|(key, value)| {
                bytes = bytes.saturating_add(key.capacity());
                value
            }),
        };
        if next.is_none() {
            stack.pop();
        }
    }
}

fn path_bytes(path: &photocraft_doc::vector::Path) -> usize {
    path.subpaths.iter().fold(0usize, |bytes, subpath| {
        bytes.saturating_add(64).saturating_add(subpath.knots.capacity().saturating_mul(std::mem::size_of::<photocraft_doc::vector::Knot>()))
    })
}

/// These ancillary types have bounded-depth schemas. Count without building JSON strings;
/// the multiplier conservatively covers Vec/enum/string allocation overhead for short values.
fn auxiliary_bytes(value: &impl serde::Serialize) -> usize {
    #[derive(Default)]
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 = self.0.saturating_add(bytes.len());
            if self.0 > MAX_PENDING_BYTES / 8 {
                return Err(std::io::Error::other("metadata budget exceeded"));
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Counter::default();
    if serde_json::to_writer(&mut count, value).is_err() {
        return MAX_PENDING_BYTES.saturating_add(1);
    }
    count.0.saturating_mul(8)
}

fn checkpoint(command: &str) -> ReplayRecord {
    ReplayRecord { schema_version: LOCAL_SCHEMA_VERSION, command: command.into(), params: Value::Null, elapsed_ms: 0, kind: ReplayKind::Checkpoint }
}

fn resolve_record(session: &Session, id: &str, params: &Value) -> Result<ReplayRecord> {
    let input_bytes = value_bytes(params)?;
    let mut record = checkpoint(id);
    let mut input = params.clone();
    let state = session.active();
    let selected: Vec<_> = state.map(|s| s.selected_layers().iter().map(|id| id.0).collect()).unwrap_or_default();
    let active = state.and_then(|s| s.active_layer).map(|id| id.0);
    let mut context = json!({
        "operationVersion": 1,
        "activeLayer": active,
        "selectedLayers": selected,
        "foreground": session.tools.foreground,
        "background": session.tools.background,
        "cmykPicks": session.tools.cmyk_picks,
        "maskTargeted": session.tools.mask_targeted,
    });
    if matches!(id, "paint.stroke" | "paint.pencil") {
        let preset = params.get("preset").and_then(Value::as_str).and_then(|name| photocraft_paint::presets::find(&session.tools.presets, name));
        // Inspect shared tile lengths before Serialize can create both raw bytes and base64.
        // Oversized assets use the required explicit result checkpoint fallback. Conservatively
        // inspect both preset and tool brush because locked sections can combine their assets.
        let initial_cost = brush_asset_cost(&session.tools.brush).saturating_add(preset.map_or(0, |p| brush_asset_cost(&p.brush)));
        if initial_cost.saturating_add(input_bytes).saturating_add(128 * 1024) > MAX_RECORD_BYTES {
            return brush_checkpoint(record, input, context);
        }
        let brush = crate::brush_cmds::resolve_brush(session, params, id)?;
        if brush_asset_cost(&brush).saturating_add(input_bytes).saturating_add(128 * 1024) > MAX_RECORD_BYTES {
            return brush_checkpoint(record, input, context);
        }
        let points = crate::brush_cmds::parse_points(params, id)?;
        if let Some(fields) = input.as_object_mut() {
            fields.remove("preset");
            fields.insert("brush".into(), serde_json::to_value(&brush).map_err(json_error)?);
            fields.insert("points".into(), serde_json::to_value(points).map_err(json_error)?);
            fields.insert("seed".into(), json!(brush.seed));
            fields.insert("color".into(), json!(brush.color));
            // An explicitly named layer may not be the selected layer. Resolve its real ID.
            if let Ok(layer) = crate::commands::layer_param(session, params) {
                fields.insert("layer".into(), json!(layer.0));
            }
        }
        context["symmetry"] = match state.and_then(|s| s.symmetry.as_ref()) {
            Some(crate::symmetry_cmds::PaintingSymmetry::Path(axis)) => json!({"mode":"path", "source":axis.source, "segments":axis.segments}),
            Some(crate::symmetry_cmds::PaintingSymmetry::Preset(preset)) => serde_json::to_value(preset).map_err(json_error)?,
            None => Value::Null,
        };
        context["brushInput"] = json!("fully resolved before deterministic pencil, mask and transparency-lock adaptation");
        record.kind = ReplayKind::Resolved;
    } else if id == "edit.undo" {
        record.kind = ReplayKind::Undo;
    } else if id == "edit.redo" {
        record.kind = ReplayKind::Redo;
    }
    record.params = json!({"input": input, "context": context});
    value_bytes(&record.params)?;
    Ok(record)
}

fn brush_checkpoint(mut record: ReplayRecord, input: Value, context: Value) -> Result<ReplayRecord> {
    record.params = json!({
        "input": input,
        "context": context,
        "checkpointReason": "Resolved brush assets exceed the 8 MiB replay-record budget; this completed stroke is retained as a lossless checkpoint step.",
    });
    value_bytes(&record.params)?;
    Ok(record)
}

fn brush_asset_cost(brush: &photocraft_paint::BrushSettings) -> usize {
    use photocraft_paint::{Pattern, TipShape};
    let stored = |tile: &photocraft_paint::StoredTile| (tile.width as usize).saturating_mul(tile.height as usize).max(tile.bitmap().data.len());
    let tip = |tip: &TipShape| match tip {
        TipShape::Round => 0,
        TipShape::Sampled(tile) => tile.data.len(),
        TipShape::Stored(tile) => stored(tile),
    };
    let pattern = match &brush.texture.pattern {
        Pattern::Procedural { .. } => 0,
        Pattern::Tile(tile) => tile.data.len(),
        Pattern::Stored(tile) => stored(tile),
    };
    // Each u16 produces <= 4 base64 bytes, conservatively charged as JSON string escaping by
    // value_bytes. This upper bound avoids serializing a huge sampled tip just to reject it.
    tip(&brush.tip).saturating_add(tip(&brush.dual_brush.tip)).saturating_add(pattern).saturating_mul(24)
}

fn json_error(error: serde_json::Error) -> EngineError {
    EngineError::Other(format!("Cannot record local replay: {error}"))
}

/// A bounded traversal prevents hostile nested params from reaching the serializer. Its
/// conservative byte estimate counts escaping, punctuation and UTF-8 before any deep clone.
fn value_bytes(value: &Value) -> Result<usize> {
    let mut pending = vec![(value, 0usize)];
    let mut bytes = 0usize;
    while let Some((value, depth)) = pending.pop() {
        if depth > MAX_RECORD_DEPTH {
            return Err(EngineError::Other("Local replay parameters exceed the nesting limit.".into()));
        }
        bytes = bytes.saturating_add(match value {
            Value::String(s) => s.len().saturating_mul(6).saturating_add(2),
            Value::Array(a) => {
                if a.len().saturating_add(pending.len()) > MAX_RECORD_BYTES / 16 {
                    return Err(EngineError::Other("Local replay parameters contain too many values.".into()));
                }
                pending.extend(a.iter().map(|v| (v, depth + 1)));
                a.len().saturating_add(2)
            }
            Value::Object(o) => {
                if o.len().saturating_add(pending.len()) > MAX_RECORD_BYTES / 16 {
                    return Err(EngineError::Other("Local replay parameters contain too many fields.".into()));
                }
                pending.extend(o.values().map(|v| (v, depth + 1)));
                o.keys().fold(2usize, |n, k| n.saturating_add(k.len().saturating_mul(6)).saturating_add(4))
            }
            _ => 32,
        });
        if bytes > MAX_RECORD_BYTES || pending.len() > MAX_RECORD_BYTES / 16 {
            return Err(EngineError::Other("Local replay parameters exceed 8 MiB. Shorten the stroke or use a smaller brush asset before retrying.".into()));
        }
    }
    Ok(bytes)
}
