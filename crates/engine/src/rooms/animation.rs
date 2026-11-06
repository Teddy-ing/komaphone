//! Animation-specific presentation and copy boundaries. Accepted frames are read-only
//! assets; an explicit copy creates a new layer in the current assigned document.
use crate::{EngineError, Result, Session};
#[cfg(not(target_arch = "wasm32"))]
use image::{ImageDecoder, ImageReader};
#[cfg(not(target_arch = "wasm32"))]
use photocraft_doc::{ColorMode, Document, Layer, SampleType};
use photocraft_doc::{LayerId, Size};
use photocraft_party::room::{Phase, Reference, RevealedEntry, RoomView};
use photocraft_party::{AssetRef, Mode, ParticipantId};
#[cfg(not(target_arch = "wasm32"))]
use sha2::{Digest, Sha256};
#[cfg(not(target_arch = "wasm32"))]
use std::io::Cursor;

const WIDTH: u32 = 1536;
const HEIGHT: u32 = 1024;

/// Only the current assignee may use the immediately preceding frame supplied by
/// the service projection. The first round and a skipped predecessor have no image.
pub fn previous_reference(room: &RoomView, actor: ParticipantId) -> Result<Option<(&AssetRef, &str)>> {
    if !matches!(room.mode, Mode::Animation { .. }) || !matches!(room.phase, Phase::Drawing | Phase::Transfer) {
        return Err(EngineError::Other("Previous frames are available during Animation rounds".into()));
    }
    let assignment = room.assignment.as_ref().ok_or_else(|| EngineError::Other("No Animation assignment is available".into()))?;
    let rounds = room.mode.rounds(room.members.len()).map_err(|e| EngineError::Other(e.to_string()))?;
    if assignment.assignment.id.round != room.round || room.round >= rounds || usize::from(assignment.assignment.id.chain) >= room.members.len() {
        return Err(EngineError::Other("The Animation reference belongs to a different round".into()));
    }
    if assignment.assignment.player != actor || !room.members.iter().any(|m| m.participant.id == actor) {
        return Err(EngineError::Other("This Animation assignment belongs to another participant".into()));
    }
    match &assignment.reference {
        Some(Reference::Animation { image, note }) if assignment.assignment.id.round > 0 => {
            image.validate().map_err(|e| EngineError::Other(e.to_string()))?;
            photocraft_party::validate_label(note, photocraft_party::MAX_NOTE_BYTES, true).map_err(|e| EngineError::Other(e.to_string()))?;
            Ok(Some((image, note)))
        }
        None | Some(Reference::Missing) => Ok(None),
        _ => Err(EngineError::Other("The Animation reference does not match this assignment".into())),
    }
}

pub fn can_copy_previous(session: &Session) -> std::result::Result<(), String> {
    let room = session.rooms.status.room.as_ref().ok_or("Join an Animation room first")?;
    let actor = session.rooms.status.profile.as_ref().ok_or("Connect your profile first")?.id;
    if previous_reference(room, actor).map_err(|e| e.to_string())?.is_none() {
        return Err("There is no previous submitted frame to copy".into());
    }
    let assignment = room.assignment.as_ref().ok_or("No current assignment")?;
    if room.phase != Phase::Drawing
        || assignment.submitted
        || session.rooms.submission_project() == Some(assignment.assignment.project)
        || session.local_projects.preparing_room
        || session.local_projects.frozen_assignment == Some(assignment.assignment.id)
        || session
            .rooms
            .status
            .freeze_pin
            .as_ref()
            .is_some_and(|pin| pin.assignment == assignment.assignment.id && pin.project == assignment.assignment.project)
    {
        return Err("This drawing is frozen for submission".into());
    }
    let active = session.active().ok_or("Open the assigned drawing first")?;
    if session.local_projects.status(active.doc.id).project != Some(assignment.assignment.project) {
        return Err("Open the assigned drawing first".into());
    }
    if active.doc.size != (Size { width: WIDTH, height: HEIGHT }) {
        return Err("Animation frames must remain 1536 × 1024 pixels".into());
    }
    if !crate::layer_copy_cmds::layered(active.doc.mode) {
        return Err("Copy previous frame requires RGB, grayscale, CMYK or Lab color".into());
    }
    Ok(())
}

