use photocraft_scene::{
    CanvasId, MAX_SPATIAL_CELLS, OwnerId, RegionKey, Srgba, Stroke, StrokeId, Wide,
    brush::{BrushBlendMode, BrushDynamic, BrushScattering, BrushSmoothing, BrushTip, ProceduralTipStyle, STROKE_BRUSH_SCHEMA, StrokeBrush, StrokeSample},
    render::{decimate, spatial_cells},
    shared::{
        AcceptedOp, DeviceId, Edit, OpId, SHARED_SCHEMA, SHARED_SCHEMA_BRUSH, StrokeCopyTarget, WorldOp, WorldState, archive_import::ArchiveManifest,
        media::Credit,
    },
    world_protocol::{Action, PreviewStyle, Request, VERSION},
};

fn hex(n: u128) -> String {
    format!("{n:032x}")
}
fn canvas(n: u128) -> CanvasId {
    CanvasId::new(&hex(n)).unwrap()
}
fn owner(n: u128) -> OwnerId {
    OwnerId::new(&hex(n)).unwrap()
}
fn device(n: u128) -> DeviceId {
    DeviceId::new(&hex(n)).unwrap()
}
fn stroke_id(n: u128) -> StrokeId {
    StrokeId::new(&hex(n)).unwrap()
}
fn op_id(n: u128) -> OpId {
    OpId::new(&hex(n)).unwrap()
}

fn legacy_stroke(n: u128, c: CanvasId, o: OwnerId) -> Stroke {
    Stroke {
        id: stroke_id(n),
        canvas: c,
        owner: o,
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[10.0, 10.0], [50.0, 20.0]],
        width: 8.0,
        color: Srgba { rgba: [0.2, 0.4, 0.8, 1.0] },
        erase: false,
        brush: None,
        samples: None,
    }
}

fn brush_stroke(n: u128, c: CanvasId, o: OwnerId, brush: StrokeBrush, samples: Option<Vec<StrokeSample>>) -> Stroke {
    Stroke {
        id: stroke_id(n),
        canvas: c,
        owner: o,
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[10.0, 10.0], [50.0, 20.0]],
        width: 24.0,
        color: Srgba { rgba: [0.9, 0.2, 0.1, 1.0] },
        erase: false,
        brush: Some(Box::new(brush)),
        samples,
    }
}

#[test]
fn legacy_stroke_json_is_unchanged_and_deserializes_from_v1_json() {
    let s = legacy_stroke(1, canvas(2), owner(3));
    let json = serde_json::to_string(&s).unwrap();
    assert!(!json.contains("brush"), "{json}");
    assert!(!json.contains("samples"), "{json}");
    let back: Stroke = serde_json::from_str(&json).unwrap();
    assert_eq!(back, s);
    assert!(!back.uses_brush_v2());
}

#[test]
fn world_schema_promotes_on_brush_v2_and_preserves_imported_v1_prefix_hash() {
    let c1 = canvas(10);
    let o1 = owner(20);
    let d1 = device(30);
    let mut source = WorldState::new(c1, o1, "Legacy world".into()).unwrap();
    let op1 = AcceptedOp {
        revision: 1,
        op: WorldOp {
            id: op_id(1),
            canvas: c1,
            author: o1,
            device: d1,
            sequence: 1,
            authority_epoch: 1,
            action: Edit::AddStroke { stroke: legacy_stroke(100, c1, o1) },
        },
    };
    source.apply(&op1).unwrap();
    assert_eq!(source.schema, SHARED_SCHEMA);
    let v1_manifest = ArchiveManifest { schema: 1, world: source.clone(), assets: vec![] };
    v1_manifest.validate().unwrap();
    assert!(ArchiveManifest { schema: 2, world: source.clone(), assets: vec![] }.validate().is_err());

    let c2 = canvas(11);
    let o2 = owner(21);
    let d2 = device(31);
    let mut imported = source.import_to(c2, o2).unwrap();
    assert_eq!(imported.schema, SHARED_SCHEMA);
    let original_prefix_hash = imported.import_origin.as_ref().unwrap().prefix_hash;

    let soft = StrokeBrush { authored_size: 40.0, hardness: 0.0, opacity: 0.5, flow: 0.35, ..Default::default() };
    let op2 = AcceptedOp {
        revision: 2,
        op: WorldOp {
            id: op_id(2),
            canvas: c2,
            author: o2,
            device: d2,
            sequence: 1,
            authority_epoch: 1,
            action: Edit::AddStroke { stroke: brush_stroke(101, c2, o2, soft, None) },
        },
    };
    imported.apply(&op2).unwrap();
    assert_eq!(imported.schema, SHARED_SCHEMA_BRUSH);
    imported.validate().unwrap();
    assert_eq!(imported.import_origin.as_ref().unwrap().prefix_hash, original_prefix_hash);
    assert_eq!(imported.replay_to(1).unwrap().schema, SHARED_SCHEMA);

    // Schema 2 archives succeed for v2 worlds; schema 1 archives are rejected for v2 worlds.
    assert!(ArchiveManifest { schema: 1, world: imported.clone(), assets: vec![] }.validate().is_err());
    let v2_manifest = ArchiveManifest { schema: 2, world: imported.clone(), assets: vec![] };
    let encoded = serde_json::to_vec(&v2_manifest).unwrap();
    let decoded = ArchiveManifest::decode(&encoded).unwrap();
    assert_eq!(decoded.schema, 2);
    assert_eq!(decoded.world.schema, SHARED_SCHEMA_BRUSH);
    assert_eq!(decoded.world, imported);
}

