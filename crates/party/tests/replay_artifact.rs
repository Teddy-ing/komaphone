//! Accepted replay packages retain exact history without trusting remote endpoint metadata.
use photocraft_party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, EventId, GameId, MAX_ASSET_BYTES, ParticipantId, ProjectId, ProjectRecord,
    ReplayId, ReplayRef, Submission, SubmissionId,
    replay_artifact::{ReplayArtifact, ReplayArtifactStep, pack_checkpoints, unpack_checkpoints},
};
use serde_json::json;

fn asset(n: u16) -> AssetRef {
    AssetRef { hash: AssetHash::new(&format!("{n:064x}")).unwrap(), bytes: 16 }
}
fn checkpoint(n: u16) -> CheckpointRef {
    CheckpointRef { id: CheckpointId::new(&format!("{n:032x}")).unwrap(), asset: asset(n) }
}
fn fixture() -> (ReplayArtifact, Submission) {
    let project = ProjectId::new(&format!("{:032x}", 10)).unwrap();
    let replay = ReplayId::new(&format!("{:032x}", 11)).unwrap();
    let steps = [
        ("baseline", "file.new", checkpoint(1), json!({"width":64,"height":48})),
        ("resolved", "layer.rename", checkpoint(2), json!({"name":"Changed"})),
        ("undo", "edit.undo", checkpoint(1), json!({})),
    ]
    .into_iter()
    .enumerate()
    .map(|(index, (kind, command, checkpoint, params))| ReplayArtifactStep {
        event: EventId::new(&format!("{:032x}", index + 20)).unwrap(),
        revision: index as u64 + 1,
        elapsed_ms: index as u64 * 100,
        command: command.into(),
        params,
        kind: kind.into(),
        checkpoint,
    })
    .collect();
    let artifact = ReplayArtifact { schema_version: 1, replay, project, packs: Vec::new(), steps };
    let submission = Submission {
        id: SubmissionId::new(&format!("{:032x}", 30)).unwrap(),
        assignment: AssignmentId { game: GameId::new(&format!("{:032x}", 31)).unwrap(), chain: 0, round: 0 },
        author: ParticipantId::new(&format!("{:032x}", 32)).unwrap(),
        project,
        revision: 3,
        image: asset(3),
        checkpoint: checkpoint(1),
        replay: ReplayRef {
            id: replay,
            manifest: Some(asset(4)),
            baseline: checkpoint(1),
            end_event: Some(EventId::new(&format!("{:032x}", 22)).unwrap()),
            event_count: 2,
            duration_ms: 200,
        },
        assets: vec![asset(1), asset(2), asset(3), asset(4)],
        note: String::new(),
        seam: None,
    };
    (artifact, submission)
}

#[test]
fn exact_history_retains_undo_transition_even_when_final_checkpoint_matches_baseline() {
    let (artifact, submission) = fixture();
    submission.validate().unwrap();
    let restored = ReplayArtifact::decode(&serde_json::to_vec(&artifact).unwrap(), &submission).unwrap();
    assert_eq!(restored.steps.len(), 3);
    assert_eq!(restored.steps[0].checkpoint, restored.steps[2].checkpoint);
    assert_ne!(restored.steps[1].checkpoint, restored.steps[2].checkpoint);
    assert_eq!(restored.steps[2].kind, "undo");
    assert_eq!(restored.checkpoints().collect::<Vec<_>>(), vec![asset(1), asset(2), asset(1)]);
}

#[test]
fn replay_rejects_reordered_duplicate_missing_or_unbound_steps() {
    let (artifact, submission) = fixture();
    let source = serde_json::to_value(&artifact).unwrap();
    for (field, value) in [
        ("revision", json!(4)),
        ("elapsed_ms", json!(300)),
        ("event", json!(artifact.steps[0].event)),
        ("kind", json!("external_script")),
        ("command", json!("")),
        ("checkpoint", json!(checkpoint(90))),
    ] {
        let mut bad = source.clone();
        bad["steps"][1][field] = value;
        assert!(ReplayArtifact::decode(&serde_json::to_vec(&bad).unwrap(), &submission).is_err(), "Accepted bad {field}");
    }
    let mut missing = artifact.clone();
    missing.steps.remove(1);
    assert!(missing.validate(&submission).is_err());
    let mut altered = submission.clone();
    altered.project = ProjectId::new(&format!("{:032x}", 99)).unwrap();
    assert!(artifact.validate(&altered).is_err());
    altered = submission.clone();
    altered.checkpoint = checkpoint(2);
    assert!(artifact.validate(&altered).is_err());
    altered = submission;
    altered.replay.end_event = Some(EventId::new(&format!("{:032x}", 21)).unwrap());
    assert!(artifact.validate(&altered).is_err());
}

#[test]
fn replay_rejects_unknown_schema_fields_corrupt_and_oversized_bytes() {
    let (artifact, submission) = fixture();
    let mut value = serde_json::to_value(&artifact).unwrap();
    value["schema_version"] = json!(2);
    assert!(ReplayArtifact::decode(&serde_json::to_vec(&value).unwrap(), &submission).is_err());
    value["schema_version"] = json!(1);
    value["external_script"] = json!("never execute this");
    assert!(ReplayArtifact::decode(&serde_json::to_vec(&value).unwrap(), &submission).is_err());
    assert!(ReplayArtifact::decode(&[0xff], &submission).is_err());
    assert!(ReplayArtifact::decode(&vec![b' '; MAX_ASSET_BYTES as usize + 1], &submission).is_err());
}

