//! Public boundary tests: untrusted JSON is bounded before semantic validation.
use photocraft_party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, Error, EventEnvelope, EventId, GameId, ImageExtent, MAX_ASSET_BYTES, MAX_ASSETS,
    MAX_DEPENDENCIES, MAX_IMAGE_SIDE, MAX_MESSAGE_BYTES, MAX_NAME_BYTES, MAX_NOTE_BYTES, MAX_PROJECT_BYTES, MAX_REPLAY_EVENTS, MAX_REPLAY_MS, Mode,
    Participant, ParticipantId, ProjectId, ProjectRecord, ReplayId, ReplayRef, ReplayStep, RequestId, RoomId, SCHEMA_VERSION, Submission, SubmissionId,
    decode_bounded, validate_assets,
};
use serde_json::{Value, json};

fn id(value: usize) -> String {
    format!("{value:032x}")
}

fn asset(value: usize, bytes: u64) -> AssetRef {
    AssetRef { hash: AssetHash::new(&format!("{value:064x}")).unwrap(), bytes }
}

fn checkpoint(value: usize) -> CheckpointRef {
    CheckpointRef { id: CheckpointId::new(&id(value)).unwrap(), asset: asset(value, 4096) }
}

fn event() -> EventEnvelope {
    EventEnvelope {
        schema_version: SCHEMA_VERSION,
        event_id: EventId::new(&id(1)).unwrap(),
        actor_id: ParticipantId::new(&id(2)).unwrap(),
        device_id: DeviceId::new(&id(3)).unwrap(),
        device_sequence: 1,
        project_id: ProjectId::new(&id(4)).unwrap(),
        base_revision: 0,
        dependencies: Vec::new(),
        relative_ms: 0,
        step: ReplayStep::Resolved { operation_version: 1, payload: asset(5, 512), result: checkpoint(6) },
    }
}

fn submission() -> Submission {
    Submission {
        id: SubmissionId::new(&id(1)).unwrap(),
        assignment: AssignmentId { game: GameId::new(&id(2)).unwrap(), chain: 0, round: 0 },
        author: ParticipantId::new(&id(3)).unwrap(),
        project: ProjectId::new(&id(4)).unwrap(),
        revision: 1,
        image: asset(10, 4096),
        checkpoint: checkpoint(5),
        replay: ReplayRef {
            manifest: None,
            id: ReplayId::new(&id(6)).unwrap(),
            baseline: checkpoint(7),
            end_event: Some(EventId::new(&id(8)).unwrap()),
            event_count: 1,
            duration_ms: 20,
        },
        assets: vec![asset(5, 4096), asset(7, 4096), asset(9, 4096), asset(10, 4096)],
        note: "Follow the blue line".into(),
        seam: Some(asset(9, 4096)),
    }
}

macro_rules! check_id {
    ($test:ident, $kind:ty, $width:expr) => {
        #[test]
        fn $test() {
            let valid = format!("{:0width$x}", 10, width = $width);
            let typed = <$kind>::new(&valid).unwrap();
            assert_eq!(typed.as_str(), valid);
            assert_eq!(serde_json::from_value::<$kind>(json!(valid)).unwrap(), typed);
            for invalid in [
                String::new(),
                "0".repeat($width),
                "a".repeat($width - 1),
                "a".repeat($width + 1),
                "A".repeat($width),
                "g".repeat($width),
                format!("{}\n", "a".repeat($width - 1)),
                "é".repeat($width / 2),
            ] {
                assert!(<$kind>::new(&invalid).is_err(), "{}: {invalid:?}", stringify!($kind));
                assert!(serde_json::from_value::<$kind>(json!(invalid)).is_err());
            }
            for value in [Value::Null, json!(1), json!(true), json!([valid]), json!({"id": valid})] {
                assert!(serde_json::from_value::<$kind>(value).is_err());
            }
        }
    };
}