#[test]
fn spatial_cells_expand_for_soft_halo_and_scattering() {
    // Point sits 14 local units from the right edge of region 0 (x = 1024).
    // With width = 20 (nominal radius = 10):
    // - hard legacy stroke stays inside cell 0 (1010 + 10 = 1020 < 1024)
    // - soft round stroke (hardness 0, reach ~1.83 * 10 = 18.3) crosses into cell 1!
    // - scattered stroke (scatter 1.5, both_axes) crosses even further into cell 1!
    let c = canvas(40);
    let o = owner(41);
    let hard = Stroke {
        id: stroke_id(42),
        canvas: c,
        owner: o,
        revision: 0,
        anchor: RegionKey::default(),
        points: vec![[1010.0, 500.0]],
        width: 20.0,
        color: Srgba { rgba: [0.0, 0.0, 0.0, 1.0] },
        erase: false,
        brush: None,
        samples: None,
    };
    assert_eq!(spatial_cells(&hard, MAX_SPATIAL_CELLS).unwrap().len(), 1);

    let mut soft = hard.clone();
    soft.brush = Some(Box::new(StrokeBrush { authored_size: 20.0, hardness: 0.0, ..Default::default() }));
    let soft_cells = spatial_cells(&soft, MAX_SPATIAL_CELLS).unwrap();
    assert_eq!(soft_cells.len(), 2);
    assert_eq!(soft_cells[0].x, Wide(0));
    assert_eq!(soft_cells[1].x, Wide(1));

    let mut scattered = hard;
    scattered.points = vec![[990.0, 500.0]];
    scattered.brush = Some(Box::new(StrokeBrush {
        authored_size: 20.0,
        tip: BrushTip::Procedural { style: ProceduralTipStyle::Spatter, size: 64, seed: 11, param: 14 },
        scattering: Some(BrushScattering {
            scatter: BrushDynamic { jitter: 2.5, ..Default::default() },
            both_axes: true,
            count: 2,
            count_jitter: BrushDynamic::default(),
        }),
        ..Default::default()
    }));
    let scattered_cells = spatial_cells(&scattered, MAX_SPATIAL_CELLS).unwrap();
    assert_eq!(scattered_cells.len(), 2);
}

