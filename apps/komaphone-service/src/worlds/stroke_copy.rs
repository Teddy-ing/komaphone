use super::{append_authorized, denied, grant, scene_error, state, summary};
use crate::{Error, Result};
use photocraft_scene::OwnerId;
use photocraft_scene::shared::media::Credit;
use photocraft_scene::shared::{DeviceId, Edit, StrokeCopyTarget, WorldOp};
use photocraft_scene::world_protocol::Response;
use rusqlite::Connection;

pub(super) fn copy(db: &Connection, profile: OwnerId, device: DeviceId, operation: &WorldOp) -> Result<Response> {
    let epoch = grant(db, operation.canvas, profile)?;
    if operation.author != profile || operation.device != device || operation.authority_epoch != epoch {
        return denied();
    }
    let Edit::CopyStroke { stroke, provenance } = &operation.action else {
        return Err(Error::Invalid("stroke copy operation"));
    };
    // Current permission and visibility are required even for duplicate receipts.
    let source_info = summary(db, provenance.canvas, profile)?;
    let source_world = state(db, provenance.canvas)?;
    source_world.validate().map_err(|_| Error::Storage)?;
    if source_world.canvas != provenance.canvas
        || source_world.owner != source_info.owner
        || source_world.revision != source_info.revision
        || source_world.name != source_info.name
        || source_world.replay_to(0).map_err(|_| Error::Storage)?.name != source_info.initial_name
    {
        return Err(Error::Storage);
    }
    let source = source_world.strokes.get(&provenance.stroke).ok_or(Error::Invalid("stroke copy source"))?;
    let credit = if let Some(origin) = &source.provenance {
        origin.author.clone()
    } else if let Some(credit) = source_world.imported_credit(source.stroke.owner) {
        credit
    } else {
        let label = crate::identity::hosted::profile_label(db, source.stroke.owner.as_str())?;
        Credit { profile: source.stroke.owner, label }
    };
    let expected = source_world
        .copy_stroke(
            provenance.stroke,
            credit,
            StrokeCopyTarget {
                canvas: operation.canvas,
                id: stroke.id,
                owner: profile,
                anchor: stroke.anchor,
                offset: provenance.offset,
                scale: provenance.scale,
            },
        )
        .map_err(scene_error)?;
    if expected != operation.action {
        return Err(Error::Invalid("stroke copy differs from authorized source"));
    }
    append_authorized(db, operation)
}
