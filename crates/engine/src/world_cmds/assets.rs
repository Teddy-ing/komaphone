use super::{
    CanvasId, CommandSpec, Deserialize, Edit, MediaId, RegionKey, Result, Session, Value, active, available, edit, empty, error, fresh, identity, json, opened,
    parse, pending, spec, wire,
};
use crate::world_assets::WorldExport;
use photocraft_scene::shared::media::{MediaSource, Provenance};
use photocraft_scene::shared::tours::ExportSettings;
use photocraft_scene::shared::{MediaPlacement, SourceId};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Export {
    format: String,
    path: String,
    tour: Option<photocraft_scene::shared::TourId>,
    width: Option<u32>,
    height: Option<u32>,
    fps: Option<u32>,
    looping: Option<bool>,
    elapsed_ms: Option<u64>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    room: SourceId,
    game: SourceId,
    chain: u8,
    frame: Option<u16>,
    fps: Option<u8>,
    looping: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Place {
    id: Option<MediaId>,
    anchor: RegionKey,
    offset: [f64; 2],
    width: f64,
    height: f64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Copy {
    media: MediaId,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Restore {
    path: String,
    canvas: Option<CanvasId>,
}
/// Restore accepts only a local archive path and an optional stable destination.
/// The worker and service validate portable contents independently. Historical
/// identities are attribution only; the new canvas belongs to the signed-in user.
pub fn parse_restore(params: &Value) -> Result<WorldExport> {
    let p: Restore = parse(params)?;
    if p.path.is_empty() || p.path.len() > 4096 || p.path.chars().any(char::is_control) {
        return Err(error("Invalid world archive source"));
    }
    let canvas = match p.canvas {
        Some(canvas) => canvas,
        None => CanvasId::new(&fresh()?).map_err(error)?,
    };
    Ok(WorldExport::Restore { path: p.path.into(), canvas })
}

/// Shared with the automation capability adapter: parsing never starts filesystem work.
pub fn parse_export(params: &Value) -> Result<WorldExport> {
    let p: Export = parse(params)?;
    if p.path.is_empty() || p.path.len() > 4096 || p.path.chars().any(char::is_control) {
        return Err(error("Invalid world export destination"));
    }
    let path = p.path.into();
    let width = p.width.unwrap_or(1280);
    let height = p.height.unwrap_or(720);
    let fps = p.fps.unwrap_or(12);
    if width == 0 || height == 0 || width > 2048 || height > 2048 || !(1..=24).contains(&fps) {
        return Err(error("World export requires sides1–2048 and fps1–24"));
    }
    match p.format.as_str() {
        "png" if p.tour.is_none() && p.fps.is_none() && p.looping.is_none() => {
            Ok(WorldExport::View { path, width, height, elapsed_ms: p.elapsed_ms.unwrap_or(0) })
        }
        "gif" if p.elapsed_ms.is_none() => Ok(WorldExport::TourGif {
            path,
            tour: p.tour.ok_or_else(|| error("Choose a saved tour for GIF export"))?,
            width,
            height,
            fps,
            looping: p.looping.unwrap_or(true),
        }),
        "sequence" if p.elapsed_ms.is_none() && p.looping.is_none() => {
            Ok(WorldExport::TourSequence { path, tour: p.tour.ok_or_else(|| error("Choose a saved tour for sequence export"))?, width, height, fps })
        }
        "archive" if p.tour.is_none() && p.width.is_none() && p.height.is_none() && p.fps.is_none() && p.looping.is_none() && p.elapsed_ms.is_none() => {
            Ok(WorldExport::Archive { path })
        }
        _ => Err(error("Export format must be png, gif, sequence or archive with matching options")),
    }
}
fn place(s: &mut Session, p: &Value, prepared: wire::PreparedMedia) -> Result<Value> {
    let p: Place = parse(p)?;
    let media = MediaPlacement {
        id: match p.id {
            Some(id) => id,
            None => MediaId::new(&fresh()?).map_err(error)?,
        },
        canvas: active(s)?.info.canvas,
        owner: identity(s)?.0,
        anchor: p.anchor,
        offset: p.offset,
        width: p.width,
        height: p.height,
        frames: prepared.frames,
        looping: prepared.looping,
        provenance: prepared.provenance,
    };
    media.validate().map_err(error)?;
    edit(s, Edit::PlaceMedia { media: Box::new(media) })
}
pub(super) fn specs() -> Vec<CommandSpec> {
    vec![
        spec("komaphone.world.restore", "Restore archive as a private canvas", r#"{"path":archive_directory,"canvas":new_canvas_id?}"#, available, |s, p| {
            identity(s)?;
            let action = parse_restore(p)?;
            let WorldExport::Restore { canvas, .. } = &action else {
                return Err(error("Invalid restore action"));
            };
            let canvas = *canvas;
            s.world_export(action)?;
            Ok(json!({"pending":true,"canvas":canvas,"restored":false}))
        }),
        spec(
            "komaphone.world.export",
            "Export the accepted shared canvas",
            r#"{"format":"png|gif|sequence|archive","path":string,"tour":tour_id?,"width":integer=1280,"height":integer=720,"fps":integer=12,"looping":bool=true,"elapsed_ms":integer=0}"#,
            opened,
            |s, p| {
                let action = parse_export(p)?;
                // Cheap bounds and route checks before handing expensive snapshot work to the worker.
                match &action {
                    WorldExport::TourGif { tour, width, height, fps, .. } | WorldExport::TourSequence { tour, width, height, fps, .. } => {
                        let route = active(s)?.canonical.tours.get(tour).ok_or_else(|| error("Wait for this tour to synchronize before exporting"))?;
                        ExportSettings { width: *width, height: *height, fps: *fps }.frame_count(route).map_err(error)?;
                    }
                    _ => {}
                }
                s.world_export(action)?;
                Ok(json!({"pending":true,"exported":false}))
            },
        ),
        spec("komaphone.world.export.cancel", "Cancel world export", "{}", available, |s, p| {
            empty(p)?;
            s.world_cancel_export()?;
            Ok(json!({"cancel_requested":true}))
        }),
        spec(
            "komaphone.world.remix.prepare",
            "Prepare authorized game media",
            r#"{"room":room_id,"game":game_id,"chain":integer,"frame":integer?,"fps":integer=6,"looping":bool=true}"#,
            opened,
            |s, p| {
                let p: Prepare = parse(p)?;
                let fps = p.fps.unwrap_or(6);
                if !(1..=24).contains(&fps) || p.chain >= 3 || p.frame.is_some_and(|n| n >= 12) {
                    return Err(error("Invalid game media selection"));
                }
                pending(
                    s,
                    wire::Action::PrepareGameMedia {
                        canvas: active(s)?.info.canvas,
                        source: wire::GameMediaSource { room: p.room, game: p.game, chain: p.chain, frame: p.frame, fps, looping: p.looping.unwrap_or(true) },
                    },
                )
            },
        ),
        spec(
            "komaphone.world.remix.place",
            "Place prepared animated media",
            r#"{"id":media_id?,"anchor":region,"offset":[x,y],"width":number,"height":number}"#,
            opened,
            |s, p| {
                let prepared = s.worlds.assets.prepared().cloned().ok_or_else(|| error("Prepare authorized media for this canvas first"))?;
                place(s, p, prepared)
            },
        ),
        spec("komaphone.world.remix.copy", "Copy shared media independently", r#"{"media":media_id}"#, opened, |s, p| {
            let p: Copy = parse(p)?;
            let active = active(s)?;
            let media = active.canonical.media.get(&p.media).ok_or_else(|| error("Wait for this media to synchronize before copying"))?.clone();
            let canvas = active.info.canvas;
            s.worlds.copy_source = Some((canvas, media));
            Ok(json!({"copied":true}))
        }),
        spec(
            "komaphone.world.remix.paste",
            "Paste an independent shared media copy",
            r#"{"id":media_id?,"anchor":region,"offset":[x,y],"width":number,"height":number}"#,
            opened,
            |s, p| {
                let (canvas, source) = s.worlds.copy_source.as_ref().ok_or_else(|| error("Copy shared media first"))?;
                let prepared = wire::PreparedMedia {
                    frames: source.media.frames.clone(),
                    looping: source.media.looping,
                    provenance: Provenance {
                        source: MediaSource::Canvas { canvas: *canvas, media: source.media.id },
                        source_revision: source.created_revision,
                        artifact_hash: source.media.provenance.artifact_hash,
                        authors: source.media.provenance.authors.clone(),
                    },
                };
                place(s, p, prepared)
            },
        ),
    ]
}
