//! Exquisite Corpse presentation boundaries and per-segment height resizing.
//! A joining strip is a read-only derivative; it is never one of the three output
//! bands or a source of editable document pixels.
use super::media::RoomImage;
use crate::{EngineError, Result, Session};
use photocraft_doc::{Document, LayerContent};
use photocraft_geom::Rect;
use photocraft_party::room::{Phase, Reference, RevealedEntry, RoomView};
use photocraft_party::{AssetRef, AssignmentId, GameId, Mode, ParticipantId};
use serde_json::{Value, json};

pub const WIDTH: u32 = photocraft_party::CORPSE_WIDTH;
pub const BAND_HEIGHT: u32 = photocraft_party::CORPSE_DEFAULT_BAND_HEIGHT;
pub const MIN_BAND_HEIGHT: u32 = photocraft_party::CORPSE_MIN_BAND_HEIGHT;
pub const MAX_BAND_HEIGHT: u32 = photocraft_party::CORPSE_MAX_BAND_HEIGHT;
pub const MAX_TOTAL_HEIGHT: u32 = photocraft_party::CORPSE_MAX_TOTAL_HEIGHT;
pub const SEAM_HEIGHT: u32 = photocraft_party::CORPSE_SEAM_HEIGHT;
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
    pub height: u32,
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
impl CorpseChain {
    pub fn total_height(&self) -> u32 {
        self.parts.iter().fold(0u32, |sum, part| sum.saturating_add(part.height))
    }
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
        let (assignment, image, missing, height) = match entry {
            RevealedEntry::Artwork { assignment, image, .. } => {
                let extent = entry.resolved_extent(room.mode).map_err(|e| invalid(e.to_string()))?;
                (assignment, Some(*image), None, extent.height)
            }
            RevealedEntry::Missing { assignment, reason } => (assignment, None, Some(reason.clone()), BAND_HEIGHT),
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
        *slot = Some(CorpsePart {
            assignment: assignment.id,
            author: assignment.player,
            author_name: author.participant.display_name.clone(),
            height,
            image,
            missing,
        });
    }
    let [head, torso, legs] = parts;
    let chain = CorpseChain {
        game: game.ok_or_else(|| invalid("This creature has not been revealed"))?,
        chain,
        parts: [
            head.ok_or_else(|| invalid("The revealed head is missing from the service response"))?,
            torso.ok_or_else(|| invalid("The revealed torso is missing from the service response"))?,
            legs.ok_or_else(|| invalid("The revealed legs are missing from the service response"))?,
        ],
    };
    if chain.total_height() > MAX_TOTAL_HEIGHT {
        return Err(invalid("Assembled creature exceeds the maximum supported height"));
    }
    Ok(chain)
}

/// Concatenate already color-managed service derivatives using each part's accepted height.
/// Alpha is unchanged and a labeled missing part is transparent at the default band height.
/// Notes, labels and joining strips have no input.
pub fn assemble(parts: [Option<&RoomImage>; 3]) -> Result<RoomImage> {
    let mut total_height = 0u32;
    for image in &parts {
        let height = if let Some(image) = image {
            if image.width != WIDTH || !(MIN_BAND_HEIGHT..=MAX_BAND_HEIGHT).contains(&image.height) {
                return Err(invalid(format!("Each revealed creature part must be {WIDTH} px wide and {MIN_BAND_HEIGHT}–{MAX_BAND_HEIGHT} px tall")));
            }
            image.height
        } else {
            BAND_HEIGHT
        };
        total_height = total_height.checked_add(height).ok_or_else(|| invalid("Creature image size overflow"))?;
    }
    if total_height > MAX_TOTAL_HEIGHT {
        return Err(invalid("Assembled creature exceeds the maximum supported height"));
    }
    let rgba = assemble_samples_with_heights(parts.map(|image| image.map(|image| (image.height, image.rgba.as_slice()))), 4)?;
    Ok(RoomImage { width: WIDTH, height: total_height, rgba })
}

/// The copying operation is independent of sample depth and color model. Its bounded
/// geometry caps allocation; callers supply channel-interleaved samples unchanged.
pub fn assemble_samples<T: Clone + Default>(parts: [Option<&[T]>; 3], channels: usize) -> Result<Vec<T>> {
    assemble_samples_with_heights(parts.map(|part| part.map(|samples| (BAND_HEIGHT, samples))), channels)
}

