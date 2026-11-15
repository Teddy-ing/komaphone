use super::*;
use photocraft_scene::shared::media::Credit;
use photocraft_scene::shared::{StrokeCopyTarget, stroke_copy};

fn fixture() -> (tempfile::TempDir, Store, WorldState, WorldOp) {
    let directory = tempfile::tempdir().unwrap();
    let mut store = Store::open(directory.path().join("service.db"), StoreOptions::default(), 0).unwrap();
    for n in 1..=3 {
        pair(&mut store, n);
    }
    create(&mut store, 1, 100);
    create(&mut store, 2, 200);
    invite(&mut store, 1, 2, 100, 700);
    call(&mut store, 1, Action::Append { operation: op(1, 100, 1, 301) });
    let mut source = WorldState::new(canvas(100), owner(1), "Canvas 100".into()).unwrap();
    for event in events(&mut store, 2, 100) {
        source.apply(&event).unwrap();
    }
    let action = stroke_copy(
        &source.strokes[&StrokeId::new(&hex(301)).unwrap()],
        canvas(100),
        Credit { profile: owner(1), label: person(1).display_name },
        StrokeCopyTarget {
            canvas: canvas(200),
            id: StrokeId::new(&hex(302)).unwrap(),
            owner: owner(2),
            anchor: RegionKey { depth: 6, ..Default::default() },
            offset: [10., 20.],
            scale: 2.,
        },
    )
    .unwrap();
    let mut copied = op(2, 200, 1, 302);
    copied.action = action;
    (directory, store, source, copied)
}
fn denied(store: &mut Store, actor: u32, action: Action) {
    assert!(store.world_execute(&token(actor), &request(action), 100).is_err());
}

#[test]
fn copied_stroke_is_independent_owned_art_with_immutable_original_credit_and_authorized_retries() {
    let (_directory, mut store, source, copied) = fixture();
    denied(&mut store, 2, Action::Append { operation: copied.clone() });
    denied(&mut store, 3, Action::RemixStroke { operation: copied.clone() });
    call(&mut store, 2, Action::RemixStroke { operation: copied.clone() });
    call(&mut store, 2, Action::RemixStroke { operation: copied.clone() });
    let accepted = events(&mut store, 2, 200);
    assert_eq!(accepted.len(), 1);
    let mut destination = WorldState::new(canvas(200), owner(2), "Canvas 200".into()).unwrap();
    destination.apply(&accepted[0]).unwrap();
    let new = &destination.strokes[&StrokeId::new(&hex(302)).unwrap()];
    assert_eq!(new.stroke.points, [[12., 22.], [14., 24.]]);
    assert_eq!(new.stroke.width, 6.);
    assert_eq!(new.stroke.owner, owner(2));
    assert_eq!(new.provenance.as_ref().unwrap().author.profile, owner(1));
    assert_eq!(new.provenance.as_ref().unwrap().source_revision, 1);
    let second = stroke_copy(
        new,
        canvas(200),
        Credit { profile: owner(2), label: person(2).display_name },
        StrokeCopyTarget {
            canvas: canvas(200),
            id: StrokeId::new(&hex(303)).unwrap(),
            owner: owner(2),
            anchor: Default::default(),
            offset: [0., 0.],
            scale: 1.,
        },
    )
    .unwrap();
    let mut next = op(2, 200, 2, 303);
    next.action = second;
    call(&mut store, 2, Action::RemixStroke { operation: next });
    let original_credit = match &events(&mut store, 2, 200)[1].op.action {
        Edit::CopyStroke { provenance, .. } => provenance.author.clone(),
        _ => panic!(),
    };
    assert_eq!(original_credit.profile, owner(1));
    let mut undo = op(2, 200, 3, 304);
    undo.action = Edit::SetStrokeVisible { stroke: new.stroke.id, visible: false };
    call(&mut store, 2, Action::Append { operation: undo });
    assert_eq!(events(&mut store, 1, 100), source.events_after(0, 128).unwrap());
    call(&mut store, 1, Action::Remove { canvas: canvas(100), member: owner(2) });
    denied(&mut store, 2, Action::RemixStroke { operation: copied }); // Receipt is not a current source grant.
    assert_eq!(events(&mut store, 2, 200).len(), 3);
}

#[test]
fn forged_copy_shape_credit_revision_and_interrupted_write_preserve_source_and_writer_sequence() {
    let (_directory, mut store, source, copied) = fixture();
    for damage in 0..4 {
        let mut forged = copied.clone();
        let Edit::CopyStroke { stroke, provenance } = &mut forged.action else { panic!() };
        match damage {
            0 => provenance.author.profile = owner(2),
            1 => provenance.source_revision = 2,
            2 => stroke.points[0][0] += 1.,
            _ => provenance.artifact_hash = photocraft_scene::shared::media::AssetHash::new(&"a".repeat(64)).unwrap(),
        }
        denied(&mut store, 2, Action::RemixStroke { operation: forged });
    }
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        store.world_execute_observed(&token(2), &request(Action::RemixStroke { operation: copied.clone() }), 100, Limits::default(), |stage| {
            if stage == SaveStage::BeforeCommit {
                panic!("synthetic loss");
            }
        })
    }));
    assert!(interrupted.is_err());
    assert!(events(&mut store, 2, 200).is_empty());
    assert_eq!(events(&mut store, 1, 100), source.events_after(0, 128).unwrap());
    call(&mut store, 2, Action::RemixStroke { operation: copied.clone() });
    let mut hidden = op(1, 100, 2, 305);
    hidden.action = Edit::SetStrokeVisible { stroke: StrokeId::new(&hex(301)).unwrap(), visible: false };
    call(&mut store, 1, Action::Append { operation: hidden });
    denied(&mut store, 2, Action::RemixStroke { operation: copied });
    assert_eq!(events(&mut store, 2, 200).len(), 1);
}

#[test]
fn hash_valid_source_checkpoint_corruption_cannot_authorize_substituted_strokes_or_ownership() {
    use sha2::{Digest, Sha256};
    for corrupt_owner in [false, true] {
        let (directory, mut store, mut source, mut copied) = fixture();
        if corrupt_owner {
            source.owner = owner(2);
        } else {
            source.strokes.get_mut(&StrokeId::new(&hex(301)).unwrap()).unwrap().stroke.points[0][0] += 100.;
        }
        let bytes = serde_json::to_vec(&source).unwrap();
        let hash = format!("{:x}", Sha256::digest(&bytes));
        let db = rusqlite::Connection::open(directory.path().join("service.db")).unwrap();
        db.execute("UPDATE worlds SET state=?1,hash=?2 WHERE id=?3", rusqlite::params![bytes, hash, canvas(100).as_str()]).unwrap();
        copied.action = stroke_copy(
            &source.strokes[&StrokeId::new(&hex(301)).unwrap()],
            canvas(100),
            Credit { profile: owner(1), label: person(1).display_name },
            StrokeCopyTarget {
                canvas: canvas(200),
                id: StrokeId::new(&hex(302)).unwrap(),
                owner: owner(2),
                anchor: RegionKey { depth: 6, ..Default::default() },
                offset: [10., 20.],
                scale: 2.,
            },
        )
        .unwrap();
        denied(&mut store, 2, Action::RemixStroke { operation: copied });
        assert!(events(&mut store, 2, 200).is_empty());
    }
}
