//! K2 lifecycle regressions, independent of the service and UI implementation.
use photocraft_party::{
    AssetHash, AssetRef, CheckpointId, CheckpointRef, DeviceId, Error, GameId, MAX_TIMER_MS, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef,
    RoomId, Submission, SubmissionId,
    room::{Action, PauseReason, Phase, Room},
};

fn participant(n: u16) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 100)).unwrap(),
        display_name: format!("Player {n}"),
    }
}

fn apply(room: &mut Room, actor: ParticipantId, action: Action) {
    room.apply(actor, room.revision(), 0, action).unwrap();
}

fn lobby() -> (Room, Participant, Participant) {
    let host = participant(1);
    let guest = participant(2);
    let mut room = Room::new(RoomId::new(&format!("{:032x}", 1000)).unwrap(), host.clone(), Mode::Animation { rotations: 2 }, None).unwrap();
    apply(&mut room, guest.id, Action::Join { participant: guest.clone() });
    (room, host, guest)
}

fn start(room: &mut Room, host: ParticipantId, game_number: u16) {
    let members = room.view(host).unwrap().members;
    for member in &members {
        apply(room, member.participant.id, Action::Ready { ready: true });
    }
    let count = usize::from(room.view(host).unwrap().mode.rounds(members.len()).unwrap()) * members.len();
    let projects = (0..count).map(|n| ProjectId::new(&format!("{:032x}", usize::from(game_number) * 100 + n)).unwrap()).collect();
    apply(room, host, Action::Start { game: GameId::new(&format!("{game_number:032x}")).unwrap(), projects });
}

fn submission(room: &Room, author: ParticipantId) -> Submission {
    let assignment = room.view(author).unwrap().assignment.unwrap().assignment;
    let asset = |n| AssetRef { hash: AssetHash::new(&format!("{n:064x}")).unwrap(), bytes: 4 };
    let checkpoint = CheckpointRef { id: CheckpointId::new(&format!("{:032x}", 50)).unwrap(), asset: asset(1) };
    Submission {
        id: SubmissionId::new(&format!("{:032x}", 60 + u16::from(assignment.id.round) * 3 + u16::from(assignment.id.chain))).unwrap(),
        assignment: assignment.id,
        author,
        project: assignment.project,
        revision: 1,
        image: asset(2),
        checkpoint,
        replay: ReplayRef {
            id: ReplayId::new(&format!("{:032x}", 70)).unwrap(),
            baseline: checkpoint,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
            manifest: None,
        },
        assets: vec![asset(1), asset(2)],
        note: "Private prior frame note".into(),
        seam: None,
    }
}

#[test]
fn changed_options_clear_consent_and_invalid_options_are_transactional() {
    let (mut room, host, guest) = lobby();
    for member in [&host, &guest] {
        apply(&mut room, member.id, Action::Ready { ready: true });
    }
    let before = room.clone();
    assert_eq!(room.apply(guest.id, room.revision(), 0, Action::SetOptions { mode: Mode::ExquisiteCorpse, timer_ms: None }), Err(Error::Unauthorized));
    for timer_ms in [Some(0), Some(MAX_TIMER_MS + 1)] {
        assert!(room.apply(host.id, room.revision(), 0, Action::SetOptions { mode: Mode::ExquisiteCorpse, timer_ms }).is_err());
        assert_eq!(room, before);
    }
    apply(&mut room, host.id, Action::SetOptions { mode: Mode::ExquisiteCorpse, timer_ms: Some(60_000) });
    let view = room.view(host.id).unwrap();
    assert_eq!(view.mode, Mode::ExquisiteCorpse);
    assert_eq!(view.timer_ms, Some(60_000));
    assert!(view.members.iter().all(|m| !m.ready));
    assert_eq!(Room::from_snapshot(&room.snapshot_bytes().unwrap()).unwrap(), room);
    start(&mut room, host.id, 200);
    let before = room.clone();
    assert_eq!(room.apply(host.id, room.revision(), 0, Action::SetOptions { mode: Mode::Animation { rotations: 1 }, timer_ms: None }), Err(Error::Phase));
    assert_eq!(room, before);
}