#[test]
fn decimate_and_stroke_copy_preserve_brush_and_samples() {
    let c = canvas(50);
    let o = owner(51);
    let d = device(52);
    let brush = StrokeBrush {
        authored_size: 32.0,
        hardness: 0.25,
        opacity: 0.6,
        flow: 0.2,
        build_up: true,
        build_up_rate: 25.0,
        mode: BrushBlendMode::Multiply,
        smoothing: Some(BrushSmoothing { amount: 0.4, pulled_string: false, catch_up: true, catch_up_on_end: true, adjust_for_zoom: true, zoom: 1.0 }),
        seed: 98765,
        ..Default::default()
    };
    let points: Vec<[f64; 2]> = (0..64).map(|i| [i as f64 * 2.0, (i as f64 * 0.5).sin() * 10.0]).collect();
    let samples: Vec<StrokeSample> =
        (0..64).map(|i| StrokeSample { time_ms: i as f64 * 16.0, pressure: 0.5 + (i as f32 / 128.0), ..Default::default() }).collect();
    let s = Stroke {
        id: stroke_id(53),
        canvas: c,
        owner: o,
        revision: 0,
        anchor: RegionKey::default(),
        points,
        width: 32.0,
        color: Srgba { rgba: [0.1, 0.5, 0.9, 1.0] },
        erase: false,
        brush: Some(Box::new(brush.clone())),
        samples: Some(samples.clone()),
    };
    let summary = decimate(&s, 12).unwrap();
    assert_eq!(summary.points.len(), 12);
    assert_eq!(summary.samples.as_ref().unwrap().len(), 12);
    assert_eq!(summary.samples.as_ref().unwrap().first(), samples.first());
    assert_eq!(summary.samples.as_ref().unwrap().last(), samples.last());
    assert_eq!(summary.brush.as_deref(), Some(&brush));
    summary.validate().unwrap();

    let mut world = WorldState::new(c, o, "Copy test".into()).unwrap();
    world
        .apply(&AcceptedOp {
            revision: 1,
            op: WorldOp { id: op_id(54), canvas: c, author: o, device: d, sequence: 1, authority_epoch: 1, action: Edit::AddStroke { stroke: s } },
        })
        .unwrap();
    let copy_edit = world
        .copy_stroke(
            stroke_id(53),
            Credit { profile: o, label: "Alice".into() },
            StrokeCopyTarget { canvas: c, id: stroke_id(55), owner: o, anchor: RegionKey::default(), offset: [100.0, 50.0], scale: 0.5 },
        )
        .unwrap();
    let Edit::CopyStroke { stroke: copied, .. } = copy_edit else { panic!("expected CopyStroke") };
    assert_eq!(copied.brush.as_deref(), Some(&brush));
    assert_eq!(copied.samples, Some(samples));
    assert_eq!(copied.width, 16.0);
}

#[test]
fn adversarial_brush_inputs_and_old_protocol_version_are_rejected() {
    let c = canvas(60);
    let o = owner(61);
    let valid_base = brush_stroke(62, c, o, StrokeBrush::default(), None);
    valid_base.validate().unwrap();

    // Non-opaque legacy stroke still rejected without brush.
    let mut bad_legacy = legacy_stroke(63, c, o);
    bad_legacy.color.rgba[3] = 0.5;
    assert!(bad_legacy.validate().is_err());

    // Samples without brush rejected.
    let mut orphan_samples = legacy_stroke(64, c, o);
    orphan_samples.samples = Some(vec![StrokeSample::default(); 2]);
    assert!(orphan_samples.validate().is_err());

    // Mismatched sample count and backwards timestamps rejected.
    let mut bad_len = valid_base.clone();
    bad_len.samples = Some(vec![StrokeSample::default()]);
    assert!(bad_len.validate().is_err());

    let mut bad_time = valid_base.clone();
    bad_time.samples = Some(vec![StrokeSample { time_ms: 20.0, ..Default::default() }, StrokeSample { time_ms: 10.0, ..Default::default() }]);
    assert!(bad_time.validate().is_err());

    // Erase + behind rejected.
    let mut erase_behind = valid_base.clone();
    erase_behind.erase = true;
    erase_behind.brush.as_mut().unwrap().behind = true;
    assert!(erase_behind.validate().is_err());

    // Bad schema, NaN opacity, invalid PNG header rejected.
    let mut bad_schema = valid_base.clone();
    bad_schema.brush.as_mut().unwrap().schema = STROKE_BRUSH_SCHEMA + 1;
    assert!(bad_schema.validate().is_err());

    let mut nan_opacity = valid_base.clone();
    nan_opacity.brush.as_mut().unwrap().opacity = f32::NAN;
    assert!(nan_opacity.validate().is_err());

    let mut bad_png = valid_base;
    bad_png.brush.as_mut().unwrap().tip = BrushTip::Sampled { width: 16, height: 16, png_b64: "AAAA".repeat(8) };
    assert!(bad_png.validate().is_err());

    // Wire protocol version bumped from 3 -> 4 (rejects both v2 and v3).
    assert_eq!(VERSION, 4);
    for old in [2u16, 3] {
        let old_req = serde_json::to_vec(&Request { version: old, action: Action::Identity {} }).unwrap();
        assert!(Request::decode(&old_req).is_err());
    }
    let new_req = serde_json::to_vec(&Request { version: VERSION, action: Action::Identity {} }).unwrap();
    assert!(Request::decode(&new_req).is_ok());

    // PreviewStyle round-trips with brush & samples.
    let preview = PreviewStyle {
        id: stroke_id(65),
        anchor: RegionKey::default(),
        width: 12.0,
        color: Srgba { rgba: [0.0, 0.0, 0.0, 1.0] },
        erase: false,
        brush: Some(Box::new(StrokeBrush { opacity: 0.4, ..Default::default() })),
        samples: Some(vec![StrokeSample { time_ms: 16.0, ..Default::default() }]),
    };
    let roundtrip: PreviewStyle = serde_json::from_slice(&serde_json::to_vec(&preview).unwrap()).unwrap();
    assert_eq!(roundtrip, preview);
}

