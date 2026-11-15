#![cfg(not(target_arch = "wasm32"))]
mod k4_media_support;
use k4_media_support::{game, local};
use photocraft_doc::{ColorMode, Document, Layer, LayerContent, SampleType, Size, Timeline, VideoData};
use photocraft_engine::gallery_media::{
    AnimationOptions, encode_png, export_gif, export_png, export_sequence, gif_bytes, gif_bytes_with, gif_preview, remix, render_creature, render_document,
    render_frame, thumbnail, validate_originals,
};
use photocraft_raster::Surface;

fn animation() -> Document {
    let mut doc = Document::new("Small animation", Size { width: 12, height: 8 }, ColorMode::Rgb, SampleType::U8);
    let mut frames = Vec::new();
    for color in [[1., 0., 0., 1.], [0., 1., 0., 0.49], [0., 0., 1., 0.51]] {
        let mut surface = Surface::new(doc.pixel_format());
        surface.try_write_region(doc.bounds(), &color.repeat(12 * 8)).unwrap();
        frames.push(surface);
    }
    let mut layer = Layer::raster("Frames", doc.pixel_format());
    layer.content = LayerContent::Raster(frames[0].clone());
    layer.video = Some(VideoData::new(frames, 6.));
    doc.layers.push(layer);
    doc.timeline = Some(Timeline::new(3, 6.));
    doc
}

#[test]
fn gif_frame_order_timing_loop_and_binary_alpha_match_preview_contract() {
    let bundle = local(&animation());
    let original = bundle.objects.clone();
    for fps in [1, 6, 24] {
        for looping in [false, true] {
            let bytes = gif_bytes(&bundle, 0, AnimationOptions { fps, looping }).unwrap();
            assert_eq!(bytes.windows(11).any(|w| w == b"NETSCAPE2.0"), looping);
            let decoded = photocraft_codecs::decode_gif_frames(&bytes, &Default::default()).unwrap();
            assert_eq!(decoded.len(), 3);
            assert!(decoded.iter().all(|frame| frame.image.warnings.is_empty()));
            assert_eq!(gif_preview(&bytes, 1).unwrap().rgba, decoded[1].image.data());
            assert_eq!(&decoded[0].image.data()[..4], &[255, 0, 0, 255]);
            assert_eq!(decoded[1].image.data()[3], 0, "a transparent frame must not inherit preceding opaque pixels");
            assert_eq!(&decoded[2].image.data()[..4], &[0, 0, 255, 255]);
            for frame in decoded {
                assert!((f64::from(frame.delay_ms) - 1000.0 / f64::from(fps)).abs() <= 5.01);
            }
        }
    }
    assert_eq!(bundle.objects, original, "GIF quantization never changes native pixels");
    assert!(gif_bytes(&bundle, 0, AnimationOptions { fps: 0, looping: true }).is_err());
    assert!(gif_bytes(&bundle, 0, AnimationOptions { fps: 25, looping: true }).is_err());
    assert!(gif_bytes_with(&bundle, 0, AnimationOptions { fps: 6, looping: true }, |index, _| index < 2).is_err());
}

#[test]
fn gif_palette_is_bounded_while_lossless_original_and_png_keep_all_colors() {
    let mut doc = Document::new("Many colors", Size { width: 32, height: 16 }, ColorMode::Rgb, SampleType::U8);
    let mut layer = Layer::raster("Colors", doc.pixel_format());
    let samples: Vec<f32> = (0..512).flat_map(|index| [f32::from((index % 256) as u8) / 255.0, f32::from((index / 256) as u8), 0.25, 1.0]).collect();
    layer.surface_mut().unwrap().try_write_region(doc.bounds(), &samples).unwrap();
    doc.layers.push(layer);
    let bundle = local(&doc);
    let expected = render_document(&doc).unwrap();
    let gif = gif_bytes(&bundle, 0, AnimationOptions { fps: 6, looping: false }).unwrap();
    let decoded = gif_preview(&gif, 0).unwrap();
    let colors: std::collections::BTreeSet<_> = decoded.rgba.chunks_exact(4).collect();
    assert!(colors.len() <= 255);
    assert_ne!(decoded.rgba, expected.rgba);
    assert_eq!(render_frame(&bundle, 0, 0).unwrap(), expected);
    let png = photocraft_codecs::decode(&encode_png(&expected).unwrap()).unwrap();
    assert_eq!(png.data(), expected.rgba);
}

