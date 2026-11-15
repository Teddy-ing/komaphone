#![cfg(not(target_arch = "wasm32"))]
mod k4_media_support;
use k4_media_support::{add, checkpoint, local, manifest, project};
use photocraft_engine::{
    Session,
    gallery_media::{encode_png, remix, render_document, render_frame, replay_image, replay_info, replay_step},
};
use photocraft_local_store::ReplayKind;
use photocraft_party::gallery::{GalleryBundle, GalleryContent};
use photocraft_party::replay_artifact::{ReplayArtifact, ReplayArtifactStep, pack_checkpoints};
use photocraft_party::{EventId, ReplayId};
use serde_json::json;
use std::collections::BTreeMap;

#[test]
fn retained_steps_show_actual_edits_at_all_depths_and_finish_exactly() {
    for depth in [8, 16, 32] {
        let mut session = Session::new();
        session.local_replay.enable();
        session.execute("file.new", json!({"width":48,"height":48,"depth":depth,"background":"transparent"})).unwrap();
        session.execute("paint.stroke", json!({"points":[[4,6],[24,18],[38,36]],"size":9,"color":[0.2,0.7,0.3,1.0],"seed":29})).unwrap();
        session.execute("paint.stroke", json!({"points":[[20,17],[25,22]],"size":8,"erase":true})).unwrap();
        session.execute("layer.new.layer", json!({"name":"Fill layer"})).unwrap();
        session.execute("edit.fill", json!({"contents":"color","color":[0.7,0.1,0.8,0.4]})).unwrap();
        session.execute("edit.transform", json!({"matrix":[1,0,0,1,3,2],"interpolation":"nearest"})).unwrap();
        session.execute("layer.setProps", json!({"opacity":0.35})).unwrap();
        session.execute("edit.undo", json!({})).unwrap();
        session.execute("edit.redo", json!({})).unwrap();
        let import_dir = tempfile::tempdir().unwrap();
        let import_path = import_dir.path().join("synthetic.png");
        let imported = photocraft_engine::rooms::media::RoomImage { width: 4, height: 4, rgba: [255, 160, 0, 255].repeat(16) };
        std::fs::write(&import_path, encode_png(&imported).unwrap()).unwrap();
        session.execute("file.placeEmbedded", json!({"path":import_path.to_str().unwrap()})).unwrap();
        drop(import_dir); // Replay must use its retained checkpoint, not a live asset path.
        // An unsupported imported/direct edit gets an exact checkpoint, never arbitrary replay execution.
        session
            .edit("Imported pixels", |doc, _| {
                let mut layer = photocraft_doc::Layer::raster("Checkpoint-only pixels", doc.pixel_format());
                layer.surface_mut().unwrap().try_write_region(photocraft_geom::Rect::new(0, 0, 2, 2), &[0.0, 1.0, 1.0, 1.0].repeat(4)).unwrap();
                doc.layers.push(layer);
                Ok(())
            })
            .unwrap();
        session.capture_local_fallback().unwrap();
        let captured: Vec<_> = std::iter::from_fn(|| session.local_replay.pop()).collect();
        assert!(captured.iter().any(|e| e.record.kind == ReplayKind::Undo));
        assert!(captured.iter().any(|e| e.record.kind == ReplayKind::Redo));
        assert!(captured.iter().any(|e| e.record.kind == ReplayKind::Checkpoint));
        assert!(captured.iter().any(|e| e.record.command == "file.placeEmbedded"));
        let mut objects = BTreeMap::new();
        let mut steps = Vec::new();
        for (index, edit) in captured.iter().enumerate() {
            let checkpoint = checkpoint(&mut objects, &edit.document);
            let kind = match edit.record.kind {
                ReplayKind::Baseline => "baseline",
                ReplayKind::Resolved => "resolved",
                ReplayKind::Checkpoint => "checkpoint",
                ReplayKind::Undo => "undo",
                ReplayKind::Redo => "redo",
            };
            steps.push(ReplayArtifactStep {
                event: EventId::new(&format!("{:032x}", index + 1)).unwrap(),
                revision: index as u64 + 1,
                elapsed_ms: index as u64 * 100,
                command: edit.record.command.clone(),
                params: edit.record.params.clone(),
                kind: kind.into(),
                checkpoint,
            });
        }
        let end = steps.last().unwrap().checkpoint.asset;
        let artifact =
            ReplayArtifact { schema_version: 1, replay: ReplayId::new(&format!("{:032x}", 111)).unwrap(), project: project(900), packs: Vec::new(), steps };
        let mut bundle = GalleryBundle {
            manifest: manifest(GalleryContent::Project { checkpoint: end, replay: Some(Box::new(artifact)), direct_assets: Vec::new() }),
            objects,
        };
        bundle.validate().unwrap();
        for (index, edit) in captured.iter().enumerate() {
            let expected = render_document(&edit.document).unwrap();
            assert_eq!(replay_step(&bundle, 0, 0, index).unwrap(), expected, "depth {depth}, edit {}", edit.record.command);
            assert_eq!(replay_image(&bundle, 0, 0, index as u64 * 100).unwrap(), expected);
        }
        assert_eq!(replay_image(&bundle, 0, 0, u64::MAX).unwrap(), render_document(&session.active().unwrap().doc).unwrap());
        let info = replay_info(&bundle, 0, 0).unwrap();
        assert_eq!(info.moments.len(), captured.len());
        assert!(!info.baseline_only);
        assert!(replay_step(&bundle, 0, 0, usize::MAX).is_err());
        // The same intermediate native checkpoints work from a bounded pack.
        if let GalleryContent::Project { replay: Some(replay), .. } = &mut bundle.manifest.content {
            let baseline = replay.steps.first().unwrap().checkpoint.asset.hash;
            let packed: Vec<_> = bundle
                .objects
                .iter()
                .filter(|(hash, _)| **hash != baseline && **hash != end.hash)
                .map(|(hash, bytes)| (photocraft_party::AssetRef { hash: *hash, bytes: bytes.len() as u64 }, bytes.clone()))
                .collect();
            for (asset, _) in &packed {
                bundle.objects.remove(&asset.hash);
            }
            replay.packs.push(add(&mut bundle.objects, pack_checkpoints(&packed).unwrap()));
        }
        bundle.validate().unwrap();
        for (index, edit) in captured.iter().enumerate() {
            assert_eq!(replay_step(&bundle, 0, 0, index).unwrap(), render_document(&edit.document).unwrap());
        }
    }
}

