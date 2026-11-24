use super::*;
use crate::identity::{GOOGLE_ISSUER, LOGIN_LIFETIME_MS, LoginChallenge, VerifiedLogin};
use photocraft_party::RequestId;
use photocraft_party::protocol::{Operation, RpcRequest};
fn device(n: u64) -> DeviceId {
    DeviceId::new(&format!("{n:032x}")).unwrap()
}
fn verified(login: &LoginChallenge, subject: &str) -> VerifiedLogin {
    VerifiedLogin {
        transaction: login.id,
        nonce_hash: crate::store::digest(login.nonce.as_bytes()),
        issuer: GOOGLE_ISSUER.into(),
        subject: subject.into(),
        expires_at_ms: LOGIN_LIFETIME_MS,
    }
}
fn fixture() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path().join("service.sqlite3"), crate::StoreOptions { hosted: true, ..Default::default() }, 0).unwrap();
    (dir, store)
}
#[test]
fn one_use_admission_creates_isolated_profile_and_device_proof_survives_lost_reply() {
    let (dir, mut store) = fixture();
    let proof = "1".repeat(64);
    let login = store.identity_begin_login(device(1), 100).unwrap();
    assert!(store.identity_finish_admitted_login(verified(&login, "alice"), Some(&proof), None, "Alice", 100).is_err());
    let admission = store.identity_create_admission(1000, 100).unwrap();
    let session = store.identity_finish_admitted_login(verified(&login, "alice"), Some(&proof), Some(&admission), "Alice", 100).unwrap();
    let original = session.context;
    assert_eq!(session.device_credential(), proof);
    assert!(store.identity_authenticate(&proof, 100).is_err());
    assert_eq!(store.identity_authenticate(session.token(), 100).unwrap(), original);
    assert!(store.identity_finish_admitted_login(verified(&login, "alice"), Some(&proof), Some(&admission), "Alice", 100).is_err());
    let stranger = store.identity_begin_login(device(2), 100).unwrap();
    assert!(store.identity_finish_admitted_login(verified(&stranger, "bob"), Some(&"2".repeat(64)), Some(&admission), "Bob", 100).is_err());
    // Lost HTTP response: a fresh login with pre-persisted writer proof recovers.
    drop(store);
    let mut store = Store::open(dir.path().join("service.sqlite3"), crate::StoreOptions { hosted: true, ..Default::default() }, 200).unwrap();
    let retry = store.identity_begin_login(device(1), 200).unwrap();
    let renewed = store.identity_finish_admitted_login(verified(&retry, "alice"), Some(&proof), None, "ignored renamed label", 200).unwrap();
    assert_eq!(renewed.context, original);
    let participant = store.identity_participant(original).unwrap();
    assert_eq!(participant.display_name, "Alice");
    let second = store.identity_begin_login(device(3), 200).unwrap();
    let device_session = store.identity_finish_admitted_login(verified(&second, "alice"), Some(&"3".repeat(64)), None, "Alice", 200).unwrap();
    assert_eq!(device_session.context.profile, original.profile);
    assert_ne!(device_session.context.device, original.device);
    store.identity_revoke_profile(original.profile).unwrap();
    assert!(store.identity_authenticate(renewed.token(), 200).is_err());
    assert!(store.identity_authenticate(device_session.token(), 200).is_err());
    let denied = store.identity_begin_login(device(1), 200).unwrap();
    assert!(store.identity_finish_admitted_login(verified(&denied, "alice"), Some(&proof), None, "Alice", 200).is_err());
}
#[test]
fn hosted_pair_is_denied_and_expired_or_canceled_admission_cannot_enroll() {
    let (_dir, mut store) = fixture();
    let participant = Participant { id: ParticipantId::new(&format!("{:032x}", 1)).unwrap(), device: device(1), display_name: "Alice".into() };
    let request = RpcRequest {
        protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
        request_id: RequestId::new(&format!("{:032x}", 1)).unwrap(),
        operation: Operation::Pair { participant, credential: "1".repeat(64) },
    };
    assert!(store.execute(None, &request, 100).is_err());
    let admission = store.identity_create_admission(100, 0).unwrap();
    let login = store.identity_begin_login(device(1), 100).unwrap();
    assert!(store.identity_finish_admitted_login(verified(&login, "alice"), Some(&"1".repeat(64)), Some(&admission), "Alice", 100).is_err());
    let admission = store.identity_create_admission(1000, 100).unwrap();
    store.identity_cancel_login(login.id, &login.nonce).unwrap();
    assert!(store.identity_finish_admitted_login(verified(&login, "alice"), Some(&"1".repeat(64)), Some(&admission), "Alice", 100).is_err());
}
#[test]
fn restored_access_and_admission_are_invalidated_without_removing_art_profiles() {
    let (_dir, mut store) = fixture();
    let admission = store.identity_create_admission(1000, 0).unwrap();
    let login = store.identity_begin_login(device(1), 0).unwrap();
    let session = store.identity_finish_admitted_login(verified(&login, "alice"), Some(&"1".repeat(64)), Some(&admission), "Alice", 0).unwrap();
    let unused = store.identity_create_admission(1000, 0).unwrap();
    store.identity_invalidate_restored_access().unwrap();
    assert!(store.identity_authenticate(session.token(), 1).is_err());
    assert_eq!(store.identity_participant(session.context).unwrap().display_name, "Alice");
    let login = store.identity_begin_login(device(2), 1).unwrap();
    assert!(store.identity_finish_admitted_login(verified(&login, "bob"), Some(&"2".repeat(64)), Some(&unused), "Bob", 1).is_err());
}