#[test]
fn png_sequence_is_exact_atomic_on_failure_and_never_clobbers_existing_data() {
    let bundle = local(&animation());
    let temp = tempfile::tempdir().unwrap();
    let cancelled = temp.path().join("cancelled");
    assert!(export_sequence(&bundle, 0, &cancelled, |index, _| index < 2).is_err());
    assert!(!cancelled.exists());
    let out = temp.path().join("sequence");
    let paths = export_sequence(&bundle, 0, &out, |_, _| true).unwrap();
    assert_eq!(paths.len(), 3);
    for (index, path) in paths.iter().enumerate() {
        let image = image::load_from_memory_with_format(&std::fs::read(path).unwrap(), image::ImageFormat::Png).unwrap().into_rgba8();
        assert_eq!(image.into_raw(), render_frame(&bundle, 0, index as u8).unwrap().rgba);
    }
    let first = std::fs::read(&paths[0]).unwrap();
    assert!(export_sequence(&bundle, 0, &out, |_, _| true).is_err());
    assert_eq!(std::fs::read(&paths[0]).unwrap(), first);
    assert!(export_png(&bundle, 0, Some(1), &paths[0]).is_err());
    assert_eq!(std::fs::read(&paths[0]).unwrap(), first);
    assert!(export_gif(&bundle, 0, AnimationOptions { fps: 6, looping: true }, &temp.path().join("missing/out.gif")).is_err());
    assert_eq!(std::fs::read_dir(temp.path()).unwrap().count(), 1, "failed exports leave no staging directories");
}

#[test]
fn project_remix_applies_saved_playback_speed_without_losing_frames() {
    let mut bundle = local(&animation());
    bundle.manifest.fps = 19;
    let (remixed, _) = remix(&bundle, 0, None).unwrap();
    assert_eq!(remixed.timeline.as_ref().unwrap().fps, 19.0);
    assert_eq!(remixed.timeline.as_ref().unwrap().duration, 3);
    assert_eq!(remixed.layers[0].video.as_ref().unwrap().fps, 19.0);
    assert_eq!(remixed.layers[0].video.as_ref().unwrap().frames.len(), 3);
    assert_eq!(render_document(&remixed).unwrap(), render_frame(&bundle, 0, 0).unwrap());
}

#[test]
fn corpse_png_joins_exact_rows_and_omits_labels_and_reference_strips() {
    let mut bundle = game(photocraft_party::Mode::ExquisiteCorpse);
    if let photocraft_party::gallery::GalleryContent::Game { room } = &bundle.manifest.content
        && let photocraft_party::room::RevealedEntry::Artwork { assignment, .. } = &room.reveal[0]
    {
        bundle.manifest.notes.push((assignment.id, "A handoff note that must remain outside the image".into()));
    }
    let image = render_creature(&bundle, 0).unwrap();
    validate_originals(&bundle).unwrap();
    assert_eq!((image.width, image.height), (1024, 1536));
    for (row, expected) in
        [(0, [255, 0, 0, 255]), (511, [255, 0, 0, 255]), (512, [0, 255, 0, 255]), (1023, [0, 255, 0, 255]), (1024, [0, 0, 255, 255]), (1535, [0, 0, 255, 255])]
    {
        assert_eq!(&image.rgba[row * 1024 * 4..row * 1024 * 4 + 4], &expected);
    }
    let bytes = encode_png(&image).unwrap();
    let decoded = photocraft_codecs::decode(&bytes).unwrap();
    assert!(decoded.meta.text.is_empty());
    assert!(decoded.meta.xmp.is_none());
    assert_eq!(decoded.data(), image.rgba);
    assert_eq!(thumbnail(&bundle, 0, 256).unwrap().height, 256);
}
