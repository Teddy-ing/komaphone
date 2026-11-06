//! The durable replay payload owns its sampled asset and resolved settings.
#![cfg(not(target_arch = "wasm32"))]

use std::time::Duration;

use photocraft_engine::Session;
use photocraft_local_store::{ReplayKind, Store, StoreOptions};
use photocraft_paint::{BrushSettings, GrayTile, TipShape};
use serde_json::json;

#[test]
fn sampled_asset_and_settings_replay_from_sqlite_matches_canonical_checkpoints() {
    for depth in [8, 16, 32] {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("drafts.sqlite3");
        let mut original = Session::new();
        original.enable_local_projects(path.clone(), StoreOptions::default()).unwrap();
        original.flush_local_projects(Duration::from_secs(20)).unwrap();
        original.execute("file.new", json!({"width":64,"height":64,"depth":depth,"background":"transparent"})).unwrap();
        // An asymmetric, nonuniform tip makes accidentally substituting a round tip visible.
        let tip = GrayTile {
            width: 5,
            height: 5,
            data: vec![0, 0, 9000, 0, 0, 0, 18000, 40000, 12000, 0, 0, 32000, 65535, 48000, 20000, 0, 7000, 26000, 16000, 0, 0, 0, 3000, 0, 0].into(),
        };
        original.tools.brush.tip = TipShape::Sampled(tip.clone());
        original.tools.brush.size = 13.0;
        original.tools.brush.opacity = 0.7;
        original.tools.brush.flow = 0.6;
        original.tools.brush.angle = 27.0;
        original.tools.brush.spacing = 0.25;
        original.tools.brush.scattering.enabled = true;
        original.tools.brush.scattering.scatter.jitter = 0.2;
        original.tools.foreground = [0.2, 0.6, 0.8, 1.0];
        original.tools.background = [0.1, 0.3, 0.5, 1.0];
        original.execute("paint.stroke", json!({"points":[[10,12,0.4],[28,24,0.7],[46,43,1]],"seed":1729})).unwrap();
        let painted = original.active().unwrap().doc.clone();
        let painted_pixels = photocraft_compose::flatten(&painted).px.clone();
        assert!(painted_pixels.iter().any(|pixel| pixel[3] > 0.0), "fixture must actually paint at depth {depth}");
        original.execute("layer.setProps", json!({"opacity":0.45})).unwrap();
        let faded_pixels = photocraft_compose::flatten(&original.active().unwrap().doc).px.clone();
        assert_ne!(painted_pixels, faded_pixels, "checkpoint step must change the canonical result");
        original.execute("edit.undo", json!({})).unwrap();
        original.execute("edit.redo", json!({})).unwrap();
        let final_document = original.active().unwrap().doc.clone();
        original.flush_local_projects(Duration::from_secs(20)).unwrap();
        let status = original.local_projects.status(final_document.id);
        assert_eq!(status.local_revision, 5);
        assert_eq!(status.pending, 0);
        let project = status.project.unwrap();
        // Drop the producer and its tool/asset state before reading the durable recording.
        drop(original);

        let store = Store::open(&path, StoreOptions::default()).unwrap();
        let records = store.replay_page(project, 0, 16).unwrap();
        assert_eq!(records.len(), 5);
        assert_eq!(records[0].kind, ReplayKind::Baseline);
        assert_eq!(records[1].kind, ReplayKind::Resolved);
        assert_eq!(records[2].kind, ReplayKind::Checkpoint);
        assert_eq!(records[3].kind, ReplayKind::Undo);
        assert_eq!(records[4].kind, ReplayKind::Redo);
        let stroke = &records[1];
        let retained: BrushSettings = serde_json::from_value(stroke.params["input"]["brush"].clone()).unwrap();
        assert_eq!(retained.tip, TipShape::Sampled(tip));
        assert_eq!(retained.size, 13.0);
        assert_eq!(retained.opacity, 0.7);
        assert_eq!(retained.flow, 0.6);
        assert_eq!(retained.angle, 27.0);
        assert_eq!(retained.spacing, 0.25);
        assert!(retained.scattering.enabled);
        assert_eq!(retained.scattering.scatter.jitter, 0.2);
        assert_eq!(retained.seed, 1729);
        assert_eq!(stroke.params["input"]["points"].as_array().unwrap().len(), 3);
        let (baseline, _) = store.load_revision(project, 1).unwrap();
        let mut replay = Session::new();
        replay.add_document(baseline, None);
        replay.tools.brush = BrushSettings { size: 2.0, opacity: 1.0, tip: TipShape::Round, ..BrushSettings::default() };
        replay.tools.foreground = [1.0, 0.0, 0.0, 1.0];
        replay.tools.background = serde_json::from_value(stroke.params["context"]["background"].clone()).unwrap();
        replay.execute(&stroke.command, stroke.params["input"].clone()).unwrap();
        let replayed = &replay.active().unwrap().doc;
        let (stroke_checkpoint, _) = store.load_revision(project, 2).unwrap();
        let layer = replay.active().unwrap().active_layer.unwrap();
        assert_eq!(
            replayed.layer(layer).unwrap().surface().unwrap().read_region(replayed.bounds()),
            stroke_checkpoint.layer(layer).unwrap().surface().unwrap().read_region(stroke_checkpoint.bounds()),
            "stored sampled asset must replay byte-exactly at depth {depth}"
        );
        assert_eq!(photocraft_compose::flatten(replayed).px, painted_pixels);
        assert_eq!(photocraft_compose::flatten(&stroke_checkpoint).px, painted_pixels);
        // Checkpoint and undo/redo steps use their retained native result as authoritative.
        for (revision, expected) in [(3, &faded_pixels), (4, &painted_pixels), (5, &faded_pixels)] {
            let (checkpoint, info) = store.load_revision(project, revision).unwrap();
            assert_eq!(info.revision, revision);
            assert_eq!(&photocraft_compose::flatten(&checkpoint).px, expected, "revision {revision}, depth {depth}");
        }
        assert_eq!(store.load(project).unwrap().document, *final_document);
    }
}
