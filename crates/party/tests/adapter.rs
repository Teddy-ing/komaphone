use photocraft_party::adapter::{MemoryAuthority, MemoryTransport, Request};
use photocraft_party::room::{Action, Room};
use photocraft_party::{
    AssetRef, CheckpointId, CheckpointRef, DeviceId, Error, GameId, MAX_MESSAGE_BYTES, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef,
    RequestId, RoomId, SCHEMA_VERSION, Submission, SubmissionId,
};

fn hex(n: u64) -> String {
    format!("{n:032x}")
}
fn member(n: u64) -> Participant {
    Participant { id: ParticipantId::new(&hex(n)).unwrap(), device: DeviceId::new(&hex(n + 100)).unwrap(), display_name: format!("Friend {n}") }
}
fn authority() -> MemoryAuthority {
    MemoryAuthority::new(Room::new(RoomId::new(&hex(1)).unwrap(), member(1), Mode::ExquisiteCorpse, None).unwrap()).unwrap()
}
fn request(authority: &MemoryAuthority, id: u64, actor: u64, action: Action) -> Vec<u8> {
    serde_json::to_vec(&Request {
        schema_version: SCHEMA_VERSION,
        request_id: RequestId::new(&hex(id)).unwrap(),
        actor: member(actor).id,
        expected_revision: authority.room().revision(),
        action,
    })
    .unwrap()
}
fn send(authority: &mut MemoryAuthority, id: u64, actor: u64, action: Action) -> Vec<u8> {
    let bytes = request(authority, id, actor, action);
    authority.deliver(member(actor).id, &bytes, 0).unwrap();
    bytes
}
fn started() -> MemoryAuthority {
    started_mode(Mode::ExquisiteCorpse)
}
fn started_mode(mode: Mode) -> MemoryAuthority {
    let mut authority = MemoryAuthority::new(Room::new(RoomId::new(&hex(1)).unwrap(), member(1), mode, None).unwrap()).unwrap();
    send(&mut authority, 1, 2, Action::Join { participant: member(2) });
    send(&mut authority, 2, 1, Action::Ready { ready: true });
    send(&mut authority, 3, 2, Action::Ready { ready: true });
    let projects = (10..10 + u64::from(mode.rounds(2).unwrap()) * 2).map(|n| ProjectId::new(&hex(n)).unwrap()).collect();
    send(&mut authority, 4, 1, Action::Start { game: GameId::new(&hex(1)).unwrap(), projects });
    authority
}
fn submission(authority: &mut MemoryAuthority, actor: u64) -> Submission {
    let assignment = authority.room().view(member(actor).id).unwrap().assignment.unwrap().assignment;
    let n = u64::from(assignment.id.round) * 3 + actor;
    let checkpoint_asset = authority.assets_mut().insert_for(member(actor).id, format!("checkpoint-{n}").as_bytes()).unwrap();
    let image = authority.assets_mut().insert_for(member(actor).id, format!("image-{n}").as_bytes()).unwrap();
    let seam = authority.assets_mut().insert_for(member(actor).id, format!("seam-{n}").as_bytes()).unwrap();
    let checkpoint = CheckpointRef { id: CheckpointId::new(&hex(n)).unwrap(), asset: checkpoint_asset };
    Submission {
        id: SubmissionId::new(&hex(n)).unwrap(),
        assignment: assignment.id,
        author: member(actor).id,
        project: assignment.project,
        revision: 1,
        image,
        checkpoint,
        replay: ReplayRef { manifest: None, id: ReplayId::new(&hex(n)).unwrap(), baseline: checkpoint, end_event: None, event_count: 0, duration_ms: 0 },
        assets: vec![checkpoint_asset, image, seam],
        note: "Not pixels".into(),
        seam: Some(seam),
    }
}
fn submit_action(authority: &MemoryAuthority, submission: Submission) -> Action {
    Action::Submit { epoch: authority.room().epoch(), submission }
}