#[test]
fn late_arrival_waits_without_receiving_or_disrupting_active_game() {
    let (mut room, host, guest) = lobby();
    start(&mut room, host.id, 200);
    let initial_assignment = room.view(guest.id).unwrap().assignment;
    let value = submission(&room, host.id);
    let epoch = room.epoch();
    apply(&mut room, host.id, Action::Submit { epoch, submission: value.clone() });
    let waiting = participant(3);
    apply(&mut room, waiting.id, Action::Join { participant: waiting.clone() });
    let view = room.view(waiting.id).unwrap();
    assert_eq!(view.members.len(), 2);
    assert_eq!(view.waiting.len(), 1);
    assert!(view.assignment.is_none() && view.reveal.is_empty());
    let payload = serde_json::to_string(&view).unwrap();
    for hidden in [value.image.hash.as_str(), value.checkpoint.asset.hash.as_str(), &value.note] {
        assert!(!payload.contains(hidden));
    }
    assert!(!room.can_fetch(waiting.id, value.image.hash));
    assert_eq!(room.notes(waiting.id), Err(Error::Unauthorized));
    for action in [Action::Ready { ready: true }, Action::ClaimModerator, Action::Pause, Action::NextGame] {
        assert_eq!(room.apply(waiting.id, room.revision(), 0, action), Err(Error::Phase));
    }
    room.set_connected(waiting.id, false, 0).unwrap();
    assert_eq!(room.view(host.id).unwrap().paused, None);
    assert_eq!(room.view(guest.id).unwrap().assignment, initial_assignment);
    room.set_connected(waiting.id, true, 0).unwrap();
    apply(&mut room, host.id, Action::End { reason: "Retain this incomplete game".into() });
    assert!(!room.view(host.id).unwrap().reveal.is_empty());
    assert!(room.view(waiting.id).unwrap().reveal.is_empty());
    assert!(!room.can_fetch(waiting.id, value.checkpoint.asset.hash));
    assert_eq!(Room::from_snapshot(&room.snapshot_bytes().unwrap()).unwrap(), room);
}

#[test]
fn next_game_promotes_waiting_player_and_rejects_old_assignment() {
    let (mut room, host, _) = lobby();
    start(&mut room, host.id, 200);
    let old = submission(&room, host.id);
    let waiting = participant(3);
    apply(&mut room, waiting.id, Action::Join { participant: waiting.clone() });
    apply(&mut room, host.id, Action::End { reason: "Start another game".into() });
    let archived = room.snapshot_bytes().unwrap();
    let terminal_revision = room.revision();
    apply(&mut room, host.id, Action::NextGame);
    let view = room.view(host.id).unwrap();
    assert_eq!(view.phase, Phase::Lobby);
    assert_eq!(view.revision, terminal_revision + 1);
    assert_eq!(view.members.len(), 3);
    assert!(view.waiting.is_empty() && view.reveal.is_empty() && view.assignment.is_none());
    assert!(view.members.iter().all(|member| !member.ready));
    assert_eq!(Room::from_snapshot(&room.snapshot_bytes().unwrap()).unwrap(), room);
    assert_eq!(Room::from_snapshot(&archived).unwrap().phase(), Phase::Archived);
    start(&mut room, host.id, 201);
    assert!(room.view(waiting.id).unwrap().assignment.is_some());
    let before = room.clone();
    assert_eq!(room.apply(host.id, room.revision(), 0, Action::Submit { epoch: room.epoch(), submission: old }), Err(Error::Unauthorized));
    assert_eq!(room, before);
}

#[test]
fn leave_rejoins_lobby_but_active_roster_and_assignment_stay_frozen() {
    let (mut room, host, guest) = lobby();
    apply(&mut room, guest.id, Action::Leave);
    assert_eq!(room.view(host.id).unwrap().members.len(), 1);
    assert_eq!(room.view(guest.id), Err(Error::Unauthorized));
    apply(&mut room, guest.id, Action::Join { participant: guest.clone() });
    start(&mut room, host.id, 200);
    let before_assignment = room.view(guest.id).unwrap().assignment;
    apply(&mut room, guest.id, Action::Leave);
    let view = room.view(host.id).unwrap();
    assert_eq!(view.members.len(), 2);
    assert_eq!(view.paused, Some(PauseReason::Disconnected));
    assert_eq!(room.apply(guest.id, room.revision(), 0, Action::Join { participant: guest.clone() }), Err(Error::Conflict));
    room.set_connected(guest.id, true, 0).unwrap();
    assert_eq!(room.view(guest.id).unwrap().assignment, before_assignment);
    apply(&mut room, host.id, Action::Resume);
    assert_eq!(room.view(host.id).unwrap().paused, None);
}

#[test]
fn lobby_host_leave_transfers_role_and_last_member_room_is_recoverable() {
    let (mut room, host, guest) = lobby();
    apply(&mut room, host.id, Action::Leave);
    assert_eq!(room.view(guest.id).unwrap().moderator, guest.id);
    apply(&mut room, guest.id, Action::Leave);
    let view = room.view(guest.id).unwrap();
    assert_eq!(view.members.len(), 1);
    assert!(!view.members[0].connected);
    let bytes = room.snapshot_bytes().unwrap();
    let mut reopened = Room::from_snapshot(&bytes).unwrap();
    reopened.restart(0).unwrap();
    reopened.set_connected(guest.id, true, 0).unwrap();
    assert!(reopened.view(guest.id).unwrap().members[0].connected);
}

#[test]
fn waiting_leave_frees_slot_without_changing_frozen_roster() {
    let (mut room, host, _) = lobby();
    start(&mut room, host.id, 200);
    let waiting = participant(3);
    apply(&mut room, waiting.id, Action::Join { participant: waiting.clone() });
    let fourth = participant(4);
    assert_eq!(room.apply(fourth.id, room.revision(), 0, Action::Join { participant: fourth.clone() }), Err(Error::Limit("players")));
    apply(&mut room, waiting.id, Action::Leave);
    assert_eq!(room.view(waiting.id), Err(Error::Unauthorized));
    apply(&mut room, fourth.id, Action::Join { participant: fourth.clone() });
    let view = room.view(host.id).unwrap();
    assert_eq!(view.members.len(), 2);
    assert_eq!(view.waiting[0].participant.id, fourth.id);
}

