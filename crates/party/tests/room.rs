use photocraft_party::{
    Error,
    contracts::{
        AssetHash, AssetRef, CheckpointId, CheckpointRef, DeviceId, GameId, MAX_ASSETS, MAX_MESSAGE_BYTES, MAX_NOTE_BYTES, MAX_ROOM_SNAPSHOT_BYTES, Mode,
        Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RoomId, Submission, SubmissionId, TRANSFER_GRACE_MS,
    },
    room::{Action, ApplyOutcome, Assignment, PauseReason, Phase, Reference, RevealedEntry, Room},
};

fn participant(n: u8) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", u16::from(n) + 100)).unwrap(),
        display_name: format!("Player {n}"),
    }
}

fn send(room: &mut Room, actor: ParticipantId, now: u64, action: Action) -> ApplyOutcome {
    room.apply(actor, room.revision(), now, action).unwrap()
}

fn started(players: u8, mode: Mode, timer: Option<u64>) -> (Room, Vec<Participant>) {
    let members: Vec<_> = (1..=players).map(participant).collect();
    let host = members.first().unwrap().clone();
    let mut room = Room::new(RoomId::new(&format!("{:032x}", 1000)).unwrap(), host.clone(), mode, timer).unwrap();
    for member in members.iter().skip(1) {
        send(&mut room, member.id, 0, Action::Join { participant: member.clone() });
    }
    for member in &members {
        send(&mut room, member.id, 0, Action::Ready { ready: true });
    }
    let rounds = mode.rounds(usize::from(players)).unwrap();
    let projects = (0..u16::from(rounds) * u16::from(players)).map(|n| ProjectId::new(&format!("{:032x}", 2000 + n)).unwrap()).collect();
    send(&mut room, host.id, 0, Action::Start { game: GameId::new(&format!("{:032x}", 3000)).unwrap(), projects });
    (room, members)
}

fn submission(room: &Room, actor: ParticipantId) -> Submission {
    let assignment = room.view(actor).unwrap().assignment.unwrap().assignment;
    let n = 1 + u64::from(assignment.id.round) * 3 + u64::from(assignment.id.chain);
    let asset = |offset| AssetRef { hash: AssetHash::new(&format!("{:064x}", n * 10 + offset)).unwrap(), bytes: 4 };
    let checkpoint = CheckpointRef { id: CheckpointId::new(&format!("{n:032x}")).unwrap(), asset: asset(1) };
    let image = asset(2);
    let seam = matches!(room.view(actor).unwrap().mode, Mode::ExquisiteCorpse).then(|| asset(3));
    let mut assets = vec![checkpoint.asset, image];
    assets.extend(seam);
    Submission {
        id: SubmissionId::new(&format!("{n:032x}")).unwrap(),
        assignment: assignment.id,
        author: actor,
        project: assignment.project,
        revision: 1,
        checkpoint,
        image,
        replay: ReplayRef {
            manifest: None,
            id: ReplayId::new(&format!("{n:032x}")).unwrap(),
            baseline: checkpoint,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
        },
        assets,
        note: format!("private-note-{n}"),
        seam,
    }
}

fn submit(room: &mut Room, actor: ParticipantId, value: Submission) -> ApplyOutcome {
    let epoch = room.epoch();
    send(room, actor, 0, Action::Submit { epoch, submission: value })
}