#[test]
fn dropped_duplicate_and_reordered_requests_commit_once() {
    let mut authority = authority();
    let join = request(&authority, 1, 2, Action::Join { participant: member(2) });
    let early_ready = request(&authority, 2, 1, Action::Ready { ready: true });
    let mut transport = MemoryTransport::default();
    transport.push(early_ready).unwrap();
    transport.push(join.clone()).unwrap();
    transport.push(join.clone()).unwrap();
    // Out-of-order arrival; the reply is lost, then identical bytes arrive again.
    let first = authority.deliver(member(2).id, &transport.take(1).unwrap(), 0).unwrap();
    assert_eq!(authority.deliver(member(2).id, &transport.take(1).unwrap(), 99).unwrap(), first);
    assert_eq!(authority.room().revision(), 1);
    assert_eq!(authority.deliver(member(1).id, &transport.take(0).unwrap(), 0), Err(Error::Conflict));
    assert!(transport.is_empty());
    assert_eq!(authority.deliver(member(1).id, &join, 0), Err(Error::Unauthorized));
    let changed = request(&authority, 1, 2, Action::Ready { ready: true });
    assert_eq!(authority.deliver(member(2).id, &changed, 0), Err(Error::Conflict));
}

#[test]
fn failed_commit_never_acknowledges_or_mutates_then_exact_retry_works() {
    let mut authority = authority();
    let bytes = request(&authority, 1, 2, Action::Join { participant: member(2) });
    let before = authority.store().snapshot().to_vec();
    authority.store_mut().fail_next_commit();
    assert_eq!(authority.deliver(member(2).id, &bytes, 0), Err(Error::Storage));
    assert_eq!(authority.store().snapshot(), before);
    assert_eq!(authority.room().revision(), 0);
    assert!(authority.room().view(member(2).id).is_err());
    let accepted = authority.deliver(member(2).id, &bytes, 0).unwrap();
    authority.restart(0).unwrap();
    assert_eq!(authority.deliver(member(2).id, &bytes, 0).unwrap(), accepted);
    authority.set_connected(member(2).id, true, 0).unwrap();
    send(&mut authority, 2, 2, Action::Ready { ready: true });
}

#[test]
fn restart_reconnect_and_moderation_are_transactional() {
    let mut authority = started();
    let assignment = authority.room().view(member(2).id).unwrap().assignment;
    authority.restart(0).unwrap();
    authority.store_mut().fail_next_commit();
    let before = authority.store().snapshot().to_vec();
    assert_eq!(authority.set_connected(member(2).id, true, 0), Err(Error::Storage));
    assert_eq!(authority.store().snapshot(), before);
    assert!(!authority.room().view(member(2).id).unwrap().members[1].connected);
    authority.set_connected(member(2).id, true, 0).unwrap();
    send(&mut authority, 5, 2, Action::ClaimModerator);
    authority.set_connected(member(1).id, true, 0).unwrap();
    send(&mut authority, 6, 2, Action::Resume);
    assert_eq!(authority.room().view(member(2).id).unwrap().assignment, assignment);
}

#[test]
fn submission_waits_for_dependencies_and_trusted_seam_then_fetches_only_projection() {
    let mut authority = started();
    let first = submission(&mut authority, 1);
    let before = authority.store().snapshot().to_vec();
    let bytes = request(&authority, 5, 1, submit_action(&authority, first.clone()));
    assert_eq!(authority.deliver(member(1).id, &bytes, 0), Err(Error::Unauthorized));
    assert_eq!(authority.store().snapshot(), before);
    authority.assets_mut().register_derived_seam(first.assignment, first.seam.unwrap()).unwrap();
    let mut missing = first.clone();
    let absent = AssetRef { hash: photocraft_party::AssetHash::new(&format!("{:064x}", 555)).unwrap(), bytes: 5 };
    missing.assets.push(absent);
    let missing_bytes = request(&authority, 6, 1, submit_action(&authority, missing));
    assert!(authority.deliver(member(1).id, &missing_bytes, 0).is_err());
    assert_eq!(authority.store().snapshot(), before);
    authority.deliver(member(1).id, &bytes, 0).unwrap();
    assert_eq!(authority.fetch(member(2).id, first.image.hash), Err(Error::Unauthorized));
    let second = submission(&mut authority, 2);
    authority.assets_mut().register_derived_seam(second.assignment, second.seam.unwrap()).unwrap();
    let action = submit_action(&authority, second);
    send(&mut authority, 7, 2, action);
    assert_eq!(authority.fetch(member(2).id, first.seam.unwrap().hash).unwrap(), b"seam-1");
    assert_eq!(authority.fetch(member(2).id, first.image.hash), Err(Error::Unauthorized));
    assert_eq!(authority.fetch(member(2).id, first.checkpoint.asset.hash), Err(Error::Unauthorized));
    assert_eq!(authority.fetch(member(3).id, first.seam.unwrap().hash), Err(Error::Unauthorized));
}

