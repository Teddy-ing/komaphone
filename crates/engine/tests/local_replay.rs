//! Canonical local replay is independent from the portable/coalesced Actions journal.
use std::sync::Arc;

use photocraft_engine::Session;
use photocraft_local_store::ReplayKind;
use serde_json::json;

fn drawing() -> Session {
    let mut session = Session::new();
    session.local_replay.enable();
    session.execute("file.new", json!({"width":64,"height":64,"background":"transparent"})).unwrap();
    session
}

#[test]
fn first_new_document_has_baseline_before_the_first_stroke() {
    let mut session = drawing();
    let layer = session.active().unwrap().active_layer.unwrap().0;
    session.execute("tools.setBrush", json!({"size":7,"hardness":0.8,"opacity":0.6})).unwrap();
    session.tools.foreground = [0.3, 0.5, 0.7, 1.0];
    session.tools.background = [0.2, 0.4, 0.6, 1.0];
    session.execute("paint.stroke", json!({"points":[[8,8,1,0,0,0,12],[24,24,0.7,0,0,0,42]],"seed":1827})).unwrap();
    let baseline = session.local_replay.pop().unwrap();
    let stroke = session.local_replay.pop().unwrap();
    assert_eq!(baseline.record.kind, ReplayKind::Baseline);
    assert_eq!(baseline.record.command, "file.new");
    assert_eq!(stroke.record.kind, ReplayKind::Resolved);
    assert_eq!(stroke.record.command, "paint.stroke");
    assert_eq!(stroke.record.params["input"]["seed"], 1827);
    assert_eq!(stroke.record.params["input"]["layer"], layer);
    assert_eq!(stroke.record.params["input"]["brush"]["size"], 7.0);
    assert_eq!(stroke.record.params["input"]["points"][1]["time"], 42.0);
    assert_eq!(stroke.record.params["context"]["activeLayer"], layer);
    assert!(stroke.record.params["context"]["background"].is_array());
    assert!(!Arc::ptr_eq(&baseline.document, &stroke.document));
    assert!(session.local_replay.pop().is_none());
    session.tools.brush.size = 21.0;
    assert_eq!(stroke.record.params["input"]["brush"]["size"], 7.0, "later tool changes cannot alter a recorded brush");
}

#[test]
fn undo_redo_and_coalesced_edits_remain_distinct_boundaries() {
    let mut session = drawing();
    session.execute("paint.stroke", json!({"points":[[12,12],[24,24]],"size":5})).unwrap();
    session.execute("layer.setProps", json!({"opacity":0.4,"coalesce":"opacity-slider"})).unwrap();
    session.execute("layer.setProps", json!({"opacity":0.7,"coalesce":"opacity-slider"})).unwrap();
    assert!(session.undo());
    assert!(session.redo());
    let events: Vec<_> = std::iter::from_fn(|| session.local_replay.pop()).collect();
    assert_eq!(events.len(), 6, "baseline, stroke, both slider edits, undo, redo");
    assert_eq!(events[2].record.kind, ReplayKind::Checkpoint);
    assert_eq!(events[2].record.params["input"]["opacity"], 0.4);
    assert_eq!(events[3].record.params["input"]["opacity"], 0.7);
    assert_eq!(events[4].record.kind, ReplayKind::Undo);
    assert_eq!(events[5].record.kind, ReplayKind::Redo);
    let selected = session.active().unwrap().active_layer.unwrap();
    assert!((events[4].document.layer(selected).unwrap().opacity - 1.0).abs() < 0.001);
    assert!((events[5].document.layer(selected).unwrap().opacity - 0.7).abs() < 0.001);
}

#[test]
fn invalid_commands_do_not_create_replay_steps() {
    let mut session = drawing();
    let before = session.local_replay.pending_len();
    assert!(session.execute("paint.stroke", json!({"points":[]})).is_err());
    assert!(session.execute("layer.setProps", json!({"layer":999999,"opacity":0.2})).is_err());
    assert_eq!(session.local_replay.pending_len(), before);
}