#[test]
fn complete_two_and_three_player_schedules_for_both_modes() {
    for count in [2, 3] {
        for mode in [
            Mode::Animation { rotations: 1 },
            Mode::Animation { rotations: 2 },
            Mode::Animation { rotations: 3 },
            Mode::Animation { rotations: 4 },
            Mode::ExquisiteCorpse,
        ] {
            let (mut room, members) = started(count, mode, None);
            let rounds = mode.rounds(usize::from(count)).unwrap();
            for round in 0..rounds {
                for (index, member) in members.iter().enumerate() {
                    let view = room.view(member.id).unwrap();
                    assert_eq!(view.phase, Phase::Drawing);
                    assert!(view.reveal.is_empty());
                    let assignment = view.assignment.unwrap().assignment;
                    assert_eq!(assignment.id.round, round);
                    assert_eq!((usize::from(assignment.id.chain) + usize::from(round)) % usize::from(count), index);
                    let value = submission(&room, member.id);
                    submit(&mut room, member.id, value);
                }
            }
            assert_eq!(room.phase(), Phase::Reveal);
            for member in &members {
                let view = room.view(member.id).unwrap();
                assert_eq!(view.reveal.len(), usize::from(count) * usize::from(rounds));
                assert!(view.assignment.is_none());
                assert!(!serde_json::to_string(&view).unwrap().contains("private-note"));
            }
            assert_eq!(room.notes(members[0].id).unwrap().len(), usize::from(count) * usize::from(rounds));
            let bytes = room.snapshot_bytes().unwrap();
            assert_eq!(Room::from_snapshot(&bytes).unwrap(), room);
            send(&mut room, members[0].id, 0, Action::Archive);
            assert_eq!(room.phase(), Phase::Archived);
        }
    }
}

#[test]
fn corpse_two_players_intentionally_return_to_head_author() {
    let (mut room, members) = started(2, Mode::ExquisiteCorpse, None);
    let head = room.view(members[0].id).unwrap().assignment.unwrap().assignment.id.chain;
    for _ in 0..2 {
        for member in &members {
            let value = submission(&room, member.id);
            submit(&mut room, member.id, value);
        }
    }
    let legs = room.view(members[0].id).unwrap().assignment.unwrap().assignment;
    assert_eq!(head, legs.id.chain);
    assert_eq!(legs.id.round, 2);
}

#[test]
fn projections_authorize_only_current_rendered_reference() {
    for mode in [Mode::Animation { rotations: 2 }, Mode::ExquisiteCorpse] {
        let (mut room, members) = started(3, mode, None);
        let previous = submission(&room, members[0].id);
        assert!(!room.can_fetch(members[1].id, previous.image.hash));
        for member in &members {
            let value = submission(&room, member.id);
            submit(&mut room, member.id, value);
        }
        let view = room.view(members[1].id).unwrap();
        let reference = view.assignment.unwrap().reference.unwrap();
        match mode {
            Mode::Animation { .. } => {
                assert_eq!(reference, Reference::Animation { image: previous.image, note: previous.note.clone() });
                assert!(room.can_fetch(members[1].id, previous.image.hash));
            }
            Mode::ExquisiteCorpse => {
                assert_eq!(reference, Reference::Seam { image: previous.seam.unwrap(), width: 1024, height: 24 });
                assert!(room.can_fetch(members[1].id, previous.seam.unwrap().hash));
                assert!(!room.can_fetch(members[1].id, previous.image.hash));
                assert!(!serde_json::to_string(&room.view(members[1].id).unwrap()).unwrap().contains(&previous.note));
            }
        }
        assert!(!room.can_fetch(members[1].id, previous.checkpoint.asset.hash));
        assert!(!room.can_fetch(members[0].id, previous.image.hash)); // Prior local drafts are not party UI grants.
        assert!(!room.can_fetch(participant(9).id, previous.image.hash));
        assert_eq!(room.notes(members[0].id), Err(Error::Phase));
    }
}

#[test]
fn immutable_retry_survives_round_advance_and_changed_payload_conflicts() {
    let (mut room, members) = started(2, Mode::Animation { rotations: 1 }, None);
    let value = submission(&room, members[0].id);
    let initial_revision = room.revision();
    let epoch = room.epoch();
    let accepted = submit(&mut room, members[0].id, value.clone());
    let other = submission(&room, members[1].id);
    submit(&mut room, members[1].id, other);
    let before = room.clone();
    let retry = room.apply(members[0].id, initial_revision, 0, Action::Submit { epoch, submission: value.clone() }).unwrap();
    assert!(retry.duplicate);
    assert_eq!(retry.revision, accepted.revision);
    assert_eq!(room, before);
    let mut changed = value.clone();
    changed.revision += 1;
    assert_eq!(room.apply(members[0].id, room.revision(), 0, Action::Submit { epoch, submission: changed }), Err(Error::Conflict));
    assert_eq!(room.apply(members[1].id, room.revision(), 0, Action::Submit { epoch, submission: value }), Err(Error::Unauthorized));
    assert_eq!(room, before);
}

