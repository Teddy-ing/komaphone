#![allow(dead_code)]
use std::collections::BTreeMap;

use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
use photocraft_engine::gallery_media::{encode_png, render_document};
use photocraft_party::gallery::{GALLERY_SCHEMA, GalleryBundle, GalleryContent, GalleryManifest};
use photocraft_party::replay_artifact::{ReplayArtifact, ReplayArtifactStep};
use photocraft_party::room::{Assignment, MemberView, Phase, RevealedEntry, RoomView};
use photocraft_party::{
    AssetHash, AssetRef, AssignmentId, CheckpointId, CheckpointRef, DeviceId, EventId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId,
    ReplayRef, RoomId,
};
use sha2::{Digest, Sha256};

pub fn project(value: u64) -> ProjectId {
    ProjectId::new(&format!("{value:032x}")).unwrap()
}
pub fn add(objects: &mut BTreeMap<AssetHash, Vec<u8>>, bytes: Vec<u8>) -> AssetRef {
    let asset = AssetRef { hash: AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap(), bytes: bytes.len() as u64 };
    objects.insert(asset.hash, bytes);
    asset
}
pub fn checkpoint(objects: &mut BTreeMap<AssetHash, Vec<u8>>, document: &Document) -> CheckpointRef {
    let asset = add(objects, photocraft_format::save_to_bytes(document, &Default::default()).unwrap());
    CheckpointRef { id: CheckpointId::new(&asset.hash.as_str()[..32]).unwrap(), asset }
}
pub fn participant(value: u64) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{value:032x}")).unwrap(),
        device: DeviceId::new(&format!("{value:032x}")).unwrap(),
        display_name: format!("Artist {value}"),
    }
}
pub fn manifest(content: GalleryContent) -> GalleryManifest {
    GalleryManifest {
        schema_version: GALLERY_SCHEMA,
        id: project(900),
        title: "Media fixture".into(),
        created_at: "2026-11-06T01:00:00Z".into(),
        updated_at: "2026-11-06T01:00:00Z".into(),
        authors: vec![participant(1)],
        content,
        provenance: None,
        notes: Vec::new(),
        fps: 6,
        looping: true,
    }
}
pub fn local(document: &Document) -> GalleryBundle {
    let mut objects = BTreeMap::new();
    let checkpoint = checkpoint(&mut objects, document).asset;
    GalleryBundle { manifest: manifest(GalleryContent::Project { checkpoint, replay: None, direct_assets: Vec::new() }), objects }
}
pub fn game(mode: Mode) -> GalleryBundle {
    let rounds = mode.rounds(2).unwrap();
    let mut objects = BTreeMap::new();
    let mut reveal = Vec::new();
    for chain in 0..2u8 {
        for round in 0..rounds {
            let color = match round % 3 {
                0 => Color::rgb(1., 0., 0.),
                1 => Color::rgb(0., 1., 0.),
                _ => Color::rgb(0., 0., 1.),
            };
            let size = if mode == Mode::ExquisiteCorpse { Size { width: 1024, height: 512 } } else { Size { width: 1536, height: 1024 } };
            let doc = Document::with_background("Accepted artwork", size, ColorMode::Rgb, SampleType::U8, color);
            let checkpoint = checkpoint(&mut objects, &doc);
            let image = add(&mut objects, encode_png(&render_document(&doc).unwrap()).unwrap());
            let project = project(100 + u64::from(chain) * 20 + u64::from(round));
            let artifact = ReplayArtifact {
                schema_version: 1,
                replay: ReplayId::new(project.as_str()).unwrap(),
                project,
                packs: Vec::new(),
                steps: vec![ReplayArtifactStep {
                    event: EventId::new(project.as_str()).unwrap(),
                    revision: 1,
                    elapsed_ms: 0,
                    command: "file.new".into(),
                    params: serde_json::json!({}),
                    kind: "baseline".into(),
                    checkpoint,
                }],
            };
            let manifest = add(&mut objects, serde_json::to_vec(&artifact).unwrap());
            let replay = ReplayRef { id: artifact.replay, manifest: Some(manifest), baseline: checkpoint, end_event: None, event_count: 0, duration_ms: 0 };
            let assignment = Assignment {
                id: AssignmentId { game: GameId::new(&format!("{:032x}", 10)).unwrap(), chain, round },
                player: participant(u64::from((chain + round) % 2) + 1).id,
                project,
            };
            reveal.push(RevealedEntry::Artwork {
                assignment,
                revision: 1,
                image,
                checkpoint,
                replay: Box::new(replay),
                assets: vec![checkpoint.asset, image, manifest],
            });
        }
    }
    let room = RoomView {
        id: RoomId::new(&format!("{:032x}", 20)).unwrap(),
        revision: 50,
        epoch: 1,
        phase: Phase::Reveal,
        round: rounds - 1,
        mode,
        timer_ms: None,
        moderator: participant(1).id,
        members: (1..=2).map(|n| MemberView { participant: participant(n), connected: true, ready: true }).collect(),
        waiting: Vec::new(),
        paused: None,
        deadline_ms: None,
        assignment: None,
        reveal,
    };
    let manifest = GalleryManifest::from_reveal(project(900), &room, participant(1).id, "2026-11-06T01:00:00Z".into()).unwrap();
    let bundle = GalleryBundle { manifest, objects };
    bundle.validate().unwrap();
    bundle
}
