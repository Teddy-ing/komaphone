use super::{BTreeSet, Result, error, wire};
fn page<T: Copy + Ord>(ids: impl Iterator<Item = T>, after: Option<T>, next: Option<T>, limit: u32) -> Result<()> {
    let mut previous = after;
    let mut count = 0usize;
    for id in ids {
        if previous.is_some_and(|p| id <= p) {
            return Err(error("Service returned an unordered directory page"));
        }
        previous = Some(id);
        count = count.saturating_add(1);
    }
    if count > limit.min(wire::MAX_PAGE) as usize || next.is_some_and(|next| count == 0 || Some(next) != previous) {
        return Err(error("Service returned an invalid directory cursor"));
    }
    Ok(())
}
fn info(info: &wire::CanvasSummary) -> Result<()> {
    photocraft_scene::validate_name(&info.name).map_err(error)?;
    photocraft_scene::validate_name(&info.initial_name).map_err(error)?;
    if let Some(origin) = &info.import_origin {
        origin.validate(info.canvas, info.owner).map_err(error)?;
        if info.revision < origin.source_revision {
            return Err(error("Canvas summary omits the committed archive prefix"));
        }
    }
    if info.epoch == 0 || info.epoch > i64::MAX as u64 || info.revision > i64::MAX as u64 {
        return Err(error("Invalid service canvas revision or grant"));
    }
    Ok(())
}
pub(super) fn response(response: &wire::Response, request: Option<&wire::Action>, sequence: u64) -> Result<()> {
    use wire::{Action as A, Response as R};
    match (request, response) {
        (Some(A::Identity {}), R::Identity { .. }) => {}
        (Some(A::RestoreCopy { new_canvas, .. }), R::Canvas { info: value }) if *new_canvas == value.canvas => info(value)?,
        (Some(A::ImportBegin { new_canvas, .. }), R::Canvas { info: value }) if *new_canvas == value.canvas => info(value)?,
        (Some(A::ImportCommit { .. }), R::Canvas { info: value }) => info(value)?,
        (Some(A::ImportBegin { id, manifest_bytes, .. }), R::ImportProgress { id: returned, object: wire::ImportObject::Manifest {}, received, total })
            if id == returned && total == manifest_bytes && received <= total => {}
        (Some(A::ImportChunk { id, object, offset, bytes }), R::ImportProgress { id: returned, object: returned_object, received, total })
            if id == returned
                && object == returned_object
                && *received >= offset.saturating_add(bytes.len() as u64)
                && received <= total
                && *total <= photocraft_scene::shared::archive_import::MAX_TOTAL_BYTES => {}
        (Some(A::ImportCancel { .. }), R::Done) => {}
        (Some(A::List { after, limit }), R::Canvases { items, next }) => {
            page(items.iter().map(|v| v.canvas), *after, *next, *limit)?;
            for item in items {
                info(item)?;
            }
        }
        (Some(A::Trash { after, limit }), R::Trash { items, next }) => {
            page(items.iter().map(|v| v.canvas), *after, *next, *limit)?;
            for item in items {
                photocraft_scene::validate_name(&item.name).map_err(error)?;
                if item.owner_epoch == 0 || item.owner_epoch > i64::MAX as u64 || item.revision > i64::MAX as u64 || item.deleted_at_ms > i64::MAX as u64 {
                    return Err(error("Invalid deleted canvas metadata"));
                }
            }
        }
        (None, R::Canvases { items, next }) if sequence == 0 => {
            page(items.iter().map(|v| v.canvas), None, *next, wire::MAX_PAGE)?;
            for item in items {
                info(item)?;
            }
        }
        (Some(A::People { after, limit }), R::People { items, next }) => {
            page(items.iter().map(|v| v.profile), *after, *next, *limit)?;
            for item in items {
                photocraft_party::validate_label(&item.display_name, photocraft_party::MAX_NAME_BYTES, false).map_err(error)?;
            }
        }
        (Some(A::Members { after, limit, .. }), R::Members { items, next }) => page(items.iter().map(|v| v.profile), *after, *next, *limit)?,
        (Some(A::Create { canvas, .. } | A::Info { canvas } | A::Accept { canvas, .. } | A::RestoreOriginal { canvas, .. }), R::Canvas { info: value })
            if *canvas == value.canvas =>
        {
            info(value)?
        }
        (Some(A::Peers { canvas } | A::Presence { canvas, .. }), R::Peers { peers }) => {
            if peers.len() > 4096 {
                return Err(error("Presence page exceeds the local limit"));
            }
            let mut devices = BTreeSet::new();
            for peer in peers {
                if peer.camera.canvas != *canvas || !devices.insert(peer.device) || peer.preview.len() > wire::MAX_PREVIEW_POINTS {
                    return Err(error("Invalid shared presence"));
                }
                photocraft_scene::coordinates::validate_camera(&peer.camera).map_err(error)?;
                if let Some(style) = &peer.stroke {
                    if !peer.preview.is_empty() {
                        photocraft_scene::Stroke {
                            id: style.id,
                            canvas: *canvas,
                            owner: peer.profile,
                            revision: 0,
                            anchor: style.anchor,
                            points: peer.preview.clone(),
                            width: style.width,
                            color: style.color,
                            erase: style.erase,
                        }
                        .validate()
                        .map_err(error)?;
                    }
                } else if !peer.preview.is_empty() {
                    return Err(error("A preview is missing its stroke style"));
                }
            }
        }
        (Some(A::Invite { id: expected, .. }), R::Invited { id, .. }) if expected == id => {}
        (Some(A::Remove { .. } | A::Leave { .. } | A::CancelInvite { .. } | A::Delete { .. }), R::Done) => {}
        (Some(A::PrepareGameMedia { .. }), R::MediaPrepared { .. }) | (Some(A::FetchMedia { .. }), R::MediaChunk { .. }) => {}
        _ => return Err(error("Service response does not match its canvas request")),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worlds::{Arc, Canvas, CanvasId, OwnerId, ReviewState, Session, Viewer, WorldState};
    #[test]
    fn directory_pages_must_advance_and_respect_bounds() {
        assert!(page([1u32, 2].into_iter(), None, Some(2), 2).is_ok());
        assert!(page([1u32, 1].into_iter(), None, None, 2).is_err());
        assert!(page([1u32, 2].into_iter(), Some(1), None, 2).is_err());
        assert!(page([1u32].into_iter(), None, Some(2), 2).is_err());
        assert!(page([1u32, 2].into_iter(), None, None, 1).is_err());
        assert!(page([].into_iter(), None, Some(1u32), 2).is_err());
    }
    #[test]
    fn presence_heartbeat_requires_a_valid_peer_snapshot_for_the_requested_canvas() {
        let canvas = CanvasId::new("11111111111111111111111111111111").unwrap();
        let other = CanvasId::new("22222222222222222222222222222222").unwrap();
        let camera = photocraft_scene::coordinates::home(canvas);
        let request = wire::Action::Presence { canvas, epoch: 1, camera: camera.clone(), preview: vec![], cursor: None, stroke: None };
        let peer = wire::Peer {
            profile: OwnerId::new("33333333333333333333333333333333").unwrap(),
            device: photocraft_scene::shared::DeviceId::new("44444444444444444444444444444444").unwrap(),
            camera,
            preview: vec![],
            cursor: None,
            stroke: None,
            expires_at_ms: 2000,
        };
        assert!(response(&wire::Response::Peers { peers: vec![peer.clone()] }, Some(&request), 7).is_ok());
        assert!(response(&wire::Response::Peers { peers: vec![] }, Some(&request), 7).is_ok());
        assert!(response(&wire::Response::Done, Some(&request), 7).is_err());
        assert!(response(&wire::Response::Peers { peers: vec![peer.clone(), peer.clone()] }, Some(&request), 7).is_err());
        let mut foreign = peer;
        foreign.camera = photocraft_scene::coordinates::home(other);
        assert!(response(&wire::Response::Peers { peers: vec![foreign] }, Some(&request), 7).is_err());
    }
    #[test]
    fn switching_account_clears_private_diagnostics_and_capabilities() {
        let mut session = Session::new();
        session.worlds.error = Some("Previous account's private export path".into());
        session.worlds.notice = Some("Previous account's private recovery note".into());
        session.worlds.invitation = Some(serde_json::json!({"secret":"previous-account-invitation"}));
        session.worlds.clear_private();
        assert!(session.worlds.error.is_none() && session.worlds.notice.is_none() && session.worlds.invitation.is_none());
    }
    #[test]
    fn catch_up_view_cannot_mutate_unseen_current_artwork() {
        let canvas = CanvasId::new("11111111111111111111111111111111").unwrap();
        let owner = OwnerId::new("22222222222222222222222222222222").unwrap();
        let world = Arc::new(WorldState::new(canvas, owner, "Review fixture".into()).unwrap());
        let mut review = ReviewState::first_visit(canvas, owner, 0).unwrap();
        review.begin(0).unwrap();
        let mut session = Session::new();
        session.worlds.selected = Some(canvas);
        session.worlds.active = Some(Canvas {
            info: wire::CanvasSummary { canvas, owner, name: world.name.clone(), import_origin: None, initial_name: world.name.clone(), revision: 0, epoch: 1 },
            canonical: world.clone(),
            display: world.clone(),
            pending: 0,
            viewer: Viewer { camera: photocraft_scene::coordinates::home(canvas), bookmarks: vec![], review: Some(review) },
            notice: None,
        });
        for action in [
            photocraft_scene::shared::Edit::Rename { name: "Unseen change".into(), expected_revision: 0 },
            photocraft_scene::shared::Edit::SetStrokeVisible {
                stroke: photocraft_scene::StrokeId::new("33333333333333333333333333333333").unwrap(),
                visible: false,
            },
        ] {
            assert!(session.world_edit(canvas, action).unwrap_err().to_string().contains("Exit catch-up"));
            assert!(session.worlds.pending.is_empty());
            assert_eq!(session.worlds.active.as_ref().unwrap().canonical, world);
        }
    }
}
