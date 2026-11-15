use photocraft_party::gallery::*;
use photocraft_party::replay_artifact::{ReplayArtifact, ReplayArtifactStep, pack_checkpoints};
use photocraft_party::room::{Assignment, MemberView, Phase, RevealedEntry, RoomView};
use photocraft_party::*;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;

fn participant(n: u8) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 20)).unwrap(),
        display_name: format!("Artist {n}"),
    }
}
fn room() -> RoomView {
    let authors = vec![participant(1), participant(2)];
    let game = GameId::new(&"a".repeat(32)).unwrap();
    let mut reveal = Vec::new();
    for chain in 0..2u8 {
        for round in 0..3u8 {
            reveal.push(RevealedEntry::Missing {
                assignment: Assignment {
                    id: AssignmentId { game, chain, round },
                    player: authors[(usize::from(chain) + usize::from(round)) % 2].id,
                    project: ProjectId::new(&format!("{:032x}", u16::from(chain) * 3 + u16::from(round) + 100)).unwrap(),
                },
                reason: "Host ended incomplete game".into(),
            });
        }
    }
    RoomView {
        id: RoomId::new(&"b".repeat(32)).unwrap(),
        revision: 10,
        epoch: 2,
        phase: Phase::Archived,
        round: 0,
        mode: Mode::ExquisiteCorpse,
        timer_ms: None,
        moderator: authors[0].id,
        members: authors.into_iter().map(|participant| MemberView { participant, connected: true, ready: true }).collect(),
        waiting: Vec::new(),
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    }
}
fn manifest() -> GalleryManifest {
    GalleryManifest::from_reveal(ProjectId::new(&"c".repeat(32)).unwrap(), &room(), participant(1).id, "2026-11-06T00:00:00.000Z".into()).unwrap()
}
fn asset(bytes: &[u8]) -> AssetRef {
    AssetRef { hash: AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap(), bytes: bytes.len() as u64 }
}
fn step(n: u8, bytes: &[u8]) -> ReplayArtifactStep {
    ReplayArtifactStep {
        event: EventId::new(&format!("{n:032x}")).unwrap(),
        revision: u64::from(n),
        elapsed_ms: u64::from(n) * 10,
        command: "paint.stroke".into(),
        params: serde_json::json!({}),
        kind: if n == 1 { "baseline" } else { "checkpoint" }.into(),
        checkpoint: CheckpointRef { id: CheckpointId::new(&format!("{n:032x}")).unwrap(), asset: asset(bytes) },
    }
}

#[test]
fn terminal_frozen_roster_and_complete_schedule_are_required() {
    let original = room();
    let mut hidden = original.clone();
    hidden.phase = Phase::Drawing;
    assert!(GalleryManifest::from_reveal(ProjectId::new(&"c".repeat(32)).unwrap(), &hidden, participant(1).id, "now".into()).is_err());
    assert!(GalleryManifest::from_reveal(ProjectId::new(&"c".repeat(32)).unwrap(), &original, participant(3).id, "now".into()).is_err());
    let mut bundle = GalleryBundle { manifest: manifest(), objects: BTreeMap::new() };
    bundle.validate().unwrap();
    if let GalleryContent::Game { room } = &mut bundle.manifest.content {
        room.reveal.pop();
    }
    assert!(bundle.validate().is_err());
}

#[test]
fn notes_cannot_grant_missing_or_foreign_assignments() {
    let mut manifest = manifest();
    let assignment = AssignmentId { game: manifest.game().unwrap(), chain: 0, round: 0 };
    manifest.notes.push((assignment, "This slot was never submitted".into()));
    assert!(manifest.validate().is_err());
}

#[test]
fn checkpoint_packs_and_direct_intermediates_preserve_exact_closure() {
    let base = b"baseline";
    let middle = b"middle";
    let end = b"final";
    let pack = pack_checkpoints(&[(asset(middle), middle.to_vec())]).unwrap();
    let mut manifest = manifest();
    let replay = ReplayArtifact {
        schema_version: 1,
        replay: ReplayId::new(&"d".repeat(32)).unwrap(),
        project: manifest.id,
        packs: vec![asset(&pack)],
        steps: vec![step(1, base), step(2, middle), step(3, end)],
    };
    manifest.content = GalleryContent::Project { checkpoint: asset(end), direct_assets: Vec::new(), replay: Some(Box::new(replay)) };
    let mut bundle = GalleryBundle {
        manifest,
        objects: BTreeMap::from([(asset(base).hash, base.to_vec()), (asset(end).hash, end.to_vec()), (asset(&pack).hash, pack.clone())]),
    };
    bundle.validate().unwrap();
    if let GalleryContent::Project { replay: Some(replay), .. } = &mut bundle.manifest.content {
        replay.steps[1].checkpoint.asset = asset(b"different");
    }
    assert!(bundle.validate().is_err(), "A correctly checksummed pack cannot hide a missing checkpoint");
    if let GalleryContent::Project { replay: Some(replay), direct_assets, .. } = &mut bundle.manifest.content {
        replay.steps[1].checkpoint.asset = asset(middle);
        direct_assets.push(asset(middle));
    }
    bundle.objects.insert(asset(middle).hash, middle.to_vec());
    bundle.validate().unwrap();
    bundle.objects.insert(asset(base).hash, b"corrupt".to_vec());
    assert!(bundle.validate().is_err());
}

#[test]
fn portable_schema_and_unrevealed_source_remain_closed() {
    let mut invalid = manifest();
    invalid.schema_version = 999;
    assert!(invalid.validate().is_err());
    let mut manifest = manifest();
    manifest.provenance = Some(RemixProvenance {
        source_entry: manifest.id,
        source_game: manifest.game(),
        source_revision: 1,
        chain: 0,
        frame: None,
        authors: manifest.authors.clone(),
    });
    assert!(manifest.validate().is_err());
}