fn enroll(store: &mut Store, n: u64) -> crate::identity::IssuedSession {
    let admission = store.identity_create_admission(1000, 0).unwrap();
    let login = store.identity_begin_login(device(n), 0).unwrap();
    store
        .identity_finish_admitted_login(verified(&login, &format!("subject-{n}")), Some(&format!("{n:064x}")), Some(&admission), &format!("Person {n}"), 0)
        .unwrap()
}
fn world_call(store: &mut Store, token: &str, action: photocraft_scene::world_protocol::Action) -> photocraft_scene::world_protocol::Response {
    store.world_execute(token, &photocraft_scene::world_protocol::Request { version: photocraft_scene::world_protocol::VERSION, action }, 1).unwrap()
}
#[test]
fn google_only_people_are_paginated_and_revoked_people_are_hidden() {
    use photocraft_scene::world_protocol::{Action, Response};
    let (_dir, mut store) = fixture();
    let sessions: Vec<_> = (1..=8).map(|n| enroll(&mut store, n)).collect();
    assert_eq!(store.db.query_row("SELECT count(*) FROM identities", [], |r| r.get::<_, i64>(0)).unwrap(), 0);
    let mut after = None;
    let mut people = Vec::new();
    loop {
        let Response::People { items, next } = world_call(&mut store, sessions[0].token(), Action::People { after, limit: 3 }) else { panic!("people") };
        people.extend(items);
        after = next;
        if after.is_none() {
            break;
        }
    }
    assert_eq!(people.len(), 8);
    for (i, session) in sessions.iter().enumerate() {
        assert!(people.iter().any(|p| p.profile.as_str() == session.context.profile.as_str() && p.display_name == format!("Person {}", i + 1)));
    }
    store.identity_revoke_profile(sessions[7].context.profile).unwrap();
    let Response::People { items, next } = world_call(&mut store, sessions[0].token(), Action::People { after: None, limit: 20 }) else { panic!("people") };
    assert_eq!(items.len(), 7);
    assert!(next.is_none());
    assert!(!items.iter().any(|p| p.profile.as_str() == sessions[7].context.profile.as_str()));
}
#[test]
fn google_only_stroke_copy_preserves_attribution_without_legacy_identity() {
    use photocraft_scene::shared::media::Credit;
    use photocraft_scene::shared::{Edit, OpId, StrokeCopyTarget, WorldOp, WorldState, stroke_copy};
    use photocraft_scene::world_protocol::{Action, Response};
    use photocraft_scene::{CanvasId, OwnerId, Srgba, Stroke, StrokeId};
    let (_dir, mut store) = fixture();
    let session = enroll(&mut store, 1);
    let owner = OwnerId::new(session.context.profile.as_str()).unwrap();
    let canvas = CanvasId::new(&format!("{:032x}", 100)).unwrap();
    world_call(&mut store, session.token(), Action::Create { canvas, name: "Hosted art".into() });
    let stroke = Stroke {
        id: StrokeId::new(&format!("{:032x}", 1)).unwrap(),
        canvas,
        owner,
        revision: 0,
        anchor: Default::default(),
        points: vec![[1., 1.], [2., 2.]],
        width: 3.,
        color: Srgba { rgba: [0., 0., 0., 1.] },
        erase: false,
        brush: None,
        samples: None,
    };
    let mut operation = WorldOp {
        id: OpId::new(&format!("{:032x}", 1)).unwrap(),
        canvas,
        author: owner,
        device: photocraft_scene::shared::DeviceId::new(session.context.device.as_str()).unwrap(),
        sequence: 1,
        authority_epoch: 1,
        action: Edit::AddStroke { stroke },
    };
    world_call(&mut store, session.token(), Action::Append { operation: operation.clone() });
    let Response::Events { events, .. } = world_call(&mut store, session.token(), Action::Events { canvas, after: 0, through: None, limit: 10 }) else {
        panic!("events")
    };
    let mut state = WorldState::new(canvas, owner, "Hosted art".into()).unwrap();
    state.apply(&events[0]).unwrap();
    let source = state.strokes.values().next().unwrap();
    operation.id = OpId::new(&format!("{:032x}", 2)).unwrap();
    operation.sequence = 2;
    operation.action = stroke_copy(
        source,
        canvas,
        Credit { profile: owner, label: "Person 1".into() },
        StrokeCopyTarget { canvas, id: StrokeId::new(&format!("{:032x}", 2)).unwrap(), owner, anchor: Default::default(), offset: [10., 10.], scale: 1. },
    )
    .unwrap();
    world_call(&mut store, session.token(), Action::RemixStroke { operation });
    let Response::Events { events, .. } = world_call(&mut store, session.token(), Action::Events { canvas, after: 1, through: None, limit: 10 }) else {
        panic!("events")
    };
    let Edit::CopyStroke { provenance, .. } = &events[0].op.action else { panic!("copy") };
    assert_eq!(provenance.author, Credit { profile: owner, label: "Person 1".into() });
    store.identity_revoke_profile(session.context.profile).unwrap();
    assert_eq!(profile_label(&store.db, owner.as_str()).unwrap(), "Person 1"); // Attribution survives access revocation.
}