#[test]
fn invalid_phase_revision_owner_and_roster_leave_state_unchanged() {
    let (mut room, members) = started(2, Mode::ExquisiteCorpse, None);
    let before = room.clone();
    let value = submission(&room, members[0].id);
    let actions = [
        (members[0].id, room.revision() - 1, Action::Pause),
        (members[1].id, room.revision(), Action::Pause),
        (members[0].id, room.revision(), Action::Ready { ready: true }),
        // K2 permits an invited late arrival to wait. Impersonating that arrival remains invalid.
        (members[0].id, room.revision(), Action::Join { participant: participant(3) }),
        (members[0].id, room.revision(), Action::Submit { epoch: room.epoch() + 1, submission: value.clone() }),
        (members[1].id, room.revision(), Action::Submit { epoch: room.epoch(), submission: value }),
    ];
    for (actor, revision, action) in actions {
        assert!(room.apply(actor, revision, 0, action).is_err());
        assert_eq!(room, before);
    }
}

#[test]
fn disconnect_claim_skip_resume_and_reconnect_preserve_assignment() {
    let (mut room, members) = started(3, Mode::ExquisiteCorpse, Some(1000));
    let assignment = room.view(members[0].id).unwrap().assignment.unwrap().assignment;
    room.set_connected(members[0].id, false, 100).unwrap();
    assert_eq!(room.view(members[1].id).unwrap().paused, Some(PauseReason::Disconnected));
    let claim_revision = room.revision();
    send(&mut room, members[1].id, 100, Action::ClaimModerator);
    assert_eq!(room.apply(members[2].id, claim_revision, 100, Action::ClaimModerator), Err(Error::Conflict));
    assert_eq!(room.apply(members[1].id, room.revision(), 100, Action::Resume), Err(Error::Phase));
    room.set_connected(members[0].id, true, 200).unwrap();
    assert_eq!(room.view(members[0].id).unwrap().assignment.unwrap().assignment, assignment);
    send(&mut room, members[1].id, 200, Action::Resume);
    assert_eq!(room.view(members[0].id).unwrap().deadline_ms, Some(1100));
    room.set_connected(members[0].id, false, 300).unwrap();
    send(&mut room, members[1].id, 300, Action::Skip { participant: members[0].id, reason: "Player stepped away".into() });
    send(&mut room, members[1].id, 300, Action::Resume);
    assert!(room.view(members[1].id).unwrap().paused.is_none());
}

#[test]
fn timer_freeze_binds_manifest_then_grace_records_missing() {
    let (mut room, members) = started(2, Mode::Animation { rotations: 1 }, Some(1000));
    let value = submission(&room, members[0].id);
    assert!(!room.tick(999).unwrap());
    assert!(room.tick(1000).unwrap());
    assert_eq!(room.phase(), Phase::Transfer);
    let epoch = room.epoch();
    assert_eq!(room.apply(members[0].id, room.revision(), 1000, Action::Submit { epoch, submission: value.clone() }), Err(Error::Conflict));
    send(&mut room, members[0].id, 1000, Action::FreezeReport { epoch, submission: value.clone() });
    let mut changed = value.clone();
    changed.revision += 1;
    assert_eq!(room.apply(members[0].id, room.revision(), 1001, Action::Submit { epoch, submission: changed }), Err(Error::Conflict));
    send(&mut room, members[0].id, 1001, Action::Submit { epoch, submission: value });
    assert!(!room.tick(1000 + TRANSFER_GRACE_MS - 1).unwrap());
    assert!(room.tick(1000 + TRANSFER_GRACE_MS).unwrap());
    assert_eq!(room.phase(), Phase::Drawing);
    assert_eq!(room.epoch(), epoch + 1);
    let view = room.view(members[0].id).unwrap();
    assert_eq!(view.assignment.unwrap().reference, Some(Reference::Missing));
    assert!(Room::from_snapshot(&room.snapshot_bytes().unwrap()).is_ok());
}