#[test]
fn byte_ingress_and_transport_queue_are_bounded() {
    let mut authority = authority();
    assert!(authority.deliver(member(1).id, b"{", 0).is_err());
    assert!(authority.deliver(member(1).id, &vec![b' '; MAX_MESSAGE_BYTES + 1], 0).is_err());
    let mut transport = MemoryTransport::default();
    for _ in 0..64 {
        transport.push(b"{}".to_vec()).unwrap();
    }
    assert!(transport.push(Vec::new()).is_err());
    assert!(transport.take(usize::MAX).is_err());
    assert!(MemoryTransport::default().push(vec![0; MAX_MESSAGE_BYTES + 1]).is_err());
}

#[test]
fn knowing_a_hidden_hash_cannot_launder_it_through_your_submission() {
    let mut authority = started();
    let first = submission(&mut authority, 1);
    let mut second = submission(&mut authority, 2);
    authority.assets_mut().register_derived_seam(second.assignment, second.seam.unwrap()).unwrap();
    second.assets.push(first.checkpoint.asset);
    let bytes = request(&authority, 5, 2, submit_action(&authority, second));
    let before = authority.store().snapshot().to_vec();
    assert_eq!(authority.deliver(member(2).id, &bytes, 0), Err(Error::Unauthorized));
    assert_eq!(authority.store().snapshot(), before);
    assert_eq!(authority.fetch(member(2).id, first.checkpoint.asset.hash), Err(Error::Unauthorized));
}

#[test]
fn immutable_retry_remains_valid_after_its_borrowed_reference_grant_expires() {
    let mut authority = started_mode(Mode::Animation { rotations: 2 });
    let mut previous_image = None;
    for actor in [1, 2] {
        let mut value = submission(&mut authority, actor);
        if actor == 1 {
            previous_image = Some(value.image);
        }
        value.assets.retain(|asset| Some(*asset) != value.seam);
        value.seam = None;
        let action = submit_action(&authority, value);
        send(&mut authority, 4 + actor, actor, action);
    }
    let borrowed = previous_image.unwrap();
    assert!(authority.fetch(member(2).id, borrowed.hash).is_ok());
    let mut retry = None;
    for actor in [1, 2] {
        let mut value = submission(&mut authority, actor);
        value.assets.retain(|asset| Some(*asset) != value.seam);
        value.seam = None;
        if actor == 2 {
            value.assets.push(borrowed);
        }
        let action = submit_action(&authority, value);
        if actor == 2 {
            retry = Some(action.clone());
        }
        send(&mut authority, 6 + actor, actor, action);
    }
    assert_eq!(authority.fetch(member(2).id, borrowed.hash), Err(Error::Unauthorized));
    let before = authority.room().revision();
    let bytes = request(&authority, 9, 2, retry.unwrap());
    let receipt = authority.deliver(member(2).id, &bytes, 0).unwrap();
    assert!(receipt.duplicate);
    assert_eq!(authority.room().revision(), before);
    assert_eq!(authority.fetch(member(2).id, borrowed.hash), Err(Error::Unauthorized));
}