#[test]
fn direct_document_change_gets_an_explicit_checkpoint() {
    let mut session = drawing();
    let state = session.active_mut().unwrap();
    Arc::make_mut(&mut state.doc).name = "Renamed outside a command".into();
    state.revision += 1;
    session.capture_local_fallback().unwrap();
    let baseline = session.local_replay.pop().unwrap();
    let checkpoint = session.local_replay.pop().unwrap();
    assert_eq!(checkpoint.record.kind, ReplayKind::Checkpoint);
    assert_eq!(checkpoint.record.command, "checkpoint.externalEdit");
    assert_ne!(baseline.document.name, checkpoint.document.name);
    session.capture_local_fallback().unwrap();
    assert_eq!(session.local_replay.pending_len(), 0, "an unchanged frame doesn't add history");
}

#[test]
fn full_queue_blocks_an_edit_without_changing_the_document() {
    let mut session = drawing();
    for index in 1..photocraft_engine::local_replay::MAX_PENDING_EDITS {
        session.execute("layer.renameLayer", json!({"name":format!("Layer {index}")})).unwrap();
    }
    let before = session.active().unwrap().doc.clone();
    assert!(session.execute("layer.renameLayer", json!({"name":"This must not apply"})).is_err());
    assert!(Arc::ptr_eq(&before, &session.active().unwrap().doc));
    assert!(session.execute("document.inspect", json!({})).is_ok(), "queries still work while saving is blocked");
    session.local_replay.pop().unwrap();
    session.execute("layer.renameLayer", json!({"name":"Retry after draining"})).unwrap();
    assert!(!Arc::ptr_eq(&before, &session.active().unwrap().doc));
}

#[test]
fn adopting_a_reopened_draft_preserves_the_replay_clock_without_a_new_baseline() {
    let mut original = drawing();
    let doc = (*original.active().unwrap().doc).clone();
    while original.local_replay.pop().is_some() {}
    let mut reopened = Session::new();
    reopened.add_document(doc, None);
    let state = reopened.active().unwrap().clone();
    reopened.local_replay.enable();
    reopened.local_replay.adopt(&state, 1500);
    reopened.execute("layer.renameLayer", json!({"name":"After reopen"})).unwrap();
    let edit = reopened.local_replay.pop().unwrap();
    assert_eq!(edit.record.kind, ReplayKind::Checkpoint);
    assert!(edit.record.elapsed_ms >= 1500);
    assert!(reopened.local_replay.pop().is_none());
}

#[test]
fn pending_snapshot_survives_closing_its_editor_tab() {
    let mut session = drawing();
    session.execute("paint.stroke", json!({"points":[[10,10],[20,20]],"size":4})).unwrap();
    let id = session.active().unwrap().doc.id;
    assert!(session.close(0).is_some());
    assert_eq!(session.local_replay.pending_for(id), 2);
    assert!(session.local_replay.pop().is_some());
    let stroke = session.local_replay.pop().unwrap();
    assert_eq!(stroke.document.id, id);
    assert_eq!(stroke.record.kind, ReplayKind::Resolved);
}

#[test]
fn failed_compound_edit_that_rolls_back_leaves_no_invented_history() {
    let mut session = drawing();
    let before = session.active().unwrap().doc.clone();
    let context = session.begin_local_replay("compound.fixture", &json!({})).unwrap();
    session
        .edit("temporary edit", |doc, _| {
            doc.resolution_dpi = 144.0;
            Ok(())
        })
        .unwrap();
    assert!(session.rollback());
    session.end_local_replay(context, false).unwrap();
    assert!(Arc::ptr_eq(&before, &session.active().unwrap().doc));
    assert_eq!(session.local_replay.pending_len(), 1, "only the original baseline remains");
}