#[test]
fn restart_pauses_and_rebases_timer_without_expiring_work() {
    let (mut room, members) = started(2, Mode::Animation { rotations: 1 }, Some(1000));
    room.tick(400).unwrap();
    let assignment = room.view(members[0].id).unwrap().assignment.unwrap().assignment;
    let snapshot = room.snapshot_bytes().unwrap();
    let mut recovered = Room::from_snapshot(&snapshot).unwrap();
    recovered.restart(0).unwrap();
    assert!(!recovered.tick(9000).unwrap());
    assert_eq!(recovered.view(members[0].id).unwrap().paused, Some(PauseReason::Restart));
    for member in &members {
        recovered.set_connected(member.id, true, 9000).unwrap();
    }
    send(&mut recovered, members[0].id, 9000, Action::Resume);
    assert_eq!(recovered.view(members[0].id).unwrap().deadline_ms, Some(9600));
    assert_eq!(recovered.view(members[0].id).unwrap().assignment.unwrap().assignment, assignment);
    assert_eq!(recovered.phase(), Phase::Drawing);
}

#[test]
fn explicit_end_archives_missing_work_without_inventing_images() {
    let (mut room, members) = started(2, Mode::ExquisiteCorpse, None);
    let value = submission(&room, members[0].id);
    submit(&mut room, members[0].id, value);
    send(&mut room, members[0].id, 0, Action::End { reason: "We stopped early".into() });
    let view = room.view(members[1].id).unwrap();
    assert_eq!(view.phase, Phase::Archived);
    assert_eq!(view.reveal.iter().filter(|entry| matches!(entry, RevealedEntry::Artwork { .. })).count(), 1);
    assert_eq!(view.reveal.iter().filter(|entry| matches!(entry, RevealedEntry::Missing { .. })).count(), 5);
    assert!(Room::from_snapshot(&room.snapshot_bytes().unwrap()).is_ok());
}

#[test]
fn malformed_snapshots_are_rejected_before_use() {
    let (room, _) = started(2, Mode::ExquisiteCorpse, None);
    let original: serde_json::Value = serde_json::from_slice(&room.snapshot_bytes().unwrap()).unwrap();
    for (key, value) in [
        ("schema_version", serde_json::json!(999)),
        ("round", serde_json::json!(200)),
        ("epoch", serde_json::json!(999)),
        ("members", serde_json::json!([])),
        ("assignments", serde_json::json!([])),
        ("phase", serde_json::json!("reveal")),
        ("deadline_ms", serde_json::json!(1000)),
    ] {
        let mut mutated = original.clone();
        mutated[key] = value;
        assert!(Room::from_snapshot(&serde_json::to_vec(&mutated).unwrap()).is_err(), "{key}");
    }
    assert!(Room::from_snapshot(b"{}").is_err());
    assert!(Room::from_snapshot(b"{truncated").is_err());
}

#[test]
fn clock_and_deadline_overflow_fail_atomically() {
    let (mut room, members) = started(2, Mode::Animation { rotations: 1 }, Some(1000));
    room.tick(10).unwrap();
    let before = room.clone();
    assert!(room.tick(9).is_err());
    assert!(room.apply(members[0].id, room.revision(), 9, Action::Pause).is_err());
    assert_eq!(room, before);
    assert!(room.tick(u64::MAX).is_err());
    assert_eq!(room, before);
}