check_id!(participant_id_rejects_malformed_and_non_string_wire_values, ParticipantId, 32);
check_id!(device_id_rejects_malformed_and_non_string_wire_values, DeviceId, 32);
check_id!(project_id_rejects_malformed_and_non_string_wire_values, ProjectId, 32);
check_id!(room_id_rejects_malformed_and_non_string_wire_values, RoomId, 32);
check_id!(game_id_rejects_malformed_and_non_string_wire_values, GameId, 32);
check_id!(submission_id_rejects_malformed_and_non_string_wire_values, SubmissionId, 32);
check_id!(event_id_rejects_malformed_and_non_string_wire_values, EventId, 32);
check_id!(request_id_rejects_malformed_and_non_string_wire_values, RequestId, 32);
check_id!(checkpoint_id_rejects_malformed_and_non_string_wire_values, CheckpointId, 32);
check_id!(replay_id_rejects_malformed_and_non_string_wire_values, ReplayId, 32);
check_id!(asset_hash_rejects_malformed_and_non_string_wire_values, AssetHash, 64);

#[test]
fn identifiers_use_wire_context_and_reject_hash_id_width_confusion() {
    // Entity IDs intentionally share an untagged wire representation. Membership/ownership
    // checks must resolve the ID in its declared field; a display name is never an identity.
    let participant = ParticipantId::new(&id(1)).unwrap();
    let wire = serde_json::to_value(participant).unwrap();
    assert_eq!(serde_json::from_value::<ProjectId>(wire.clone()).unwrap().as_str(), participant.as_str());
    assert!(serde_json::from_value::<AssetHash>(wire).is_err());
    assert!(serde_json::from_value::<ProjectId>(serde_json::to_value(asset(1, 1).hash).unwrap()).is_err());
}

#[test]
fn mode_round_counts_cover_two_three_players_and_all_rotations() {
    for players in [2, 3] {
        for rotations in 1..=4 {
            assert_eq!(Mode::Animation { rotations }.rounds(players).unwrap(), rotations * players as u8);
        }
        assert_eq!(Mode::ExquisiteCorpse.rounds(players).unwrap(), 3);
    }
    for players in [0, 1, 4, usize::MAX] {
        assert!(Mode::Animation { rotations: 2 }.rounds(players).is_err());
        assert!(Mode::ExquisiteCorpse.rounds(players).is_err());
    }
    for rotations in [0, 5, u8::MAX] {
        assert!(Mode::Animation { rotations }.rounds(2).is_err());
    }
    for wire in [
        json!({"kind": "classic"}),
        json!({"kind": "animation", "rotations": "2"}),
        json!({"kind": "animation", "rotations": 256}),
        json!({"kind": "animation", "rotations": 2, "players": 8}),
    ] {
        assert!(serde_json::from_value::<Mode>(wire).is_err());
    }
}

#[test]
fn assignment_coordinates_are_bounded_before_game_specific_ownership() {
    let game = GameId::new(&id(1)).unwrap();
    assert!(AssignmentId { game, chain: 2, round: 11 }.validate().is_ok());
    for (chain, round) in [(3, 0), (0, 12), (u8::MAX, u8::MAX)] {
        assert!(AssignmentId { game, chain, round }.validate().is_err());
    }
    assert!(serde_json::from_value::<AssignmentId>(json!({"game": game, "chain": -1, "round": 0})).is_err());
    assert!(serde_json::from_value::<AssignmentId>(json!({"game": game, "chain": 0, "round": 0, "player": id(2)})).is_err());
}

#[test]
fn participant_labels_are_byte_bounded_and_cannot_be_control_only() {
    let mut participant =
        Participant { id: ParticipantId::new(&id(1)).unwrap(), device: DeviceId::new(&id(2)).unwrap(), display_name: "é".repeat(MAX_NAME_BYTES / 2) };
    participant.validate().unwrap();
    participant.display_name.push('a');
    assert_eq!(participant.validate(), Err(Error::Limit("text bytes")));
    for name in ["", " \t\n", "name\0", "name\u{1b}"] {
        participant.display_name = name.into();
        assert!(participant.validate().is_err());
    }
}

#[test]
fn asset_sizes_counts_totals_and_unique_hashes_have_closed_bounds() {
    assert!(asset(1, 1).validate().is_ok());
    assert!(asset(1, MAX_ASSET_BYTES).validate().is_ok());
    for bytes in [0, MAX_ASSET_BYTES + 1, u64::MAX] {
        assert!(asset(1, bytes).validate().is_err());
    }
    assert!(validate_assets(&[]).is_err());
    let mut assets: Vec<_> = (1..=MAX_ASSETS).map(|value| asset(value, 1)).collect();
    validate_assets(&assets).unwrap();
    assets.push(asset(MAX_ASSETS + 1, 1));
    assert_eq!(validate_assets(&assets), Err(Error::Limit("asset count")));
    let mut assets: Vec<_> = (1..=8).map(|value| asset(value, MAX_ASSET_BYTES)).collect();
    assert_eq!(assets.iter().map(|a| a.bytes).sum::<u64>(), MAX_PROJECT_BYTES);
    validate_assets(&assets).unwrap();
    assets.push(asset(9, 1));
    assert_eq!(validate_assets(&assets), Err(Error::Limit("project bytes")));
    assert_eq!(validate_assets(&[asset(1, 1), asset(1, 2)]), Err(Error::Invalid("duplicate asset")));
}