#[test]
#[cfg(not(target_arch = "wasm32"))]
fn excluded_external_tab_does_not_persist_a_rolled_back_local_edit() {
    use std::time::Duration;

    let temp = tempfile::tempdir().unwrap();
    let database = temp.path().join("drafts.sqlite3");
    let mut session = Session::new();
    session.enable_local_projects(database.clone(), Default::default()).unwrap();
    session.execute("file.new", json!({"width":6000,"height":4000,"background":"transparent"})).unwrap();
    let external = session.active().unwrap().doc.id;
    assert!(session.local_replay.excluded_reason(external).is_some());
    session.execute("komaphone.project.new", json!({"width":32,"height":32})).unwrap();
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    let before = session.active().unwrap().doc.clone();
    let saved = session.local_projects.status(before.id);
    assert_eq!(saved.local_revision, 1);
    let context = session.begin_local_replay("compound.fixture", &json!({})).unwrap();
    session
        .edit("temporary edit", |doc, _| {
            doc.resolution_dpi = 144.0;
            Ok(())
        })
        .unwrap();
    session.execute("komaphone.project.status", json!({})).unwrap();
    assert!(session.rollback());
    session.end_local_replay(context, false).unwrap();
    assert!(Arc::ptr_eq(&before, &session.active().unwrap().doc));
    assert_eq!(session.local_replay.pending_len(), 0, "the failed edit creates no replay transition");
    session.flush_local_projects(Duration::from_secs(20)).unwrap();
    assert_eq!(session.local_projects.status(before.id).local_revision, saved.local_revision);
    let store = photocraft_local_store::Store::open(database, Default::default()).unwrap();
    let loaded = store.load(saved.project.unwrap()).unwrap();
    assert_eq!(loaded.info.revision, saved.local_revision);
    assert_eq!(store.replay_page(saved.project.unwrap(), 0, 16).unwrap().len(), 1, "only the durable baseline remains");
    assert_eq!(*before, loaded.document);
}

#[test]
fn failed_command_with_a_changed_managed_document_set_retains_its_events() {
    for change in ["add", "remove", "replace"] {
        let mut session = drawing();
        let original = session.active().unwrap().doc.id;
        let context = session.begin_local_replay("compound.fixture", &json!({})).unwrap();
        session
            .edit("completed partial edit", |doc, _| {
                doc.resolution_dpi = 144.0;
                Ok(())
            })
            .unwrap();
        assert!(session.rollback());
        if change != "add" {
            session.close(0).unwrap();
        }
        let added = if change != "remove" {
            session.execute("file.new", json!({"width":32,"height":32})).unwrap();
            Some(session.active().unwrap().doc.id)
        } else {
            None
        };
        session.end_local_replay(context, false).unwrap();
        let events: Vec<_> = std::iter::from_fn(|| session.local_replay.pop()).collect();
        assert!(events.iter().any(|edit| edit.document.id == original && edit.document.resolution_dpi == 144.0), "{change}: partial events remain");
        if let Some(added) = added {
            assert!(events.iter().any(|edit| edit.document.id == added && edit.record.kind == ReplayKind::Baseline), "{change}: the new baseline remains");
        }
    }
}

#[test]
fn resolved_stroke_reproduces_pixels_after_tools_change_at_multiple_depths() {
    for depth in [8, 16, 32] {
        for command in ["paint.stroke", "paint.pencil"] {
            let mut original = Session::new();
            original.local_replay.enable();
            original.execute("file.new", json!({"width":64,"height":64,"depth":depth,"background":"transparent"})).unwrap();
            original.tools.brush.size = 9.0;
            original.tools.brush.hardness = 0.35;
            original.tools.brush.opacity = 0.7;
            original.tools.brush.scattering.enabled = true;
            original.tools.brush.scattering.scatter.jitter = 0.3;
            original.tools.foreground = [0.2, 0.6, 0.8, 1.0];
            original.tools.background = [0.1, 0.3, 0.5, 1.0];
            original.execute(command, json!({"points":[[10,10,0.3],[30,20,0.7],[40,40,1]],"seed":1729})).unwrap();
            let baseline = original.local_replay.pop().unwrap();
            let stroke = original.local_replay.pop().unwrap();
            let mut replay = Session::new();
            replay.add_document((*baseline.document).clone(), None);
            // Deliberately different defaults prove the retained payload resolves settings.
            replay.tools.brush.size = 2.0;
            replay.tools.foreground = [1.0, 0.0, 0.0, 1.0];
            replay.tools.background = serde_json::from_value(stroke.record.params["context"]["background"].clone()).unwrap();
            replay.execute(&stroke.record.command, stroke.record.params["input"].clone()).unwrap();
            let active = replay.active().unwrap().active_layer.unwrap();
            let expected = stroke.document.layer(active).unwrap().surface().unwrap().read_region(stroke.document.bounds());
            let state = replay.active().unwrap();
            let actual = state.doc.layer(active).unwrap().surface().unwrap().read_region(state.doc.bounds());
            assert_eq!(actual, expected, "{command} depth={depth}");
        }
    }
}