#[test]
fn start_requires_ready_roster_and_fresh_unique_project_ids() {
    let host = participant(1);
    assert!(Room::new(RoomId::new(&format!("{:032x}", 1000)).unwrap(), host.clone(), Mode::Animation { rotations: 0 }, None).is_err());
    assert!(Room::new(RoomId::new(&format!("{:032x}", 1000)).unwrap(), host.clone(), Mode::ExquisiteCorpse, Some(0)).is_err());
    let mut room = Room::new(RoomId::new(&format!("{:032x}", 1000)).unwrap(), host.clone(), Mode::ExquisiteCorpse, None).unwrap();
    let game = GameId::new(&format!("{:032x}", 2)).unwrap();
    let project = ProjectId::new(&format!("{:032x}", 3)).unwrap();
    assert!(room.apply(host.id, room.revision(), 0, Action::Start { game, projects: vec![project; 6] }).is_err());
    let other = participant(2);
    send(&mut room, other.id, 0, Action::Join { participant: other.clone() });
    send(&mut room, host.id, 0, Action::Ready { ready: true });
    send(&mut room, other.id, 0, Action::Ready { ready: true });
    let before = room.clone();
    assert!(room.apply(host.id, room.revision(), 0, Action::Start { game, projects: vec![project; 6] }).is_err());
    assert_eq!(room, before);
}

#[test]
fn moderator_loss_at_reveal_does_not_strand_archive() {
    let (mut room, members) = started(2, Mode::Animation { rotations: 1 }, None);
    for _ in 0..2 {
        for member in &members {
            let value = submission(&room, member.id);
            submit(&mut room, member.id, value);
        }
    }
    assert_eq!(room.phase(), Phase::Reveal);
    room.set_connected(members[0].id, false, 0).unwrap();
    send(&mut room, members[1].id, 0, Action::ClaimModerator);
    send(&mut room, members[1].id, 0, Action::Archive);
    assert_eq!(room.phase(), Phase::Archived);
}

#[test]
fn largest_valid_room_snapshot_fits_its_separate_storage_budget() {
    let (mut room, members) = started(3, Mode::Animation { rotations: 4 }, None);
    for _ in 0..12 {
        for member in &members {
            let mut value = submission(&room, member.id);
            for n in 0..MAX_ASSETS - value.assets.len() {
                value.assets.push(AssetRef { hash: AssetHash::new(&format!("{:064x}", 10000 + n)).unwrap(), bytes: 1 });
            }
            value.note = "n".repeat(MAX_NOTE_BYTES);
            submit(&mut room, member.id, value);
        }
    }
    let bytes = room.snapshot_bytes().unwrap();
    assert!(bytes.len() > MAX_MESSAGE_BYTES);
    assert!(bytes.len() < MAX_ROOM_SNAPSHOT_BYTES);
    assert_eq!(Room::from_snapshot(&bytes).unwrap(), room);
    assert!(Room::from_snapshot(&vec![b' '; MAX_ROOM_SNAPSHOT_BYTES + 1]).is_err());
}

#[test]
fn archived_snapshot_cannot_invent_a_submitted_future_round() {
    let (mut room, members) = started(2, Mode::ExquisiteCorpse, None);
    let mut forged = submission(&room, members[0].id);
    send(&mut room, members[0].id, 0, Action::End { reason: "Ended before any drawing".into() });
    let mut snapshot: serde_json::Value = serde_json::from_slice(&room.snapshot_bytes().unwrap()).unwrap();
    let future: Assignment = serde_json::from_value(snapshot["assignments"][2].clone()).unwrap();
    forged.assignment = future.id;
    forged.author = future.player;
    forged.project = future.project;
    snapshot["entries"][2] = serde_json::json!({"kind": "submitted", "value": forged, "accepted_revision": room.revision()});
    assert!(Room::from_snapshot(&serde_json::to_vec(&snapshot).unwrap()).is_err());
}