/// Depth- and colour-general concatenation of three Exquisite Corpse parts with variable heights.
pub fn assemble_samples_with_heights<T: Clone + Default>(parts: [Option<(u32, &[T])>; 3], channels: usize) -> Result<Vec<T>> {
    if !(1..=16).contains(&channels) || std::mem::size_of::<T>() > 16 {
        return Err(invalid("Unsupported creature sample layout"));
    }
    let row_samples = usize::try_from(WIDTH).ok().and_then(|width| width.checked_mul(channels)).ok_or_else(|| invalid("Creature image size overflow"))?;
    let mut total_height = 0u32;
    let mut total_samples = 0usize;
    let mut band_counts = [0usize; 3];
    for (slot, part) in parts.iter().enumerate() {
        let height = match part {
            Some((height, samples)) => {
                if !(MIN_BAND_HEIGHT..=MAX_BAND_HEIGHT).contains(height) {
                    return Err(invalid(format!("Creature segment height must be {MIN_BAND_HEIGHT}–{MAX_BAND_HEIGHT} px")));
                }
                let expected = usize::try_from(*height).ok().and_then(|h| row_samples.checked_mul(h)).ok_or_else(|| invalid("Creature image size overflow"))?;
                if samples.len() != expected {
                    return Err(invalid("Creature sample count does not match its band dimensions"));
                }
                *height
            }
            None => BAND_HEIGHT,
        };
        total_height = total_height.checked_add(height).ok_or_else(|| invalid("Creature image size overflow"))?;
        let count = usize::try_from(height).ok().and_then(|h| row_samples.checked_mul(h)).ok_or_else(|| invalid("Creature image size overflow"))?;
        if let Some(target) = band_counts.get_mut(slot) {
            *target = count;
        }
        total_samples = total_samples.checked_add(count).ok_or_else(|| invalid("Creature image size overflow"))?;
    }
    if total_height > MAX_TOTAL_HEIGHT {
        return Err(invalid("Assembled creature exceeds the maximum supported height"));
    }
    let mut assembled = Vec::new();
    assembled.try_reserve_exact(total_samples).map_err(|_| invalid("Not enough memory to assemble this creature"))?;
    for (part, count) in parts.into_iter().zip(band_counts) {
        if let Some((_, samples)) = part {
            assembled.extend_from_slice(samples);
        } else {
            assembled.resize(assembled.len().saturating_add(count), T::default());
        }
    }
    Ok(assembled)
}

/// Lowest row (`y1`, in `0..=doc.size.height`) containing non-blank artwork on any layer,
/// independent of bit depth and colour model. Locked solid-white Background pixels are treated
/// as blank canvas; painted marks on the Background or any other layer extend this bound.
pub fn content_bottom(doc: &Document) -> u32 {
    let canvas = doc.bounds();
    if canvas.is_empty() {
        return 0;
    }
    let mut bottom = 0u32;
    let mut stack: Vec<(&photocraft_doc::Layer, u8)> = doc.layers.iter().map(|layer| (layer, 0)).collect();
    while let Some((layer, depth)) = stack.pop() {
        if depth > 64 {
            continue;
        }
        match &layer.content {
            LayerContent::Group(group) => {
                for child in &group.children {
                    stack.push((child, depth.saturating_add(1)));
                }
            }
            LayerContent::Raster(surface) if doc.is_background(layer.id) => {
                bottom = bottom.max(background_content_bottom(surface, canvas, bottom));
            }
            LayerContent::Raster(surface) => {
                let mut bounds = surface.content_bounds();
                if !bounds.is_empty() && photocraft_compose::effects::has_effects(layer) {
                    bounds = bounds.inflate(photocraft_compose::effects::margin(layer));
                }
                let clipped = bounds.intersect(&canvas);
                if !clipped.is_empty() && clipped.y1 > 0 {
                    bottom = bottom.max(clipped.y1 as u32);
                }
            }
            LayerContent::Adjustment(_) => {}
            LayerContent::Fill(_) | LayerContent::Text(_) | LayerContent::Shape(_) | LayerContent::Smart(_) => {
                let mut bounds = photocraft_compose::layer_bounds(layer, canvas);
                if !bounds.is_empty() && photocraft_compose::effects::has_effects(layer) {
                    bounds = bounds.inflate(photocraft_compose::effects::margin(layer));
                }
                let clipped = bounds.intersect(&canvas);
                if !clipped.is_empty() && clipped.y1 > 0 {
                    bottom = bottom.max(clipped.y1 as u32);
                }
            }
        }
        if bottom >= doc.size.height {
            return doc.size.height;
        }
    }
    bottom.min(doc.size.height)
}