#[test]
fn large_sampled_brush_uses_an_explicit_checkpoint_without_serializing_the_asset() {
    let mut session = drawing();
    // 2 MiB source data is enough to cross the conservative serialized replay budget;
    // the fixture does not allocate a giant tip or document to exercise the guard.
    session.tools.brush.tip =
        photocraft_paint::TipShape::Sampled(photocraft_paint::GrayTile { width: 1024, height: 1024, data: vec![u16::MAX; 1024 * 1024].into() });
    session.execute("paint.stroke", json!({"points":[[24,24]],"size":8})).unwrap();
    session.local_replay.pop().unwrap();
    let edit = session.local_replay.pop().unwrap();
    assert_eq!(edit.record.kind, ReplayKind::Checkpoint);
    assert!(edit.record.params["checkpointReason"].as_str().unwrap().contains("brush assets"));
    assert!(edit.record.params["input"].get("brush").is_none(), "no serialized tip is allocated for the checkpoint fallback");
    assert!(serde_json::to_vec(&edit.record).unwrap().len() < 4096);
    let active = session.active().unwrap().active_layer.unwrap();
    assert!(edit.document.layer(active).unwrap().surface().unwrap().rgba(24, 24)[3] > 0.0);
}

#[test]
fn large_legacy_documents_keep_the_advanced_editor_without_blocking_local_drafts() {
    let mut session = Session::new();
    session.local_replay.enable();
    session.execute("file.new", json!({"width":6000,"height":4000,"background":"transparent"})).unwrap();
    let external = session.active().unwrap().doc.id;
    assert!(session.local_replay.excluded_reason(external).unwrap().contains("File Save"));
    session.execute("layer.renameLayer", json!({"name":"External layer still editable"})).unwrap();
    assert_eq!(session.local_replay.pending_len(), 0);
    session.execute("file.new", json!({"width":64,"height":64,"background":"transparent"})).unwrap();
    session.execute("paint.stroke", json!({"points":[[12,12],[24,24]],"size":4})).unwrap();
    assert_eq!(session.local_replay.pending_len(), 2);
    assert!(session.local_replay.excluded_reason(session.active().unwrap().doc.id).is_none());
}