#[test]
fn event_roundtrip_supports_resolved_and_checkpoint_fallback_steps() {
    let mut value = event();
    for step in [value.step.clone(), ReplayStep::Checkpoint { result: checkpoint(1) }] {
        value.step = step;
        let wire = serde_json::to_vec(&value).unwrap();
        assert_eq!(EventEnvelope::decode(&wire).unwrap(), value);
    }
}

#[test]
fn event_ingress_checks_message_size_before_decoding() {
    let expected = event();
    let mut wire = serde_json::to_vec(&expected).unwrap();
    wire.resize(MAX_MESSAGE_BYTES, b' ');
    assert_eq!(EventEnvelope::decode(&wire).unwrap(), expected);
    wire.push(b' ');
    assert_eq!(EventEnvelope::decode(&wire), Err(Error::Limit("message bytes")));
    let oversized_non_json = vec![b'!'; MAX_MESSAGE_BYTES + 1];
    assert_eq!(EventEnvelope::decode(&oversized_non_json), Err(Error::Limit("message bytes")));
}

#[test]
fn event_ingress_rejects_unknown_schema_operation_and_fields() {
    let expected = event();
    for schema_version in [0, SCHEMA_VERSION + 1, u16::MAX] {
        let value = EventEnvelope { schema_version, ..expected.clone() };
        assert_eq!(EventEnvelope::decode(&serde_json::to_vec(&value).unwrap()), Err(Error::Version(schema_version)));
    }
    for operation_version in [0, 2, u16::MAX] {
        let mut value = expected.clone();
        value.step = ReplayStep::Resolved { operation_version, payload: asset(5, 512), result: checkpoint(6) };
        assert_eq!(value.validate(), Err(Error::Version(operation_version)));
    }
    let mut top = serde_json::to_value(&expected).unwrap();
    top["unrecognized"] = json!(true);
    assert!(EventEnvelope::decode(&serde_json::to_vec(&top).unwrap()).is_err());
    let mut nested = serde_json::to_value(&expected).unwrap();
    nested["step"]["path"] = json!("C:/private/document.pcraft");
    assert!(EventEnvelope::decode(&serde_json::to_vec(&nested).unwrap()).is_err());
    let mut asset_field = serde_json::to_value(&expected).unwrap();
    asset_field["step"]["payload"]["path"] = json!("untrusted");
    assert!(EventEnvelope::decode(&serde_json::to_vec(&asset_field).unwrap()).is_err());
}

#[test]
fn malformed_events_never_become_valid_records() {
    for wire in [b"".as_slice(), b"null", b"[]", b"{}", b"{\"schema_version\":", b"\xff"] {
        assert!(EventEnvelope::decode(wire).is_err());
    }
    let mut wrong_type = serde_json::to_value(event()).unwrap();
    wrong_type["device_sequence"] = json!("1");
    assert!(EventEnvelope::decode(&serde_json::to_vec(&wrong_type).unwrap()).is_err());
    let mut duplicate_field = serde_json::to_string(&event()).unwrap();
    duplicate_field.insert_str(1, "\"schema_version\":1,");
    assert!(EventEnvelope::decode(duplicate_field.as_bytes()).is_err());
}

#[test]
fn event_sequences_dependencies_and_relative_time_are_validated() {
    let mut value = event();
    value.device_sequence = 0;
    assert!(value.validate().is_err());
    value = event();
    value.base_revision = u64::MAX;
    assert!(value.validate().is_err());
    value = event();
    value.relative_ms = MAX_REPLAY_MS;
    value.dependencies = (2..=MAX_DEPENDENCIES + 1).map(|i| EventId::new(&id(i)).unwrap()).collect();
    value.validate().unwrap();
    value.dependencies.push(EventId::new(&id(MAX_DEPENDENCIES + 2)).unwrap());
    assert!(value.validate().is_err());
    value = event();
    value.relative_ms = MAX_REPLAY_MS + 1;
    assert!(value.validate().is_err());
    value = event();
    value.dependencies = vec![value.event_id];
    assert!(value.validate().is_err());
    let dependency = EventId::new(&id(2)).unwrap();
    value.dependencies = vec![dependency, dependency];
    assert!(value.validate().is_err());
}