fn background_content_bottom(surface: &photocraft_raster::Surface, canvas: Rect, current_bottom: u32) -> u32 {
    let fmt = surface.format();
    let bpp = fmt.bytes_per_pixel();
    if bpp == 0 {
        return current_bottom;
    }
    let white = photocraft_raster::from_rgba(&fmt, [1.0, 1.0, 1.0, 1.0]);
    let mut white_bytes = vec![0u8; bpp];
    photocraft_raster::encode_pixel(&fmt, &white, &mut white_bytes);
    let ts = photocraft_geom::TILE_SIZE as usize;
    let blank_row = white_bytes.repeat(ts);
    let row_stride = ts.saturating_mul(bpp);
    let mut bottom = current_bottom;
    for (&coord, tile) in surface.tiles() {
        let origin = coord.rect();
        let clipped = origin.intersect(&canvas);
        if clipped.is_empty() || clipped.y1 <= 0 || (clipped.y1 as u32) <= bottom {
            continue;
        }
        let Ok(bytes) = tile.try_bytes() else {
            return canvas.height();
        };
        let lx0 = ((clipped.x0 - origin.x0) as usize).saturating_mul(bpp);
        let lx1 = ((clipped.x1 - origin.x0) as usize).saturating_mul(bpp);
        let Some(blank_slice) = blank_row.get(lx0..lx1) else {
            continue;
        };
        let min_y = clipped.y0.max(bottom as i32);
        for y in (min_y..clipped.y1).rev() {
            let ly = (y - origin.y0) as usize;
            let row_start = ly.saturating_mul(row_stride);
            let Some(row_slice) = bytes.get(row_start.saturating_add(lx0)..row_start.saturating_add(lx1)) else {
                continue;
            };
            if row_slice != blank_slice {
                bottom = bottom.max((y + 1) as u32);
                break;
            }
        }
    }
    bottom
}

/// Minimum height the current Exquisite Corpse segment can shrink to without clipping existing
/// marks (`MIN_BAND_HEIGHT..=doc.size.height.max(MIN_BAND_HEIGHT)`).
pub fn safe_min_height(doc: &Document) -> u32 {
    content_bottom(doc).clamp(MIN_BAND_HEIGHT, doc.size.height.max(MIN_BAND_HEIGHT))
}

/// Whether the active document is the player's editable current Exquisite Corpse segment.
pub fn can_resize_segment(session: &Session) -> std::result::Result<(), String> {
    let room = session.rooms.status.room.as_ref().ok_or("Join an Exquisite Corpse room first")?;
    if room.mode != Mode::ExquisiteCorpse {
        return Err("Segment height resizing is only available in Exquisite Corpse".into());
    }
    if room.phase != Phase::Drawing {
        return Err("This Exquisite Corpse segment cannot be resized after drawing ends".into());
    }
    let actor = session.rooms.status.profile.as_ref().ok_or("Connect your profile first")?.id;
    let assignment = room.assignment.as_ref().ok_or("No active Exquisite Corpse assignment")?;
    if assignment.assignment.player != actor || assignment.assignment.id.round != room.round {
        return Err("This segment does not belong to your current assignment".into());
    }
    if assignment.submitted {
        return Err("Submitted segments are locked and cannot be resized".into());
    }
    let document = session.active().ok_or("Open your assigned Exquisite Corpse drawing first")?;
    if session.local_projects.status(document.doc.id).project != Some(assignment.assignment.project) {
        return Err("Open your current Exquisite Corpse segment to resize its height".into());
    }
    if let Some(reason) = session.room_active_edit_refusal() {
        return Err(reason);
    }
    if document.doc.size.width != WIDTH {
        return Err(format!("Exquisite Corpse segment width must remain {WIDTH} px"));
    }
    Ok(())
}

/// Resize only the current Exquisite Corpse segment's height while keeping the top/reference
/// seam anchored (`y = 0`) and artwork unscaled. Shrinking below `safe_min_height` is refused
/// unless `allow_crop` is explicitly true (still fully undoable).
pub fn resize_segment_height(session: &mut Session, height: u32, allow_crop: bool) -> Result<Value> {
    can_resize_segment(session).map_err(invalid)?;
    if !(MIN_BAND_HEIGHT..=MAX_BAND_HEIGHT).contains(&height) {
        return Err(invalid(format!("Segment height must be between {MIN_BAND_HEIGHT} and {MAX_BAND_HEIGHT} px")));
    }
    let (current_height, safe_min) = {
        let state = session.active().ok_or(EngineError::NoDocument)?;
        (state.doc.size.height, safe_min_height(&state.doc))
    };
    if height == current_height {
        return Ok(json!({"width": WIDTH, "height": height, "changed": false, "safeMinHeight": safe_min, "cropped": false}));
    }
    if height < safe_min && !allow_crop {
        return Err(invalid(format!("Cannot shrink below {safe_min} px without cropping existing artwork. Confirm crop to resize to {height} px (undoable).")));
    }
    let cropped = height < safe_min;
    let rect = Rect::from_xywh(0, 0, WIDTH, height);
    session.edit_corpse_segment_height("Resize Segment Height", |doc, _| {
        crate::image_cmds::crop_doc(doc, "komaphone.corpse.setSegmentHeight", rect, cropped, [1.0, 1.0, 1.0, 1.0])?;
        if !cropped
            && height < current_height
            && let Some(bg) = doc.background_mut()
            && let LayerContent::Raster(surface) = &mut bg.content
        {
            *surface = photocraft_algo::resample::crop_surface(surface, rect);
        }
        Ok(())
    })?;
    let next_safe = session.active().map_or(MIN_BAND_HEIGHT, |state| safe_min_height(&state.doc));
    Ok(json!({"width": WIDTH, "height": height, "changed": true, "safeMinHeight": next_safe, "cropped": cropped}))
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
