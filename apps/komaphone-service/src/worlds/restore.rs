//! Archive restoration never accepts caller-supplied historical author authority.
//! The original service proves the immutable prefix, then creates a private copy.
use super::{Limits, MAX_EVENT_BYTES, MAX_STATE_BYTES, media, require_owner, scene_error, state, summary};
use crate::store::{digest, sql_int};
use crate::{Error, Result};
use photocraft_party::Error as Domain;
use photocraft_scene::shared::media::AssetHash;
use photocraft_scene::world_protocol::Response;
use photocraft_scene::{CanvasId, OwnerId};
use rusqlite::{Transaction, params};
use std::collections::{BTreeMap, BTreeSet};

pub(super) fn copy(
    db: &Transaction<'_>,
    profile: OwnerId,
    source: CanvasId,
    revision: u64,
    hash: AssetHash,
    destination: CanvasId,
    limits: Limits,
) -> Result<Response> {
    // Also checked before idempotent retries: a historical receipt is no grant.
    let source_info = require_owner(db, source, profile)?;
    if source == destination {
        return Err(Error::Invalid("archive restore requires a new canvas"));
    }
    let original = state(db, source)?;
    original.validate().map_err(|_| Error::Storage)?;
    if original.canvas != source
        || original.owner != profile
        || original.revision != source_info.revision
        || original.name != source_info.name
        || original.replay_to(0).map_err(|_| Error::Storage)?.name != source_info.initial_name
    {
        return Err(Error::Storage);
    }
    if revision > original.revision {
        return Err(Domain::Conflict.into());
    }
    let prefix = original.replay_to(revision).map_err(scene_error)?;
    if prefix.canonical_hash().map_err(scene_error)? != hash.as_str() {
        return Err(Domain::Conflict.into());
    }
    let copied = prefix.copy_to(destination).map_err(scene_error)?;
    if db.query_row("SELECT EXISTS(SELECT 1 FROM worlds WHERE id=?1)", [destination.as_str()], |r| r.get::<_, bool>(0))? {
        let existing_info = require_owner(db, destination, profile)?;
        let existing = state(db, destination)?;
        existing.validate().map_err(|_| Error::Storage)?;
        if existing.canvas != destination
            || existing.owner != profile
            || existing.revision != existing_info.revision
            || existing.name != existing_info.name
            || existing.replay_to(0).map_err(|_| Error::Storage)?.name != existing_info.initial_name
        {
            return Err(Error::Storage);
        }
        if existing.revision < revision || existing.replay_to(revision).map_err(scene_error)? != copied {
            return Err(Domain::Conflict.into());
        }
        // Leave subsequent accepted art and explicit membership changes intact.
        return Ok(Response::Canvas { info: summary(db, destination, profile)? });
    }
    let count: u32 = db.query_row("SELECT count(*) FROM worlds", [], |r| r.get(0))?;
    if count >= limits.canvases {
        return Err(Domain::Limit("canvas count").into());
    }
    let body = serde_json::to_vec(&copied)?;
    if body.len() > MAX_STATE_BYTES {
        return Err(Domain::Limit("canvas checkpoint").into());
    }
    let initial = copied.replay_to(0).map_err(scene_error)?.name;
    db.execute(
        "INSERT INTO worlds VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![destination.as_str(), profile.as_str(), copied.name, initial, sql_int(copied.revision)?, body, digest(&body)],
    )?;
    db.execute("INSERT INTO world_members VALUES(?1,?2,1,1)", params![destination.as_str(), profile.as_str()])?;
    super::archive_import::store_origin(db, &copied)?;
    let mut after = 0;
    loop {
        let events = copied.events_after(after, photocraft_scene::shared::MAX_EVENT_PAGE).map_err(scene_error)?;
        if events.is_empty() {
            break;
        }
        for event in events {
            let body = serde_json::to_vec(&event)?;
            if body.len() > MAX_EVENT_BYTES {
                return Err(Domain::Limit("canvas event").into());
            }
            db.execute(
                "INSERT INTO world_events VALUES(?1,?2,?3,?4,?5)",
                params![destination.as_str(), sql_int(event.revision)?, event.op.id.as_str(), body, digest(&body)],
            )?;
            after = event.revision;
        }
    }
    // One verified image at a time: never accumulate a second 512 MiB archive.
    let mut seen = BTreeSet::new();
    for media in copied.media.values() {
        for frame in &media.media.frames {
            if seen.insert(frame.hash) {
                let bytes = media::read_blob(db, source, frame.hash, Some(frame.byte_len))?;
                media::store_blobs(db, destination, &BTreeMap::from([(frame.hash, bytes)]))?;
            }
        }
    }
    Ok(Response::Canvas { info: summary(db, destination, profile)? })
}