#[test]
fn event_steps_require_bounded_payload_and_result_assets() {
    let mut value = event();
    value.step = ReplayStep::Resolved { operation_version: 1, payload: asset(5, 0), result: checkpoint(6) };
    assert!(value.validate().is_err());
    let mut result = checkpoint(6);
    result.asset.bytes = MAX_ASSET_BYTES + 1;
    value.step = ReplayStep::Checkpoint { result };
    assert!(value.validate().is_err());
}

#[test]
fn submission_requires_the_exact_final_baseline_and_seam_dependencies() {
    let valid = submission();
    valid.validate().unwrap();
    let decoded: Submission = decode_bounded(&serde_json::to_vec(&valid).unwrap()).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, valid);
    for missing in [valid.image, valid.checkpoint.asset, valid.replay.baseline.asset, valid.seam.unwrap()] {
        let mut value = valid.clone();
        value.assets.retain(|a| a.hash != missing.hash);
        assert_eq!(value.validate(), Err(Error::Invalid("missing asset dependency")));
    }
    let mut mismatched_length = valid.clone();
    mismatched_length.checkpoint.asset.bytes += 1;
    assert_eq!(mismatched_length.validate(), Err(Error::Invalid("missing asset dependency")));
    let mut unsaved = valid;
    unsaved.revision = 0;
    assert!(unsaved.validate().is_err());
}

#[test]
fn seam_reference_cannot_alias_the_full_final_or_baseline_checkpoint() {
    let mut value = submission();
    for seam in [value.image, value.checkpoint.asset, value.replay.baseline.asset] {
        value.seam = Some(seam);
        assert_eq!(value.validate(), Err(Error::Invalid("seam must be a separate derived asset")));
    }
    value.seam = None;
    value.validate().unwrap();
}

#[test]
fn rendered_reference_cannot_grant_editable_checkpoint_access() {
    let mut value = submission();
    for editable in [value.checkpoint.asset, value.replay.baseline.asset] {
        value.image = editable;
        // Both hashes are complete, correctly sized manifest dependencies. The image grant
        // must still refuse them because an animation reference exposes only rendered pixels.
        assert!(value.assets.contains(&editable));
        assert!(value.validate().is_err());
    }
}

#[test]
fn replay_endpoint_and_bounds_include_legacy_baseline_without_forged_events() {
    let mut replay = submission().replay;
    replay.event_count = 0;
    assert!(replay.validate().is_err());
    replay.end_event = None;
    replay.duration_ms = 0;
    replay.validate().unwrap();
    replay.event_count = 1;
    assert!(replay.validate().is_err());
    replay.end_event = Some(EventId::new(&id(1)).unwrap());
    replay.event_count = MAX_REPLAY_EVENTS;
    replay.duration_ms = MAX_REPLAY_MS;
    replay.validate().unwrap();
    replay.event_count += 1;
    assert!(replay.validate().is_err());
    replay.event_count = 1;
    replay.duration_ms += 1;
    assert!(replay.validate().is_err());
    replay.duration_ms = 0;
    replay.baseline.asset.bytes = 0;
    assert!(replay.validate().is_err());
}

#[test]
fn notes_are_optional_byte_bounded_metadata() {
    let mut value = submission();
    value.note.clear();
    value.validate().unwrap();
    value.note = "é".repeat(MAX_NOTE_BYTES / 2);
    value.validate().unwrap();
    value.note.push('a');
    assert_eq!(value.validate(), Err(Error::Limit("text bytes")));
    value.note = "line one\nline two\twith spacing".into();
    value.validate().unwrap();
    value.note.push('\0');
    assert!(value.validate().is_err());
}

