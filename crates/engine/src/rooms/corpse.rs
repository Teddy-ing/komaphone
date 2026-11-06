//! Exquisite Corpse presentation boundaries. A joining strip is a read-only derivative;
//! it is never one of the three output bands or a source of editable document pixels.
use super::media::RoomImage;
use crate::{EngineError, Result};
use photocraft_party::room::{Phase, Reference, RevealedEntry, RoomView};
use photocraft_party::{AssetRef, AssignmentId, GameId, Mode, ParticipantId};

pub const WIDTH: u32 = 1024;
pub const BAND_HEIGHT: u32 = 512;
pub const SEAM_HEIGHT: u32 = 24;
pub const HEIGHT: u32 = BAND_HEIGHT * 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SeamReference {
    Blank,
    Missing,
    Image(AssetRef),
}

/// Return only the derivative granted to the current assignment. In particular this
/// function never searches revealed entries or a locally retained prior draft.
pub fn seam_reference(room: &RoomView, actor: ParticipantId) -> Result<SeamReference> {
    if room.mode != Mode::ExquisiteCorpse || !matches!(room.phase, Phase::Drawing | Phase::Transfer) {
        return Err(invalid("A joining strip is available only for an active creature assignment"));
    }
    let assigned = room.assignment.as_ref().ok_or_else(|| invalid("No creature assignment is available"))?;
    if assigned.assignment.player != actor || assigned.assignment.id.round >= 3 || assigned.assignment.id.round != room.round {
        return Err(invalid("The joining strip does not belong to your current assignment"));
    }
    match (&assigned.reference, assigned.assignment.id.round) {
        (None, 0) => Ok(SeamReference::Blank),
        (Some(Reference::Missing), 1 | 2) => Ok(SeamReference::Missing),
        (Some(Reference::Seam { image, width: WIDTH, height: SEAM_HEIGHT }), 1 | 2) => {
            image.validate().map_err(|e| invalid(e.to_string()))?;
            Ok(SeamReference::Image(*image))
        }
        _ => Err(invalid("The service did not provide a valid 1024 × 24 joining strip")),
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorpsePart {
    pub assignment: AssignmentId,
    pub author: ParticipantId,
    pub author_name: String,
    pub image: Option<AssetRef>,
    /// A service-labeled missing submission, displayed separately from image pixels.
    pub missing: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CorpseChain {
    pub game: GameId,
    pub chain: u8,
    /// Head, torso/arms, legs/feet. The order never depends on payload arrival order.
    pub parts: [CorpsePart; 3],
}

pub fn part_name(round: u8) -> &'static str {
    match round {
        0 => "Head",
        1 => "Torso and arms",
        2 => "Legs and feet",
        _ => "Unknown part",
    }
}

/// Build a bounded reveal from the service-authorized projection. A missing entry is
/// not fabricated: only the authority's explicit placeholder can occupy a missing band.
pub fn reveal_chain(room: &RoomView, chain: u8) -> Result<CorpseChain> {
    if room.mode != Mode::ExquisiteCorpse || !matches!(room.phase, Phase::Reveal | Phase::Archived) {
        return Err(invalid("Creatures are available only after the game is revealed"));
    }
    let players = room.members.len();
    if !(2..=3).contains(&players) || usize::from(chain) >= players || room.reveal.len() > 9 {
        return Err(invalid("Invalid creature roster or chain"));
    }
    let mut parts: [Option<CorpsePart>; 3] = [None, None, None];
    let mut game = None;
    for entry in &room.reveal {
        let (assignment, image, missing) = match entry {
            RevealedEntry::Artwork { assignment, image, .. } => (assignment, Some(*image), None),
            RevealedEntry::Missing { assignment, reason } => (assignment, None, Some(reason.clone())),
        };
        if assignment.id.chain != chain {
            continue;
        }
        if game.is_some_and(|id| id != assignment.id.game) {
            return Err(invalid("Creature parts belong to different games"));
        }
        game = Some(assignment.id.game);
        let round = usize::from(assignment.id.round);
        let slot = parts.get_mut(round).ok_or_else(|| invalid("A creature has exactly three parts"))?;
        if slot.is_some() {
            return Err(invalid("A creature part appears twice in the reveal"));
        }
        let author_index = (usize::from(chain) + round) % players;
        let author = room.members.get(author_index).ok_or_else(|| invalid("A creature author is missing"))?;
        if author.participant.id != assignment.player {
            return Err(invalid("Creature authors do not match the frozen rotation"));
        }
        if let Some(image) = image {
            image.validate().map_err(|e| invalid(e.to_string()))?;
        }
        *slot = Some(CorpsePart { assignment: assignment.id, author: assignment.player, author_name: author.participant.display_name.clone(), image, missing });
    }
    let [head, torso, legs] = parts;
    Ok(CorpseChain {
        game: game.ok_or_else(|| invalid("This creature has not been revealed"))?,
        chain,
        parts: [
            head.ok_or_else(|| invalid("The revealed head is missing from the service response"))?,
            torso.ok_or_else(|| invalid("The revealed torso is missing from the service response"))?,
            legs.ok_or_else(|| invalid("The revealed legs are missing from the service response"))?,
        ],
    })
}

/// Concatenate already color-managed service derivatives. Alpha is unchanged and a
/// labeled missing part is transparent. Notes, labels and joining strips have no input.
pub fn assemble(parts: [Option<&RoomImage>; 3]) -> Result<RoomImage> {
    for image in parts.iter().flatten() {
        if image.width != WIDTH || image.height != BAND_HEIGHT {
            return Err(invalid("Each revealed creature part must be exactly 1024 × 512 pixels"));
        }
    }
    Ok(RoomImage { width: WIDTH, height: HEIGHT, rgba: assemble_samples(parts.map(|image| image.map(|image| image.rgba.as_slice())), 4)? })
}

/// The copying operation is independent of sample depth and color model. Its fixed
/// geometry bounds allocation; callers supply channel-interleaved samples unchanged.
pub fn assemble_samples<T: Clone + Default>(parts: [Option<&[T]>; 3], channels: usize) -> Result<Vec<T>> {
    if !(1..=16).contains(&channels) || std::mem::size_of::<T>() > 16 {
        return Err(invalid("Unsupported creature sample layout"));
    }
    let band = usize::try_from(WIDTH)
        .ok()
        .and_then(|width| usize::try_from(BAND_HEIGHT).ok().and_then(|height| width.checked_mul(height)))
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| invalid("Creature image size overflow"))?;
    if parts.iter().flatten().any(|samples| samples.len() != band) {
        return Err(invalid("Creature sample count does not match its band dimensions"));
    }
    let total = band.checked_mul(3).ok_or_else(|| invalid("Creature image size overflow"))?;
    let mut assembled = Vec::new();
    assembled.try_reserve_exact(total).map_err(|_| invalid("Not enough memory to assemble this creature"))?;
    for samples in parts {
        if let Some(samples) = samples {
            assembled.extend_from_slice(samples);
        } else {
            assembled.resize(assembled.len().saturating_add(band), T::default());
        }
    }
    Ok(assembled)
}

fn invalid(message: impl Into<String>) -> EngineError {
    EngineError::Other(message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_party::room::{Assignment, AssignmentView, MemberView};
    use photocraft_party::{AssetHash, DeviceId, Participant, ProjectId, RoomId};

    fn actor(index: u8) -> ParticipantId {
        ParticipantId::new(&format!("{:032x}", u16::from(index) + 1)).unwrap()
    }
    fn asset(index: u8) -> AssetRef {
        AssetRef { hash: AssetHash::new(&format!("{:064x}", u16::from(index) + 1)).unwrap(), bytes: 100 }
    }
    fn assignment(chain: u8, round: u8, players: u8) -> Assignment {
        Assignment {
            id: AssignmentId { game: GameId::new(&format!("{:032x}", 5)).unwrap(), chain, round },
            player: actor((chain + round) % players),
            project: ProjectId::new(&format!("{:032x}", 100 + u16::from(chain) * 3 + u16::from(round))).unwrap(),
        }
    }
    fn room(players: u8) -> RoomView {
        RoomView {
            id: RoomId::new(&format!("{:032x}", 1)).unwrap(),
            revision: 1,
            epoch: 1,
            phase: Phase::Reveal,
            round: 2,
            mode: Mode::ExquisiteCorpse,
            timer_ms: None,
            moderator: actor(0),
            members: (0..players)
                .map(|index| MemberView {
                    participant: Participant {
                        id: actor(index),
                        device: DeviceId::new(&format!("{:032x}", u16::from(index) + 1)).unwrap(),
                        display_name: format!("Player {}", index + 1),
                    },
                    connected: true,
                    ready: true,
                })
                .collect(),
            waiting: vec![],
            paused: None,
            deadline_ms: None,
            assignment: None,
            reveal: (0..players)
                .flat_map(|chain| {
                    (0..3).map(move |round| RevealedEntry::Missing { assignment: assignment(chain, round, players), reason: "Explicit skip".into() })
                })
                .collect(),
        }
    }

    #[test]
    fn assembly_preserves_boundary_rows_alpha_and_all_three_band_extents() {
        let mut head = RoomImage { width: WIDTH, height: BAND_HEIGHT, rgba: vec![11; (WIDTH * BAND_HEIGHT * 4) as usize] };
        let mut torso = head.clone();
        torso.rgba.fill(22);
        let mut legs = head.clone();
        legs.rgba.fill(33);
        let row = (WIDTH * 4) as usize;
        let head_end = head.rgba.len();
        head.rgba[head_end - row..].fill(101);
        torso.rgba[..row].fill(102);
        let torso_end = torso.rgba.len();
        torso.rgba[torso_end - row..].fill(103);
        legs.rgba[..row].fill(104);
        let result = assemble([Some(&head), Some(&torso), Some(&legs)]).unwrap();
        assert_eq!((result.width, result.height), (1024, 1536));
        assert_eq!(&result.rgba[..head_end], &head.rgba);
        assert_eq!(&result.rgba[head_end..head_end + torso_end], &torso.rgba);
        assert_eq!(&result.rgba[head_end + torso_end..], &legs.rgba);
        assert_eq!(result.rgba[(512 * 1024 * 4) - 1], 101);
        assert_eq!(result.rgba[512 * 1024 * 4], 102);
        assert_eq!(result.rgba[(1024 * 1024 * 4) - 1], 103);
        assert_eq!(result.rgba[1024 * 1024 * 4], 104);
    }

    #[test]
    fn assembly_does_not_quantize_u16_or_float_samples() {
        let band = (WIDTH * BAND_HEIGHT) as usize;
        let u16_band = vec![32_789_u16; band];
        let u16_result = assemble_samples([Some(u16_band.as_slice()), None, Some(u16_band.as_slice())], 1).unwrap();
        assert_eq!(u16_result.len(), band * 3);
        assert!(u16_result[..band].iter().all(|sample| *sample == 32_789));
        assert!(u16_result[band..band * 2].iter().all(|sample| *sample == 0));
        let float_band = vec![0.12345678_f32; band];
        let float_result = assemble_samples([Some(float_band.as_slice()), None, None], 1).unwrap();
        assert_eq!(&float_result[..band], float_band.as_slice());
    }

    #[test]
    fn assembly_rejects_strips_wrong_lengths_and_unbounded_channel_counts() {
        let strip = RoomImage { width: WIDTH, height: SEAM_HEIGHT, rgba: vec![0; (WIDTH * SEAM_HEIGHT * 4) as usize] };
        assert!(assemble([Some(&strip), None, None]).is_err());
        let truncated = RoomImage { width: WIDTH, height: BAND_HEIGHT, rgba: vec![0; 1] };
        assert!(assemble([Some(&truncated), None, None]).is_err());
        assert!(assemble_samples::<u8>([None, None, None], usize::MAX).is_err());
        assert!(assemble_samples::<u8>([None, None, None], 0).is_err());
    }

    #[test]
    fn two_and_three_player_reveals_sort_parts_and_keep_repeat_authorship() {
        for players in [2, 3] {
            let mut room = room(players);
            room.reveal.reverse();
            for chain in 0..players {
                let result = reveal_chain(&room, chain).unwrap();
                assert_eq!(result.parts.each_ref().map(|part| part.assignment.round), [0, 1, 2]);
                assert_eq!(result.parts[0].author == result.parts[2].author, players == 2);
                assert!(result.parts.iter().all(|part| part.missing.as_deref() == Some("Explicit skip")));
            }
        }
    }

    #[test]
    fn reveal_rejects_early_missing_duplicate_and_forged_author_entries() {
        let original = room(2);
        let mut early = original.clone();
        early.phase = Phase::Drawing;
        assert!(reveal_chain(&early, 0).is_err());
        let mut absent = original.clone();
        absent.reveal.remove(0);
        assert!(reveal_chain(&absent, 0).is_err());
        let mut duplicate = original.clone();
        duplicate.reveal.push(duplicate.reveal[0].clone());
        assert!(reveal_chain(&duplicate, 0).is_err());
        let mut forged = original;
        if let RevealedEntry::Missing { assignment, .. } = &mut forged.reveal[0] {
            assignment.player = actor(1);
        }
        assert!(reveal_chain(&forged, 0).is_err());
    }

    #[test]
    fn only_current_players_24_pixel_derivative_is_a_seam() {
        let mut room = room(2);
        room.phase = Phase::Drawing;
        room.round = 1;
        room.assignment = Some(AssignmentView {
            assignment: assignment(0, 1, 2),
            submitted: false,
            reference: Some(Reference::Seam { image: asset(1), width: WIDTH, height: SEAM_HEIGHT }),
        });
        assert_eq!(seam_reference(&room, actor(1)).unwrap(), SeamReference::Image(asset(1)));
        assert!(seam_reference(&room, actor(0)).is_err());
        room.assignment.as_mut().unwrap().reference = Some(Reference::Seam { image: asset(1), width: WIDTH, height: BAND_HEIGHT });
        assert!(seam_reference(&room, actor(1)).is_err());
        room.assignment.as_mut().unwrap().reference = Some(Reference::Animation { image: asset(1), note: "Hidden".into() });
        assert!(seam_reference(&room, actor(1)).is_err());
        room.assignment.as_mut().unwrap().reference = Some(Reference::Missing);
        assert_eq!(seam_reference(&room, actor(1)).unwrap(), SeamReference::Missing);
        room.phase = Phase::Archived;
        assert!(seam_reference(&room, actor(1)).is_err());
    }
}
