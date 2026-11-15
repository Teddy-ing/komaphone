#![cfg(not(target_arch = "wasm32"))]
mod k4_media_support;
use k4_media_support::{game, local, participant};
use photocraft_engine::gallery_media::{remix, render_creature, render_document, render_frame, replay_info};
use photocraft_party::Mode;
use photocraft_party::gallery::GalleryContent;
use photocraft_party::room::{Phase, RevealedEntry};

#[test]
fn animation_remix_keeps_every_frame_timing_and_source_credits_with_new_ids() {
    let mut bundle = game(Mode::Animation { rotations: 2 });
    bundle.manifest.fps = 11;
    bundle.manifest.looping = false;
    let original = bundle.objects.clone();
    let (mut doc, source) = remix(&bundle, 0, None).unwrap();
    assert_eq!(doc.timeline.as_ref().unwrap().duration, 4);
    assert_eq!(doc.timeline.as_ref().unwrap().fps, 11.);
    assert_eq!(source.source_entry, bundle.manifest.id);
    assert_eq!(source.source_revision, 50);
    assert_eq!(source.frame, None);
    assert_eq!(source.authors, bundle.manifest.authors);
    for index in 0..4u8 {
        let local = local(&doc);
        assert_eq!(render_frame(&local, 0, index).unwrap(), render_frame(&bundle, 0, index).unwrap());
    }
    let (still, still_source) = remix(&bundle, 0, Some(1)).unwrap();
    assert!(still.timeline.is_none());
    assert_eq!(still_source.authors, vec![participant(2)]);
    assert_eq!(render_document(&still).unwrap(), render_frame(&bundle, 0, 1).unwrap());
    let source_doc = if let GalleryContent::Game { room } = &bundle.manifest.content {
        match &room.reveal[1] {
            RevealedEntry::Artwork { checkpoint, .. } => photocraft_format::load_from_bytes(bundle.object(checkpoint.asset).unwrap()).unwrap(),
            _ => panic!("fixture artwork"),
        }
    } else {
        panic!("fixture game")
    };
    assert_ne!(source_doc.id, still.id);
    assert_ne!(source_doc.layers[0].id, still.layers[0].id);
    doc.layers[0].surface_mut().unwrap().try_write_region(photocraft_geom::Rect::new(0, 0, 1, 1), &[0., 0., 0., 1.]).unwrap();
    assert_eq!(bundle.objects, original);
    drop(doc);
    assert_eq!(render_frame(&bundle, 0, 0).unwrap().rgba[..4], [255, 0, 0, 255]);
    let bytes = photocraft_format::save_to_bytes(&still, &Default::default()).unwrap();
    let restored = photocraft_format::load_from_bytes(&bytes).unwrap();
    assert_eq!(restored.metadata.text, still.metadata.text);
    assert_eq!(restored.metadata.xmp, still.metadata.xmp);
    assert!(restored.metadata.xmp.as_ref().unwrap().contains(bundle.manifest.id.as_str()));
}

#[test]
fn creature_remix_parts_are_editable_independent_and_survive_source_drop() {
    let bundle = game(Mode::ExquisiteCorpse);
    let expected = render_creature(&bundle, 0).unwrap();
    let (mut doc, source) = remix(&bundle, 0, None).unwrap();
    assert_eq!(doc.layers.len(), 3);
    assert_eq!(render_document(&doc).unwrap(), expected);
    assert_eq!(source.authors.len(), 2);
    let source_entry = source.source_entry;
    drop(bundle);
    doc.layers[0].visible = false;
    let changed = render_document(&doc).unwrap();
    assert_eq!(changed.rgba[3], 0);
    assert_eq!(changed.rgba[512 * 1024 * 4 + 3], 255);
    let bytes = photocraft_format::save_to_bytes(&doc, &Default::default()).unwrap();
    let reopened = photocraft_format::load_from_bytes(&bytes).unwrap();
    assert_eq!(render_document(&reopened).unwrap(), changed);
    assert!(reopened.metadata.xmp.as_ref().unwrap().contains(source_entry.as_str()));
}

#[test]
fn foreign_native_id_range_is_reserved_before_remix_allocation() {
    let mut source = photocraft_doc::Document::with_background(
        "Foreign",
        photocraft_doc::Size { width: 8, height: 8 },
        photocraft_doc::ColorMode::Rgb,
        photocraft_doc::SampleType::U8,
        photocraft_doc::Color::WHITE,
    );
    let foreign_id = photocraft_doc::DocId::fresh().0 + 10_000_000;
    source.id = photocraft_doc::DocId(foreign_id);
    source.layers[0].id = photocraft_doc::LayerId(foreign_id + 1);
    source.layers[0].psd_id = Some(15);
    let bundle = local(&source);
    let (copy, _) = remix(&bundle, 0, None).unwrap();
    assert!(copy.id.0 > foreign_id + 1);
    assert!(copy.layers[0].id.0 > foreign_id + 1);
    assert_eq!(copy.layers[0].psd_id, None);
    assert_eq!(render_document(&copy).unwrap(), render_document(&source).unwrap());
}

#[test]
fn hidden_missing_and_invalid_sources_do_not_become_remixes() {
    let mut bundle = game(Mode::Animation { rotations: 1 });
    assert!(remix(&bundle, 3, None).is_err());
    assert!(remix(&bundle, 0, Some(12)).is_err());
    if let GalleryContent::Game { room } = &mut bundle.manifest.content {
        room.phase = Phase::Drawing;
    }
    assert!(remix(&bundle, 0, None).is_err());
    assert!(render_frame(&bundle, 0, 0).is_err());
    assert!(replay_info(&bundle, 0, 0).is_err());

    if let GalleryContent::Game { room } = &mut bundle.manifest.content {
        room.phase = Phase::Archived;
        let assignment = match &room.reveal[0] {
            RevealedEntry::Artwork { assignment, .. } => assignment.clone(),
            _ => panic!("fixture artwork"),
        };
        room.reveal[0] = RevealedEntry::Missing { assignment, reason: "Skipped by host".into() };
    }
    let required = bundle.manifest.required_assets().unwrap();
    bundle.objects.retain(|hash, _| required.iter().any(|asset| asset.hash == *hash));
    bundle.validate().unwrap();
    assert!(replay_info(&bundle, 0, 0).unwrap().missing);
    assert!(render_frame(&bundle, 0, 0).unwrap().rgba.iter().all(|v| *v == 0));
    assert!(remix(&bundle, 0, Some(0)).is_err());
    let (copy, _) = remix(&bundle, 0, None).unwrap();
    assert!(copy.metadata.text.iter().any(|(key, value)| key == "Komaphone missing submissions" && value.contains("Skipped by host")));
}
