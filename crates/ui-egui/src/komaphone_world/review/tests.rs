#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use scene::shared::media::{AssetHash, Credit, MediaFrame, MediaPlacement, MediaSource, Provenance};
use scene::shared::{AcceptedOp, DeviceId, OpId, SourceId, StrokeCopyTarget, WorldOp, stroke_copy};

fn fixture() -> WorldState {
    let canvas = CanvasId::new(&format!("{:032x}", 1)).unwrap();
    let owner = scene::OwnerId::new(&format!("{:032x}", 2)).unwrap();
    WorldState::new(canvas, owner, "Navigation fixture".into()).unwrap()
}
fn append(world: &mut WorldState, action: Edit) -> Change {
    let revision = world.revision + 1;
    let event = AcceptedOp {
        revision,
        op: WorldOp {
            id: OpId::new(&format!("{:032x}", revision + 100)).unwrap(),
            canvas: world.canvas,
            author: world.owner,
            device: DeviceId::new(&format!("{:032x}", 3)).unwrap(),
            sequence: revision,
            authority_epoch: 1,
            action,
        },
    };
    world.apply(&event).unwrap();
    world.change(&event).unwrap()
}

#[test]
fn deep_offset_remix_and_deletion_jump_to_immutable_media_center() {
    let mut world = fixture();
    let media = MediaPlacement {
        id: MediaId::new(&format!("{:032x}", 4)).unwrap(),
        canvas: world.canvas,
        owner: world.owner,
        anchor: scene::RegionKey { depth: 6, ..Default::default() },
        offset: [-4000., 3000.],
        width: 400.,
        height: 200.,
        frames: vec![MediaFrame { hash: AssetHash::new(&"a".repeat(64)).unwrap(), byte_len: 1, width: 1, height: 1, duration_ms: 100 }],
        looping: false,
        provenance: Provenance {
            source: MediaSource::Gallery { entry: SourceId::new(&format!("{:032x}", 5)).unwrap(), room: None, game: None, chain: 0, frame: None },
            source_revision: 1,
            artifact_hash: AssetHash::new(&"b".repeat(64)).unwrap(),
            authors: vec![Credit { profile: world.owner, label: "Artist".into() }],
        },
    };
    let added = append(&mut world, Edit::PlaceMedia { media: Box::new(media.clone()) });
    let hidden = append(&mut world, Edit::SetMediaVisible { media: media.id, visible: false });
    let hash = world.canonical_hash().unwrap();
    for change in [added, hidden] {
        let jump = target(&world, &change).unwrap().unwrap();
        assert_eq!(jump.media, Some(media.id));
        assert_eq!(jump.stroke, None);
        let expected = scene::coordinates::point(&media.anchor, [-3800., 3100.]).unwrap();
        assert_eq!(scene::coordinates::center(&jump.camera).unwrap(), expected);
        let viewport = scene::Viewport { camera: jump.camera, width: 800, height: 600 };
        let projected = scene::coordinates::point_to_screen(&viewport, expected).unwrap();
        assert!((projected[0] - 400.).abs() < 1e-6 && (projected[1] - 300.).abs() < 1e-6);
    }
    assert_eq!(world.canonical_hash().unwrap(), hash);
}

#[test]
fn copied_stroke_navigation_and_hidden_history_use_exact_retained_bounds() {
    let mut world = fixture();
    let stroke = scene::Stroke {
        id: StrokeId::new(&format!("{:032x}", 6)).unwrap(),
        canvas: world.canvas,
        owner: world.owner,
        revision: 0,
        anchor: Default::default(),
        points: vec![[0., 0.], [10., 20.]],
        width: 2.,
        color: scene::Srgba { rgba: [0., 0., 0., 1.] },
        erase: false,
        brush: None,
        samples: None,
    };
    append(&mut world, Edit::AddStroke { stroke: stroke.clone() });
    let copied_id = StrokeId::new(&format!("{:032x}", 7)).unwrap();
    let copied = stroke_copy(
        &world.strokes[&stroke.id],
        world.canvas,
        Credit { profile: world.owner, label: "Artist".into() },
        StrokeCopyTarget {
            canvas: world.canvas,
            id: copied_id,
            owner: world.owner,
            anchor: scene::RegionKey { depth: 6, ..Default::default() },
            offset: [3000., -3000.],
            scale: 1.,
        },
    )
    .unwrap();
    let added = append(&mut world, copied);
    let hidden = append(&mut world, Edit::SetStrokeVisible { stroke: copied_id, visible: false });
    let expected = scene::coordinates::point(&world.strokes[&copied_id].stroke.anchor, [3005., -2990.]).unwrap();
    for change in [added.clone(), hidden] {
        let jump = target(&world, &change).unwrap().unwrap();
        assert_eq!(jump.stroke, Some(copied_id));
        assert_eq!(jump.media, None);
        assert_eq!(scene::coordinates::center(&jump.camera).unwrap(), expected);
    }
    let mut invalid = added;
    invalid.author = scene::OwnerId::new(&format!("{:032x}", 99)).unwrap();
    assert!(target(&world, &invalid).is_err());
}
