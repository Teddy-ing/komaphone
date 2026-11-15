//! Read-only access to terminal games. Authorization belongs to the frozen roster,
//! never the current lobby or knowledge of an old asset hash.
use crate::{Error, Result, assets, store};
use photocraft_party::protocol::{ArchivedGameSummary, RpcResponse};
use photocraft_party::room::{Phase, RevealedEntry, Room, RoomView};
use photocraft_party::{AssetHash, GameId, ParticipantId, RoomId};
use rusqlite::{OptionalExtension, Transaction, params};

const MAX_ARCHIVES: usize = 256;
const PAGE: usize = 16;

fn terminal(room: &Room, actor: ParticipantId) -> Result<RoomView> {
    if !matches!(room.phase(), Phase::Reveal | Phase::Archived) {
        return Err(photocraft_party::Error::Phase.into());
    }
    let view = room.view(actor)?;
    if !view.members.iter().any(|member| member.participant.id == actor) || view.reveal.is_empty() {
        return Err(photocraft_party::Error::Unauthorized.into());
    }
    Ok(view)
}

fn game_id(view: &RoomView) -> Result<GameId> {
    let entry = view.reveal.first().ok_or(Error::Storage)?;
    Ok(match entry {
        RevealedEntry::Artwork { assignment, .. } | RevealedEntry::Missing { assignment, .. } => assignment.id.game,
    })
}

fn visit(tx: &Transaction<'_>, room: RoomId, mut visitor: impl FnMut(Room) -> Result<()>) -> Result<()> {
    let mut query = tx.prepare("SELECT revision,substr(snapshot,1,?2),length(snapshot) FROM archives WHERE room=?1 ORDER BY revision LIMIT 257")?;
    let rows = query.query_map(params![room.as_str(), photocraft_party::MAX_ROOM_SNAPSHOT_BYTES as i64], |r| {
        Ok((store::read_u64(r, 0)?, r.get::<_, Vec<u8>>(1)?, store::read_u64(r, 2)?))
    })?;
    for (index, row) in rows.enumerate() {
        if index >= MAX_ARCHIVES {
            return Err(photocraft_party::Error::Limit("archived games").into());
        }
        let (revision, bytes, length) = row?;
        if bytes.len() as u64 != length {
            return Err(Error::Storage);
        }
        let archived = Room::from_snapshot(&bytes)?;
        if archived.id() != room || archived.revision() != revision || !matches!(archived.phase(), Phase::Reveal | Phase::Archived) {
            return Err(Error::Storage);
        }
        visitor(archived)?;
    }
    Ok(())
}

pub(crate) fn verify(tx: &Transaction<'_>) -> Result<()> {
    let mut query = tx.prepare("SELECT DISTINCT room FROM archives LIMIT 129")?;
    let ids = query.query_map([], |r| r.get::<_, String>(0))?;
    for (index, id) in ids.enumerate() {
        if index >= 128 {
            return Err(photocraft_party::Error::Limit("archived rooms").into());
        }
        let id = RoomId::new(&id?)?;
        let _current = store::load_room(tx, id)?;
        visit(tx, id, |room| assets::verify_room_dependencies(tx, &room))?;
    }
    Ok(())
}

pub(crate) fn history(tx: &Transaction<'_>, room: RoomId, actor: ParticipantId, after: u64) -> Result<RpcResponse> {
    let current = store::load_room(tx, room)?;
    let mut authorized = current.view(actor).is_ok();
    let mut games = Vec::new();
    let mut add = |candidate: &Room| -> Result<()> {
        // A waiting member is not in this game's frozen roster.
        let Ok(view) = terminal(candidate, actor) else { return Ok(()) };
        authorized = true;
        if candidate.revision() > after {
            games.push(ArchivedGameSummary {
                game: game_id(&view)?,
                revision: view.revision,
                mode: view.mode,
                complete: view.reveal.iter().all(|entry| matches!(entry, RevealedEntry::Artwork { .. })),
                authors: view.members.into_iter().map(|member| member.participant).collect(),
            });
        }
        Ok(())
    };
    visit(tx, room, |candidate| add(&candidate))?;
    add(&current)?;
    if !authorized {
        return Err(photocraft_party::Error::Unauthorized.into());
    }
    if after > current.revision() {
        return Err(photocraft_party::Error::Conflict.into());
    }
    let more = games.len() > PAGE;
    games.truncate(PAGE);
    let next_revision = if more { games.last().map(|game| game.revision) } else { None };
    Ok(RpcResponse::History { games, next_revision })
}