#[test]
fn old_snapshots_default_waiting_and_forged_membership_is_rejected() {
    let (room, host, _) = lobby();
    let mut state: serde_json::Value = serde_json::from_slice(&room.snapshot_bytes().unwrap()).unwrap();
    state.as_object_mut().unwrap().remove("waiting");
    assert_eq!(Room::from_snapshot(&serde_json::to_vec(&state).unwrap()).unwrap(), room);
    let duplicate = state["members"][0].clone();
    state["waiting"] = serde_json::json!([duplicate]);
    assert!(Room::from_snapshot(&serde_json::to_vec(&state).unwrap()).is_err());
    state["waiting"] = serde_json::json!([]);
    state["schema_version"] = serde_json::json!(999);
    assert_eq!(Room::from_snapshot(&serde_json::to_vec(&state).unwrap()), Err(Error::Version(999)));
    assert_eq!(room.view(host.id).unwrap().phase, Phase::Lobby);
}

#[test]
fn archived_game_recovers_moderation_once_after_host_disconnect() {
    let (mut room, host, guest) = lobby();
    let third = participant(3);
    apply(&mut room, third.id, Action::Join { participant: third.clone() });
    start(&mut room, host.id, 200);
    apply(&mut room, host.id, Action::End { reason: "Keep completed portions".into() });
    room.set_connected(host.id, false, 0).unwrap();
    let contested_revision = room.revision();
    room.apply(guest.id, contested_revision, 0, Action::ClaimModerator).unwrap();
    assert_eq!(room.apply(third.id, contested_revision, 0, Action::ClaimModerator), Err(Error::Conflict));
    assert_eq!(room.view(guest.id).unwrap().moderator, guest.id);
    apply(&mut room, guest.id, Action::NextGame);
    assert_eq!(room.phase(), Phase::Lobby);
    assert_eq!(Room::from_snapshot(&room.snapshot_bytes().unwrap()).unwrap(), room);
}

#[test]
fn simultaneous_submissions_share_base_revision_without_advancing_twice() {
    for count in [2, 3] {
        let (mut room, host, guest) = lobby();
        let mut players = vec![host.clone(), guest];
        if count == 3 {
            let third = participant(3);
            apply(&mut room, third.id, Action::Join { participant: third.clone() });
            players.push(third);
        }
        start(&mut room, host.id, 200);
        let base = room.revision();
        let epoch = room.epoch();
        let values: Vec<_> = players.iter().map(|p| submission(&room, p.id)).collect();
        let mut accepted = Vec::new();
        for value in values.iter().rev() {
            accepted.push(room.apply(value.author, base, 0, Action::Submit { epoch, submission: value.clone() }).unwrap());
        }
        assert_eq!(room.view(host.id).unwrap().round, 1);
        assert_eq!(room.revision(), base + u64::try_from(count).unwrap());
        let stable = room.clone();
        for (value, original) in values.iter().rev().zip(accepted) {
            let duplicate = room.apply(value.author, base, 0, Action::Submit { epoch, submission: value.clone() }).unwrap();
            assert!(duplicate.duplicate);
            assert_eq!(duplicate.revision, original.revision);
        }
        let current = submission(&room, host.id);
        assert_eq!(room.apply(host.id, base, 0, Action::Submit { epoch, submission: current.clone() }), Err(Error::Phase));
        assert_eq!(room.apply(host.id, room.revision() + 1, 0, Action::Submit { epoch: room.epoch(), submission: current }), Err(Error::Conflict));
        assert_eq!(room, stable);
    }
}

#[test]
fn simultaneous_freeze_manifests_preserve_exact_payload_and_metadata_cas() {
    let (mut room, host, guest) = lobby();
    apply(&mut room, host.id, Action::SetOptions { mode: Mode::Animation { rotations: 1 }, timer_ms: Some(1000) });
    start(&mut room, host.id, 200);
    room.tick(1000).unwrap();
    let base = room.revision();
    let epoch = room.epoch();
    let values = [submission(&room, host.id), submission(&room, guest.id)];
    for value in &values {
        room.apply(value.author, base, 1000, Action::FreezeReport { epoch, submission: value.clone() }).unwrap();
    }
    assert_eq!(room.apply(host.id, base, 1000, Action::Pause), Err(Error::Conflict), "Metadata still needs current revision");
    let mut changed = values[0].clone();
    changed.revision += 1;
    assert_eq!(room.apply(host.id, base, 1000, Action::Submit { epoch, submission: changed }), Err(Error::Conflict));
    for value in values {
        room.apply(value.author, base, 1000, Action::Submit { epoch, submission: value }).unwrap();
    }
    assert_eq!(room.view(host.id).unwrap().round, 1);
    assert_eq!(room.phase(), Phase::Drawing);
    assert_eq!(Room::from_snapshot(&room.snapshot_bytes().unwrap()).unwrap(), room);
}