/// The caller obtains `png` from its authorized asset cache. Digest and length are
/// rechecked here so a stale response or caller-supplied image cannot become a copy.
/// The returned layer has an independent identity, color-managed pixels and source
/// metadata. Notes/reference overlays never become document pixels.
#[cfg(not(target_arch = "wasm32"))]
pub fn copy_previous_frame(session: &mut Session, png: &[u8]) -> Result<LayerId> {
    can_copy_previous(session).map_err(EngineError::Other)?;
    let room = session.rooms.status.room.as_ref().ok_or_else(|| EngineError::Other("No room".into()))?;
    let actor = session.rooms.status.profile.as_ref().ok_or_else(|| EngineError::Other("No profile".into()))?.id;
    let (asset, _) = previous_reference(room, actor)?.ok_or_else(|| EngineError::Other("No previous frame".into()))?;
    asset.validate().map_err(|e| EngineError::Other(e.to_string()))?;
    if png.len() as u64 != asset.bytes || format!("{:x}", Sha256::digest(png)) != asset.hash.as_str() {
        return Err(EngineError::Other("Previous-frame bytes do not match the authorized image; refresh the room".into()));
    }
    let assignment = room.assignment.as_ref().ok_or_else(|| EngineError::Other("No assignment".into()))?.assignment.id;
    let previous_round = assignment.round.checked_sub(1).ok_or_else(|| EngineError::Other("The first round has no previous frame".into()))?;
    let member_count = room.members.len();
    if !(2..=3).contains(&member_count) {
        return Err(EngineError::Other("Animation requires two or three participants".into()));
    }
    let author = room
        .members
        .get((usize::from(assignment.chain) + usize::from(previous_round)) % member_count)
        .ok_or_else(|| EngineError::Other("The previous-frame author is unavailable".into()))?
        .participant
        .id;
    let source = format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\"><rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\"><rdf:Description xmlns:komaphone=\"urn:komaphone:source:1\" komaphone:game=\"{}\" komaphone:chain=\"{}\" komaphone:round=\"{}\" komaphone:author=\"{}\" komaphone:image=\"{}\"/></rdf:RDF></x:xmpmeta>",
        assignment.game.as_str(),
        assignment.chain,
        previous_round,
        author.as_str(),
        asset.hash.as_str()
    );
    let mut reader = ImageReader::with_format(Cursor::new(png), image::ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(WIDTH);
    limits.max_image_height = Some(HEIGHT);
    limits.max_alloc = Some(u64::from(WIDTH) * u64::from(HEIGHT) * 8);
    reader.limits(limits);
    let decoder = reader.into_decoder().map_err(|e| EngineError::Other(format!("Cannot read the previous frame: {e}")))?;
    if decoder.dimensions() != (WIDTH, HEIGHT) || decoder.color_type() != image::ColorType::Rgba8 {
        return Err(EngineError::Other("The previous frame must be a canonical 1536 × 1024 RGBA PNG".into()));
    }
    let image = image::DynamicImage::from_decoder(decoder).map_err(|e| EngineError::Other(format!("Cannot decode the previous frame: {e}")))?.into_rgba8();
    let mut copies = Document::new("Previous frame", Size { width: WIDTH, height: HEIGHT }, ColorMode::Rgb, SampleType::U8);
    copies.icc_profile = Some(photocraft_cms::Builtin::Srgb.profile().to_bytes());
    let mut layer = Layer::raster("Previous frame copy", copies.pixel_format());
    let surface = crate::pixels_mut(&mut layer)?;
    // One bounded row at a time avoids a second full-frame float allocation.
    let mut row = Vec::with_capacity(WIDTH as usize * 4);
    for (y, pixels) in image.rows().enumerate() {
        row.clear();
        for pixel in pixels {
            row.extend(pixel.0.iter().map(|sample| f32::from(*sample) / 255.0));
        }
        let y = i32::try_from(y).map_err(|_| EngineError::Other("Previous frame row exceeds the canvas".into()))?;
        surface
            .try_write_region(photocraft_geom::Rect::new(0, y, WIDTH as i32, y + 1), &row)
            .map_err(|e| EngineError::Other(format!("Cannot allocate the previous-frame copy: {e}")))?;
    }
    layer.xmp = Some(source.into());
    copies.layers.push(layer);
    let active = session.active().ok_or(EngineError::NoDocument)?;
    // A fresh assignment's white baseline is a convenience, not artwork to bake
    // behind transparent copied pixels. Retain it hidden/undoable on the first copy.
    // Any completed or pending edit makes all existing layers stay untouched.
    let pristine_background =
        if session.local_document_saved(active.doc.id) && session.local_projects.status(active.doc.id).local_revision == 1 && active.doc.layers.len() == 1 {
            active.doc.layers.first().filter(|layer| active.doc.is_background(layer.id)).map(|layer| layer.id)
        } else {
            None
        };
    crate::layer_copy_cmds::adapt_copies(session, &mut copies, &active.doc)?;
    let layer = copies.layers.pop().ok_or_else(|| EngineError::Other("The previous frame contains no editable pixels".into()))?;
    session.edit("Copy previous frame", move |document, active| {
        if let Some(background) = pristine_background.and_then(|id| document.layer_mut(id)) {
            background.visible = false;
        }
        let id = crate::layer_place::insert_new(document, *active, layer, crate::layer_place::Place::AutoEditable);
        *active = Some(id);
        document.selection = None;
        Ok(id)
    })
}

#[cfg(target_arch = "wasm32")]
pub fn copy_previous_frame(_session: &mut Session, _png: &[u8]) -> Result<LayerId> {
    Err(EngineError::Other("Private-room frame copies require the Windows app".into()))
}

/// Ordered, phase-authorized chain entries, including explicit missing frames.
pub fn reveal_chain(room: &RoomView, chain: u8) -> Result<Vec<&RevealedEntry>> {
    if !matches!(room.mode, Mode::Animation { .. }) || !matches!(room.phase, Phase::Reveal | Phase::Archived) {
        return Err(EngineError::Other("Animation chains are available after reveal".into()));
    }
    let rounds = room.mode.rounds(room.members.len()).map_err(|e| EngineError::Other(e.to_string()))?;
    if usize::from(chain) >= room.members.len() {
        return Err(EngineError::Other("Choose an existing Animation chain".into()));
    }
    let mut entries: Vec<_> = room
        .reveal
        .iter()
        .filter(|entry| match entry {
            RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment.id.chain == chain,
        })
        .collect();
    entries.sort_by_key(|entry| match entry {
        RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment.id.round,
    });
    if entries.len() != usize::from(rounds) {
        return Err(EngineError::Other("The chain is incomplete; refresh the room to retrieve its labeled frames".into()));
    }
    let mut game = None;
    for (round, entry) in entries.iter().enumerate() {
        let assignment = match entry {
            RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment,
        };
        let expected_author = room.members.get((usize::from(chain) + round) % room.members.len()).map(|m| m.participant.id);
        if usize::from(assignment.id.round) != round || expected_author != Some(assignment.player) || game.is_some_and(|id| id != assignment.id.game) {
            return Err(EngineError::Other("The revealed Animation order or author does not match its assignments".into()));
        }
        game = Some(assignment.id.game);
    }
    Ok(entries)
}