fn load(tx: &Transaction<'_>, room: RoomId, game: GameId, actor: ParticipantId) -> Result<Room> {
    let current = store::load_room(tx, room)?;
    if terminal(&current, actor).is_ok_and(|view| game_id(&view).is_ok_and(|id| id == game)) {
        return Ok(current);
    }
    let mut found = None;
    visit(tx, room, |candidate| {
        if terminal(&candidate, actor).is_ok_and(|view| game_id(&view).is_ok_and(|id| id == game)) {
            if found.is_some() {
                return Err(Error::Storage);
            }
            found = Some(candidate);
        }
        Ok(())
    })?;
    found.ok_or_else(|| photocraft_party::Error::Unauthorized.into())
}

pub(crate) fn game(tx: &Transaction<'_>, room: RoomId, game: GameId, actor: ParticipantId) -> Result<RpcResponse> {
    Ok(RpcResponse::Game { view: Box::new(terminal(&load(tx, room, game, actor)?, actor)?) })
}

fn load_at(tx: &Transaction<'_>, room: RoomId, game: GameId, actor: ParticipantId, revision: u64) -> Result<Room> {
    if revision == 0 {
        return Err(photocraft_party::Error::Unauthorized.into());
    }
    let current = store::load_room(tx, room)?;
    // Membership heartbeats may raise a terminal room's revision after Game was
    // read. The immutable game/roster/assets remain the authority, not this hint.
    if revision <= current.revision() && terminal(&current, actor).is_ok_and(|view| game_id(&view).is_ok_and(|id| id == game)) {
        return Ok(current);
    }
    let found: Option<(Vec<u8>, u64, u64)> = tx
        .query_row(
            "SELECT substr(snapshot,1,?3),length(snapshot),revision FROM archives WHERE room=?1 AND revision>=?2 ORDER BY revision LIMIT 1",
            params![room.as_str(), store::sql_int(revision)?, photocraft_party::MAX_ROOM_SNAPSHOT_BYTES as i64],
            |row| Ok((row.get(0)?, store::read_u64(row, 1)?, store::read_u64(row, 2)?)),
        )
        .optional()?;
    let (bytes, length, stored_revision) = found.ok_or(photocraft_party::Error::Unauthorized)?;
    if bytes.len() as u64 != length {
        return Err(Error::Storage);
    }
    let archived = Room::from_snapshot(&bytes)?;
    if archived.id() != room || archived.revision() != stored_revision || game_id(&terminal(&archived, actor)?)? != game {
        return Err(photocraft_party::Error::Unauthorized.into());
    }
    Ok(archived)
}

pub(crate) fn fetch(
    tx: &Transaction<'_>,
    room: RoomId,
    game: GameId,
    actor: ParticipantId,
    revision: u64,
    hash: AssetHash,
    offset: u64,
) -> Result<RpcResponse> {
    if !load_at(tx, room, game, actor, revision)?.can_fetch(actor, hash) {
        return Err(photocraft_party::Error::Unauthorized.into());
    }
    assets::fetch(tx, room, hash, offset)
}

pub(crate) fn notes(tx: &Transaction<'_>, room: RoomId, game: GameId, actor: ParticipantId) -> Result<RpcResponse> {
    Ok(RpcResponse::Notes { notes: load(tx, room, game, actor)?.notes(actor)? })
}