#[test]
fn submission_wire_rejects_unknown_fields_and_wrong_types() {
    let mut value = serde_json::to_value(submission()).unwrap();
    value["accepted"] = json!(true);
    assert!(decode_bounded::<Submission>(&serde_json::to_vec(&value).unwrap()).is_err());
    value = serde_json::to_value(submission()).unwrap();
    value["revision"] = json!(-1);
    assert!(decode_bounded::<Submission>(&serde_json::to_vec(&value).unwrap()).is_err());
    value = serde_json::to_value(submission()).unwrap();
    value["assignment"]["game"] = json!({"participant": id(1)});
    assert!(decode_bounded::<Submission>(&serde_json::to_vec(&value).unwrap()).is_err());
}

#[test]
fn image_extent_bounds_preserve_runtime_pixel_depth_and_color_model() {
    let extent = ImageExtent { width: 1536, height: 1024 };
    // Gray8, RGBA8, RGBA16, RGBA32f and a wider channel model are all runtime choices.
    for bytes_per_pixel in [1, 4, 8, 16, 32] {
        assert_eq!(extent.decoded_bytes(bytes_per_pixel).unwrap(), 1536 * 1024 * bytes_per_pixel);
    }
    assert!(ImageExtent { width: MAX_IMAGE_SIDE, height: 1 }.decoded_bytes(1).is_ok());
    let largest = ImageExtent { width: 4096, height: 4096 };
    assert_eq!(largest.decoded_bytes(32).unwrap(), MAX_PROJECT_BYTES);
    assert!(largest.decoded_bytes(33).is_err());
    assert!(ImageExtent { width: 4096, height: 4097 }.decoded_bytes(1).is_err());
    for extent in [
        ImageExtent { width: 0, height: 1 },
        ImageExtent { width: 1, height: 0 },
        ImageExtent { width: MAX_IMAGE_SIDE + 1, height: 1 },
        ImageExtent { width: 1, height: MAX_IMAGE_SIDE + 1 },
        ImageExtent { width: u32::MAX, height: u32::MAX },
    ] {
        assert!(extent.decoded_bytes(4).is_err());
    }
    assert!(extent.decoded_bytes(0).is_err());
    assert!(extent.decoded_bytes(u64::MAX).is_err());
}

fn project() -> ProjectRecord {
    let submitted = submission();
    ProjectRecord {
        schema_version: SCHEMA_VERSION,
        project_id: submitted.project,
        owner: submitted.author,
        device: DeviceId::new(&id(2)).unwrap(),
        device_sequence: 7,
        local_revision: 3,
        synced_revision: None,
        checkpoint: submitted.checkpoint,
        replay: submitted.replay,
        assets: submitted.assets,
    }
}

#[test]
fn local_project_sync_ack_cannot_exceed_durable_revision() {
    let mut record = project();
    for synced in [None, Some(1), Some(record.local_revision)] {
        record.synced_revision = synced;
        let bytes = serde_json::to_vec(&record).unwrap();
        assert_eq!(ProjectRecord::decode(&bytes).unwrap(), record);
    }
    record.synced_revision = Some(record.local_revision + 1);
    assert!(record.validate().is_err());
    record = project();
    record.local_revision = 0;
    assert!(record.validate().is_err());
    record = project();
    record.device_sequence = 0;
    assert!(record.validate().is_err());
}

#[test]
fn project_ingress_validates_versions_dependencies_and_replay() {
    let valid = project();
    for required in [valid.checkpoint.asset, valid.replay.baseline.asset] {
        let mut record = valid.clone();
        record.assets.retain(|a| a.hash != required.hash);
        assert!(ProjectRecord::decode(&serde_json::to_vec(&record).unwrap()).is_err());
    }
    let mut record = valid.clone();
    record.checkpoint.asset.bytes += 1;
    assert!(record.validate().is_err());
    record = valid.clone();
    record.replay.event_count = 0;
    assert!(record.validate().is_err());
    record = valid;
    record.schema_version += 1;
    assert_eq!(ProjectRecord::decode(&serde_json::to_vec(&record).unwrap()), Err(Error::Version(SCHEMA_VERSION + 1)));
    assert_eq!(ProjectRecord::decode(&vec![b' '; MAX_MESSAGE_BYTES + 1]), Err(Error::Limit("message bytes")));
    let mut unknown = serde_json::to_value(project()).unwrap();
    unknown["saved"] = json!(true);
    assert!(ProjectRecord::decode(&serde_json::to_vec(&unknown).unwrap()).is_err());
}