#[test]
fn replay_validation_also_rejects_invalid_submission_aggregate() {
    let (artifact, mut submission) = fixture();
    submission.image.bytes = 0;
    assert!(artifact.validate(&submission).is_err(), "A valid timeline cannot authorize an invalid submission");
}

#[test]
fn project_requires_replay_manifest_dependency_and_old_optional_field_stays_readable() {
    let (_, submission) = fixture();
    let mut project = ProjectRecord {
        schema_version: 1,
        project_id: submission.project,
        owner: submission.author,
        device: DeviceId::new(&format!("{:032x}", 40)).unwrap(),
        device_sequence: 3,
        local_revision: 3,
        synced_revision: None,
        checkpoint: submission.checkpoint,
        replay: submission.replay,
        assets: submission.assets.clone(),
    };
    project.validate().unwrap();
    project.assets.retain(|a| Some(*a) != project.replay.manifest);
    assert!(project.validate().is_err(), "Project must retain its declared replay package");
    let mut old = serde_json::to_value(submission.replay).unwrap();
    old.as_object_mut().unwrap().remove("manifest");
    let old: ReplayRef = serde_json::from_value(old).unwrap();
    assert_eq!(old.manifest, None);
    old.validate().unwrap();
}

fn real_asset(bytes: &[u8]) -> AssetRef {
    use sha2::{Digest, Sha256};
    AssetRef { hash: AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap(), bytes: bytes.len() as u64 }
}

#[test]
fn checkpoint_pack_round_trips_more_than_256_unique_revisions() {
    let entries: Vec<_> = (0..300u32)
        .map(|n| {
            let bytes = n.to_le_bytes().to_vec();
            (real_asset(&bytes), bytes)
        })
        .collect();
    let bytes = pack_checkpoints(&entries).unwrap();
    let restored = unpack_checkpoints(&bytes).unwrap();
    assert_eq!(restored.len(), 300);
    for (expected, actual) in entries.iter().zip(restored) {
        assert_eq!(expected.0, actual.0);
        assert_eq!(expected.1, actual.1);
    }
    let (mut artifact, mut submission) = fixture();
    artifact.packs = vec![real_asset(&bytes)];
    artifact.steps = entries
        .iter()
        .enumerate()
        .map(|(index, (asset, _))| ReplayArtifactStep {
            event: EventId::new(&format!("{:032x}", index + 1)).unwrap(),
            revision: index as u64 + 1,
            elapsed_ms: index as u64 * 100,
            command: if index == 0 { "file.new" } else { "paint.stroke" }.into(),
            params: json!({}),
            kind: if index == 0 { "baseline" } else { "checkpoint" }.into(),
            checkpoint: CheckpointRef { id: CheckpointId::new(&format!("{:032x}", index + 1)).unwrap(), asset: *asset },
        })
        .collect();
    submission.revision = 300;
    submission.checkpoint = artifact.steps[299].checkpoint;
    submission.replay.baseline = artifact.steps[0].checkpoint;
    submission.replay.end_event = Some(artifact.steps[299].event);
    submission.replay.event_count = 299;
    submission.replay.duration_ms = 29_900;
    submission.assets =
        vec![submission.image, submission.checkpoint.asset, submission.replay.baseline.asset, submission.replay.manifest.unwrap(), artifact.packs[0]];
    artifact.validate(&submission).unwrap();
    assert_eq!(ReplayArtifact::decode(&serde_json::to_vec(&artifact).unwrap(), &submission).unwrap().steps.len(), 300);
}

#[test]
fn checkpoint_pack_rejects_hostile_count_lengths_hashes_and_trailing_data() {
    let data = b"checkpoint fixture".to_vec();
    let original = pack_checkpoints(&[(real_asset(&data), data)]).unwrap();
    for len in 0..original.len() {
        assert!(unpack_checkpoints(&original[..len]).is_err(), "Truncated length {len} accepted");
    }
    let mut bad = original.clone();
    bad[0] = b'X';
    assert!(unpack_checkpoints(&bad).is_err());
    bad = original.clone();
    bad[6..10].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(unpack_checkpoints(&bad).is_err());
    bad = original.clone();
    bad[74..82].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(unpack_checkpoints(&bad).is_err());
    bad = original.clone();
    *bad.last_mut().unwrap() ^= 1;
    assert!(unpack_checkpoints(&bad).is_err());
    bad = original;
    bad.push(0);
    assert!(unpack_checkpoints(&bad).is_err());
    assert!(pack_checkpoints(&[]).is_err());
    assert!(pack_checkpoints(&[(asset(1), b"wrong digest".to_vec())]).is_err());
}

#[test]
fn duplicate_checkpoint_pack_entries_and_manifest_packs_are_rejected() {
    let data = b"same checkpoint".to_vec();
    let entry = (real_asset(&data), data);
    assert!(pack_checkpoints(&[entry.clone(), entry.clone()]).is_err());
    // Forge duplicate framing independently, so the reader cannot rely on its own writer.
    let mut bytes = pack_checkpoints(&[entry]).unwrap();
    let encoded_entry = bytes[10..].to_vec();
    bytes[6..10].copy_from_slice(&2u32.to_le_bytes());
    bytes.extend_from_slice(&encoded_entry);
    assert!(unpack_checkpoints(&bytes).is_err());
    let (mut artifact, submission) = fixture();
    artifact.packs = vec![asset(4), asset(4)];
    assert!(artifact.validate(&submission).is_err());
}