#[test]
fn legacy_import_is_an_honest_baseline_and_bad_assets_fail() {
    let doc = photocraft_doc::Document::with_background(
        "Legacy",
        photocraft_doc::Size { width: 8, height: 8 },
        photocraft_doc::ColorMode::Rgb,
        photocraft_doc::SampleType::U16,
        photocraft_doc::Color::WHITE,
    );
    let mut bundle = local(&doc);
    assert!(replay_info(&bundle, 0, 0).unwrap().baseline_only);
    assert_eq!(replay_image(&bundle, 0, 0, 999).unwrap(), render_document(&doc).unwrap());
    assert!(replay_step(&bundle, 0, 0, 1).is_err());
    assert!(replay_info(&bundle, 1, 0).is_err());
    bundle.objects.values_mut().next().unwrap()[0] ^= 1;
    assert!(render_frame(&bundle, 0, 0).is_err());
}

#[test]
fn gallery_color_conversion_is_canonical_and_native_remix_keeps_profile_and_depth() {
    for depth in [photocraft_doc::SampleType::U8, photocraft_doc::SampleType::U16, photocraft_doc::SampleType::F32] {
        let mut doc = photocraft_doc::Document::with_background(
            "Linear",
            photocraft_doc::Size { width: 8, height: 8 },
            photocraft_doc::ColorMode::Rgb,
            depth,
            photocraft_doc::Color::rgb(0.5, 0.5, 0.5),
        );
        doc.icc_profile = Some(photocraft_cms::Builtin::LinearSrgb.profile().to_bytes());
        let bundle = local(&doc);
        assert_eq!(&render_frame(&bundle, 0, 0).unwrap().rgba[..4], &[188, 188, 188, 255]);
        let (copy, _) = remix(&bundle, 0, None).unwrap();
        assert_eq!(copy.depth, depth);
        assert_eq!(copy.icc_profile, doc.icc_profile);
        assert_eq!(render_document(&copy).unwrap(), render_document(&doc).unwrap());
    }
}