#[test]
fn checkpoint_bounded_zlib_zipbomb_hash_and_archive_v3_roundtrip() {
    use photocraft_scene::shared::WorldCheckpoint;
    use photocraft_scene::world_protocol::MAX_CHECKPOINT_UNCOMPRESSED_BYTES;
    use std::io::Write;

    let mut world = WorldState::new(canvas(90), owner(91), "Checkpoint v4".into()).unwrap();
    for rev in 1..=25u64 {
        let mut s = brush_stroke(rev as u128 + 100, canvas(90), owner(91), StrokeBrush::default(), None);
        s.points = vec![[rev as f64, 0.0], [rev as f64 + 10.0, 5.0]];
        world
            .apply(&AcceptedOp {
                revision: rev,
                op: WorldOp {
                    id: op_id(900 + rev as u128),
                    canvas: canvas(90),
                    author: owner(91),
                    device: device(92),
                    sequence: rev,
                    authority_epoch: 1,
                    action: Edit::AddStroke { stroke: s },
                },
            })
            .unwrap();
    }

    let ckpt = world.to_checkpoint();
    let (compressed, comp_sha, state_sha, uncomp_len) = ckpt.encode_zlib().unwrap();
    let decoded = WorldCheckpoint::decode_zlib_bounded(&compressed, &comp_sha, uncomp_len, &state_sha, MAX_CHECKPOINT_UNCOMPRESSED_BYTES).unwrap();
    assert_eq!(decoded, ckpt);
    let from_ckpt = WorldState::from_checkpoint(decoded).unwrap();
    assert_eq!(from_ckpt.revision, world.revision);
    assert_eq!(from_ckpt.strokes, world.strokes);
    assert_eq!(from_ckpt.base_revision(), world.revision);
    assert!(from_ckpt.validate().is_ok());

    // Hash mismatch rejected.
    let mut tampered = compressed.clone();
    tampered[0] ^= 0xFF;
    assert!(WorldCheckpoint::decode_zlib_bounded(&tampered, &comp_sha, uncomp_len, &state_sha, MAX_CHECKPOINT_UNCOMPRESSED_BYTES).is_err());

    // Streaming decompression cap rejects oversized / zip-bomb payloads cleanly before full allocation.
    let mut bomb_enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    bomb_enc.write_all(&vec![b'A'; 128 * 1024]).unwrap();
    let bomb = bomb_enc.finish().unwrap();
    let bomb_sha = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&bomb));
    assert!(WorldCheckpoint::decode_zlib_bounded(&bomb, &bomb_sha, 64 * 1024, &state_sha, 64 * 1024).is_err());

    // `.komworld` v3 archive encodes events ONCE (zlib-compressed) and decodes bit-identically.
    let manifest = ArchiveManifest { schema: 2, world: world.clone(), assets: vec![] };
    let v3_bytes = manifest.encode_v3().unwrap();
    let decoded_manifest = ArchiveManifest::decode(&v3_bytes).unwrap();
    assert_eq!(decoded_manifest.world, world);
}
