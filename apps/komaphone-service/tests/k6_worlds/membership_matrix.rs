//! Generated authorization coverage: display names and group combinations never define scope.
use super::{Action, CanvasId, OpId, OwnerId, Response, Store, StoreOptions, call, canvas, coordinates, events, hex, invite, op, owner, pair, request, token};
use std::collections::BTreeSet;

const LABEL: &str = "Same display name";

struct Case {
    id: u32,
    owner: u32,
    members: BTreeSet<u32>,
}
fn directory(store: &mut Store, actor: u32) -> BTreeSet<CanvasId> {
    let mut after = None;
    let mut result = BTreeSet::new();
    loop {
        let Response::Canvases { items, next } = call(store, actor, Action::List { after, limit: 3 }) else { panic!("directory response") };
        assert!(items.len() <= 3);
        for item in items {
            assert_eq!(item.name, LABEL);
            assert!(result.insert(item.canvas), "repeated directory identity");
        }
        if let Some(next) = next {
            assert!(after.is_none_or(|previous| next > previous));
            after = Some(next);
        } else {
            return result;
        }
    }
}
fn assert_directories(store: &mut Store, cases: &[Case]) {
    for actor in 1..=5 {
        let expected = cases.iter().filter(|case| case.members.contains(&actor)).map(|case| canvas(case.id)).collect::<BTreeSet<_>>();
        assert_eq!(directory(store, actor), expected, "actor {actor} sees another group or loses a same-name canvas");
    }
}
fn presence(case: &Case) -> Action {
    Action::Presence {
        canvas: canvas(case.id),
        epoch: 1,
        camera: coordinates::home(canvas(case.id)),
        preview: vec![],
        cursor: Some(Default::default()),
        stroke: None,
    }
}
fn check_actor(store: &mut Store, case: &Case, actor: u32) {
    let allowed = case.members.contains(&actor);
    for action in [
        Action::Info { canvas: canvas(case.id) },
        Action::Members { canvas: canvas(case.id), after: None, limit: 128 },
        Action::Events { canvas: canvas(case.id), after: 0, through: None, limit: 128 },
        Action::Peers { canvas: canvas(case.id) },
        Action::Append { operation: op(actor, case.id, 1, 100_000 + case.id * 10 + actor) },
        presence(case),
    ] {
        let result = store.world_execute(&token(actor), &request(action), 100);
        assert_eq!(result.is_ok(), allowed, "actor {actor}, canvas {}, owner {}, response {result:?}", case.id, case.owner);
    }
}

#[test]
fn generated_same_name_repeated_membership_matrix_isolates_every_actor_and_invitation() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("matrix.db");
    let mut store = Store::open(&path, StoreOptions::default(), 0).unwrap();
    for actor in 1..=5 {
        pair(&mut store, actor);
    }
    let mut cases = Vec::new();
    // Every nonempty subset of four actors is represented twice. This gives
    // solo, overlapping, disjoint and identical groups; actor five is always
    // an outsider. Larger concurrent and paginated membership fixtures remain
    // separate, so this exhaustive bounded matrix does not imply a member cap.
    for mask in 1_u32..16 {
        let members = (1..=4).filter(|actor| mask & (1 << (actor - 1)) != 0).collect::<BTreeSet<_>>();
        for repeat in 0..2 {
            let by = *members.iter().nth(repeat % members.len()).unwrap();
            let id = 1000 + mask * 2 + u32::try_from(repeat).unwrap();
            call(&mut store, by, Action::Create { canvas: canvas(id), name: LABEL.into() });
            for &member in &members {
                if member != by {
                    invite(&mut store, by, member, id, 200_000 + id * 10 + member);
                }
            }
            cases.push(Case { id, owner: by, members: members.clone() });
        }
    }
    assert_eq!(cases.len(), 30);
    assert_eq!(cases.iter().map(|case| case.owner).collect::<BTreeSet<_>>(), BTreeSet::from([1, 2, 3, 4]));
    assert_directories(&mut store, &cases);
    for case in &cases {
        for actor in 1..=5 {
            check_actor(&mut store, case, actor);
        }
        let accepted = events(&mut store, case.owner, case.id);
        assert_eq!(accepted.len(), case.members.len());
        assert_eq!(accepted.iter().map(|event| event.op.author).collect::<BTreeSet<OwnerId>>(), case.members.iter().copied().map(owner).collect());
        let Response::Peers { peers } = call(&mut store, case.owner, Action::Peers { canvas: canvas(case.id) }) else { panic!("peers response") };
        assert_eq!(peers.iter().map(|peer| peer.profile).collect::<BTreeSet<_>>(), case.members.iter().copied().map(owner).collect());
    }
    drop(store);
    let mut store = Store::open(&path, StoreOptions::default(), 500).unwrap();
    assert_directories(&mut store, &cases);
    for case in &cases {
        assert_eq!(events(&mut store, case.owner, case.id).len(), case.members.len());
    }
    // Removing someone from one repeated group cannot affect the other canvas,
    // even though their labels, member sets and prior accepted revisions match.
    call(&mut store, 1, Action::Remove { canvas: canvas(1006), member: owner(2) });
    assert!(store.world_execute(&token(2), &request(Action::Info { canvas: canvas(1006) }), 500).is_err());
    call(&mut store, 2, Action::Append { operation: op(2, 1007, 2, 900_000) });
    assert_eq!(events(&mut store, 2, 1007).len(), 3);
    assert_eq!(events(&mut store, 1, 1006).len(), 2);
    assert!(directory(&mut store, 2).contains(&canvas(1007)));
    assert!(!directory(&mut store, 2).contains(&canvas(1006)));
    swapped_invitation(&mut store);
}
fn swapped_invitation(store: &mut Store) {
    for id in [9000, 9001] {
        call(store, 1, Action::Create { canvas: canvas(id), name: LABEL.into() });
    }
    let invitation = OpId::new(&hex(9002)).unwrap();
    call(store, 1, Action::Invite { canvas: canvas(9000), id: invitation, recipient: owner(5), secret: token(9002), expires_at_ms: 100000 });
    let wrong = Action::Accept { canvas: canvas(9001), id: invitation, secret: token(9002) };
    assert!(store.world_execute(&token(5), &request(wrong), 500).is_err());
    assert!(directory(store, 5).is_empty(), "wrong-canvas invitation granted a same-name canvas");
    // The rejected cross-canvas use does not consume the legitimate capability.
    call(store, 5, Action::Accept { canvas: canvas(9000), id: invitation, secret: token(9002) });
    assert_eq!(directory(store, 5), BTreeSet::from([canvas(9000)]));
    assert!(store.world_execute(&token(5), &request(Action::Info { canvas: canvas(9001) }), 500).is_err());
}
