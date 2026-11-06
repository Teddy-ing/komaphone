//! The only images requested for party presentation are those in the current projection.
//! Checkpoints, replay assets and old references are never speculative downloads.
use photocraft_party::room::{Phase, Reference, RevealedEntry, RoomView};
use photocraft_party::{AssetRef, Mode, ParticipantId};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;
pub const MAX_CACHE_BYTES: usize = 256 * 1024 * 1024;

pub fn images(view: &RoomView, actor: ParticipantId) -> Vec<AssetRef> {
    let players = view.members.len();
    let Ok(rounds) = view.mode.rounds(players) else {
        return Vec::new();
    };
    let mut members = BTreeSet::new();
    if view.members.iter().any(|member| !members.insert(member.participant.id)) || !members.contains(&actor) {
        return Vec::new();
    }
    let assignment_valid = |assignment: &photocraft_party::room::Assignment| {
        usize::from(assignment.id.chain) < players
            && assignment.id.round < rounds
            && view
                .members
                .get((usize::from(assignment.id.chain) + usize::from(assignment.id.round)) % players)
                .is_some_and(|member| member.participant.id == assignment.player)
    };
    let mut images = Vec::new();
    if matches!(view.phase, Phase::Drawing | Phase::Transfer)
        && let Some(assignment) =
            view.assignment.as_ref().filter(|a| a.assignment.player == actor && a.assignment.id.round == view.round && assignment_valid(&a.assignment))
    {
        let image = match (&view.mode, &assignment.reference, view.round) {
            (Mode::Animation { .. }, Some(Reference::Animation { image, .. }), 1..) => Some(*image),
            (Mode::ExquisiteCorpse, Some(Reference::Seam { image, width: 1024, height: 24 }), 1 | 2) => Some(*image),
            _ => None,
        };
        if let Some(image) = image.filter(valid_image) {
            images.push(image);
        }
    }
    if matches!(view.phase, Phase::Reveal | Phase::Archived) {
        if view.reveal.len() > players.saturating_mul(usize::from(rounds)) {
            return Vec::new();
        }
        let mut assignments = BTreeSet::new();
        let mut assets = BTreeMap::new();
        let mut game = None;
        for entry in &view.reveal {
            let assignment = match entry {
                RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment,
            };
            if !assignment_valid(assignment) || !assignments.insert(assignment.id) || game.is_some_and(|id| id != assignment.id.game) {
                return Vec::new();
            }
            game = Some(assignment.id.game);
            if let RevealedEntry::Artwork { image, .. } = entry {
                if !valid_image(image) || assets.get(&image.hash).is_some_and(|bytes| *bytes != image.bytes) {
                    return Vec::new();
                }
                if assets.insert(image.hash, image.bytes).is_none() {
                    images.push(*image);
                }
            }
        }
    }
    images
}

fn valid_image(image: &AssetRef) -> bool {
    image.bytes > 0 && image.bytes <= MAX_IMAGE_BYTES
}