#[test]
fn local_draft_size_growth_is_rejected_before_mutating_the_document() {
    let mut session = drawing();
    let before = session.active().unwrap().doc.clone();
    assert!(
        session
            .edit("oversized local resize", |doc, _| {
                doc.size = photocraft_geom::Size::new(6000, 4000);
                Ok(())
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.active().unwrap().doc));
    assert_eq!(session.local_replay.pending_len(), 1);
}

#[test]
fn blocked_project_is_skipped_without_reordering_its_later_edits() {
    let mut session = drawing();
    let first = session.active().unwrap().doc.id;
    session.execute("layer.renameLayer", json!({"name":"First edit"})).unwrap();
    session.execute("file.new", json!({"width":32,"height":32})).unwrap();
    let second = session.active().unwrap().doc.id;
    let blocked = std::collections::HashSet::from([first]);
    assert_eq!(session.local_replay.peek_unblocked(&blocked).unwrap().document.id, second);
    assert_eq!(session.local_replay.pop_unblocked(&blocked).unwrap().document.id, second);
    assert!(session.local_replay.pop_unblocked(&blocked).is_none());
    let baseline = session.local_replay.pop().unwrap();
    let edit = session.local_replay.pop().unwrap();
    assert_eq!(baseline.document.id, first);
    assert_eq!(baseline.record.kind, ReplayKind::Baseline);
    assert_eq!(edit.document.id, first);
    assert!(edit.editor_revision > baseline.editor_revision);
}

#[test]
fn retained_snapshot_budget_includes_metadata_even_when_no_pixels_are_allocated() {
    let mut session = drawing();
    let mut document = (*session.active().unwrap().doc).clone();
    let before = photocraft_engine::local_replay::snapshot_retained_bytes(&document);
    document.metadata.exif = Some(Arc::new(vec![7; 1024 * 1024]));
    document.icc_profile = Some(Arc::new(vec![8; 4096]));
    document.metadata.xmp = Some(Arc::from("x".repeat(8192)));
    let after = photocraft_engine::local_replay::snapshot_retained_bytes(&document);
    assert!(after >= before + 1024 * 1024 + 4096 + 8192);
    session.local_replay.pop().unwrap();
    assert!(session.check_new_local_replay_capacity().is_ok());
}

fn comp_with_raw_effects(layer: photocraft_doc::LayerId, raw: Arc<Vec<u8>>) -> photocraft_doc::LayerComp {
    photocraft_doc::LayerComp {
        id: 1,
        name: "Appearance".into(),
        comment: String::new(),
        apply_visibility: false,
        apply_position: false,
        apply_appearance: true,
        states: vec![photocraft_doc::CompLayerState {
            layer,
            visible: None,
            position: None,
            appearance: Some(photocraft_doc::CompAppearance {
                blend: photocraft_color::BlendMode::Normal,
                opacity: 1.0,
                fill_opacity: 1.0,
                effects: photocraft_doc::Effects { psd_raw: Some(raw), ..Default::default() },
            }),
        }],
    }
}

#[test]
fn retained_budget_includes_duotone_and_both_comp_appearance_sources() {
    let session = drawing();
    let mut document = (*session.active().unwrap().doc).clone();
    let before = photocraft_engine::local_replay::snapshot_retained_bytes(&document);
    let raw = Arc::new(vec![7; 1024 * 1024]);
    let comp = comp_with_raw_effects(session.active().unwrap().active_layer.unwrap(), raw);
    document.layer_comps.push(comp.clone());
    document.last_document_state = Some(comp);
    document.duotone = Some(photocraft_doc::Duotone { inks: Vec::new(), psd_raw: Some(Arc::from(vec![8; 1024 * 1024])) });
    let after = photocraft_engine::local_replay::snapshot_retained_bytes(&document);
    assert!(after >= before + 3 * 1024 * 1024, "each retained appearance and duotone payload is conservatively charged");
}

#[test]
fn repeated_metadata_references_reach_the_budget_before_a_local_edit_commits() {
    let mut session = drawing();
    let before = session.active().unwrap().doc.clone();
    let comp = comp_with_raw_effects(session.active().unwrap().active_layer.unwrap(), Arc::new(vec![7; 1024 * 1024]));
    // Only one MiB is allocated for the shared payload. The queue deliberately overcounts
    // retained references, so this exercises its full limit without a giant allocation.
    assert!(
        session
            .edit("excessive comp metadata", |document, _| {
                document.layer_comps = vec![comp; 513];
                Ok(())
            })
            .is_err()
    );
    assert!(Arc::ptr_eq(&before, &session.active().unwrap().doc));
    assert_eq!(session.local_replay.pending_len(), 1);
}

#[test]
fn retained_budget_counts_complete_text_styles_and_adjustment_assets() {
    use photocraft_doc::text::{CharStyle, FontFeature, FontVariation, TextRun};
    use photocraft_doc::{Adjustment, Layer, LayerContent, TextLayer};

    let session = drawing();
    let mut document = (*session.active().unwrap().doc).clone();
    document.layers.push(Layer::new("Type", LayerContent::Text(TextLayer::default())));
    let before = photocraft_engine::local_replay::snapshot_retained_bytes(&document);
    let label = "x".repeat(16 * 1024);
    let LayerContent::Text(text) = &mut document.layers.last_mut().unwrap().content else { panic!("text fixture") };
    text.runs.push(TextRun {
        len: 0,
        style: CharStyle {
            font_style: label.clone(),
            postscript_name: Some(label.clone()),
            language: Some(label.clone()),
            features: vec![FontFeature { tag: label.clone(), value: 1 }],
            variations: vec![FontVariation { axis: label, value: 1.0 }],
            ..Default::default()
        },
    });
    let styled = photocraft_engine::local_replay::snapshot_retained_bytes(&document);
    assert!(styled >= before + 5 * 16 * 1024, "style strings, feature tags, and variation axes all consume retained memory");
    document.layers.push(Layer::new(
        "Lookup",
        LayerContent::Adjustment(Adjustment::ColorLookup {
            name: "Test LUT".into(),
            lut: Some(Arc::new(vec![0.5; 256 * 1024])),
            size: 2,
            tetrahedral: false,
            dither: false,
            source: Some(photocraft_doc::adjust::LutSource { format: photocraft_doc::adjust::LutFormat::Cube, bytes: Arc::new(vec![1; 1024 * 1024]) }),
        }),
    ));
    let with_assets = photocraft_engine::local_replay::snapshot_retained_bytes(&document);
    assert!(with_assets >= styled + 2 * 1024 * 1024, "lookup values and original source bytes are both retained");
    document.text_styles.psd_raw = Some(json!({"unknown": "x".repeat(1024 * 1024)}));
    assert!(photocraft_engine::local_replay::snapshot_retained_bytes(&document) >= with_assets + 1024 * 1024);
    let mut nested = json!(null);
    for _ in 0..130 {
        nested = json!([nested]);
    }
    document.text_styles.psd_raw = Some(nested);
    assert!(photocraft_engine::local_replay::snapshot_retained_bytes(&document) > photocraft_engine::local_replay::MAX_PENDING_BYTES);
}

#[test]
fn full_save_queue_preserves_a_floating_move_until_drop_can_commit() {
    for return_to_origin in [false, true] {
        let mut session = drawing();
        session.execute("paint.stroke", json!({"points":[[16,16]],"size":6,"color":"#ff0000"})).unwrap();
        session.execute("select.rect", json!({"x":8,"y":8,"width":16,"height":16})).unwrap();
        while session.local_replay.pending_len() < photocraft_engine::local_replay::MAX_PENDING_EDITS {
            session.execute("layer.renameLayer", json!({"name":format!("Layer {}", session.local_replay.pending_len())})).unwrap();
        }
        session.execute("select.float", json!({"dx":8,"dy":0})).unwrap();
        if return_to_origin {
            session.execute("select.float", json!({"dx":-8,"dy":0})).unwrap();
        }
        let before = session.active().unwrap().doc.clone();
        let floating = photocraft_engine::float_cmds::floating(session.active().unwrap()).unwrap().clone();
        let preview = photocraft_engine::float_cmds::displayed(session.active().unwrap(), (0, 0)).unwrap();
        for command in ["select.drop", "komaphone.project.status"] {
            assert!(session.execute(command, json!({})).is_err(), "{command} must refuse the edit while the queue is full");
            let state = session.active().unwrap();
            assert!(Arc::ptr_eq(&before, &state.doc), "a refused drop must not mutate the document");
            let retained = photocraft_engine::float_cmds::floating(state).expect("the refused move remains available for retry");
            assert_eq!(retained.offset, floating.offset);
            assert_eq!(retained.revision, floating.revision);
            assert!(Arc::ptr_eq(&retained.parts, &floating.parts));
            assert_eq!(session.local_replay.pending_len(), photocraft_engine::local_replay::MAX_PENDING_EDITS);
        }
        session.local_replay.pop().unwrap();
        session.execute("select.drop", json!({})).unwrap();
        let state = session.active().unwrap();
        assert!(state.floating.is_none());
        let layer = state.active_layer.unwrap();
        // Returning to the origin changes only the selection. Preserve the original raw
        // pixels, including RGB beneath zero alpha, which the composited preview may omit.
        let expected = if return_to_origin { before.as_ref() } else { &preview };
        let actual = state.doc.layer(layer).unwrap().surface().unwrap().read_region(state.doc.bounds());
        let expected = expected.layer(layer).unwrap().surface().unwrap().read_region(expected.bounds());
        assert_eq!(actual.len(), expected.len());
        let mismatch_count = actual.iter().zip(&expected).filter(|(a, b)| a != b).count();
        let first_mismatch = actual.iter().zip(&expected).enumerate().find(|(_, (a, b))| a != b);
        assert_eq!(mismatch_count, 0, "retry must preserve every expected sample (return_to_origin={return_to_origin}); first mismatch: {first_mismatch:?}");
        assert_eq!(session.local_replay.pending_len(), photocraft_engine::local_replay::MAX_PENDING_EDITS);
    }
}
