//! Private local note recovery. Advancing a game must not prune acknowledged text.
use super::{digest, sql};
use crate::{ClientStatus, Error, NoteDraft, Result, normalize_endpoint};
use photocraft_party::{AssignmentId, RoomId};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

// At most 1,024 distinct assignments and 2 MiB of encoded text per client database.
// Reaching either quota refuses growth; no old note is automatically discarded.
const MAX_NOTES: u64 = 1024;
const MAX_NOTES_BYTES: u64 = 2 * 1024 * 1024;
const MAX_RECORD_BYTES: usize = 8 * 1024;

fn validate(draft: &NoteDraft) -> Result<()> {
    draft.assignment.validate().map_err(|error| Error::Invalid(error.to_string()))?;
    photocraft_party::validate_label(&draft.note, photocraft_party::MAX_NOTE_BYTES, true).map_err(|error| Error::Invalid(error.to_string()))
}

pub(super) fn read(db: &Connection, endpoint: &str, room: RoomId, assignment: AssignmentId) -> Result<Option<NoteDraft>> {
    assignment.validate().map_err(|error| Error::Invalid(error.to_string()))?;
    // Bound SQL output before allocating even when the database was corrupted.
    let raw: Option<(i64, Vec<u8>, i64, String)> = db
        .query_row(
            "SELECT length(body),substr(body,1,?6),length(digest),substr(digest,1,65)
             FROM local_assignment_notes WHERE endpoint=?1 AND room=?2 AND game=?3 AND chain=?4 AND round=?5",
            params![endpoint, room.as_str(), assignment.game.as_str(), assignment.chain, assignment.round, (MAX_RECORD_BYTES + 1) as i64],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((length, body, checksum_length, checksum)) = raw else { return Ok(None) };
    if length <= 0 || length > MAX_RECORD_BYTES as i64 || usize::try_from(length).ok() != Some(body.len()) || checksum_length != 64 || digest(&body) != checksum
    {
        return Err(Error::Storage("Local note checksum or size failed; preserve this database for recovery".into()));
    }
    let draft: NoteDraft = serde_json::from_slice(&body).map_err(|_| Error::Storage("Invalid saved note; preserve this database for recovery".into()))?;
    if draft.assignment != assignment || validate(&draft).is_err() {
        return Err(Error::Storage("Invalid saved note assignment or text; preserve this database for recovery".into()));
    }
    Ok(Some(draft))
}

pub(super) fn write(transaction: &Transaction<'_>, endpoint: &str, room: RoomId, draft: &NoteDraft) -> Result<()> {
    validate(draft)?;
    // Detect corrupt prior content before replacing it with a later edit.
    let previous = read(transaction, endpoint, room, draft.assignment)?;
    let body = serde_json::to_vec(draft).map_err(|_| Error::Storage("Cannot encode local note".into()))?;
    if body.len() > MAX_RECORD_BYTES {
        return Err(Error::Storage("Local note exceeds its storage limit".into()));
    }
    let (count, used): (i64, i64) = transaction
        .query_row("SELECT count(*),coalesce(sum(length(body)),0) FROM local_assignment_notes", [], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(sql)?;
    let count = u64::try_from(count).map_err(|_| Error::Storage("Invalid local note count; preserve this database".into()))?;
    let used = u64::try_from(used).map_err(|_| Error::Storage("Invalid local note size; preserve this database".into()))?;
    let old_bytes: i64 = if previous.is_some() {
        transaction
            .query_row(
                "SELECT length(body) FROM local_assignment_notes WHERE endpoint=?1 AND room=?2 AND game=?3 AND chain=?4 AND round=?5",
                params![endpoint, room.as_str(), draft.assignment.game.as_str(), draft.assignment.chain, draft.assignment.round],
                |row| row.get(0),
            )
            .map_err(sql)?
    } else {
        0
    };
    let old_bytes = u64::try_from(old_bytes).map_err(|_| Error::Storage("Invalid local note size; preserve this database".into()))?;
    if (previous.is_none() && count >= MAX_NOTES) || used.saturating_sub(old_bytes).saturating_add(body.len() as u64) > MAX_NOTES_BYTES {
        return Err(Error::Storage("Local note recovery storage is full (1,024 assignments / 2 MiB); existing notes are preserved".into()));
    }
    transaction
        .execute(
            "INSERT INTO local_assignment_notes(endpoint,room,game,chain,round,body,digest) VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(endpoint,room,game,chain,round) DO UPDATE SET body=excluded.body,digest=excluded.digest",
            params![endpoint, room.as_str(), draft.assignment.game.as_str(), draft.assignment.chain, draft.assignment.round, body, digest(&body)],
        )
        .map_err(sql)?;
    Ok(())
}

fn retain_current(transaction: &Transaction<'_>, status: &ClientStatus) -> Result<()> {
    let Some(current) = status.note_draft.as_ref() else { return Ok(()) };
    let Some((endpoint, room)) = status.endpoint.as_deref().zip(status.room.as_ref()) else {
        return Err(Error::Storage("Saved note has no unambiguous service and room scope; preserve this database for recovery".into()));
    };
    let endpoint = normalize_endpoint(endpoint)?;
    match read(transaction, &endpoint, room.id, current.assignment)? {
        Some(saved) if &saved != current => {
            return Err(Error::Storage("Current note and its recovery record disagree; preserve this database for recovery".into()));
        }
        Some(_) => {}
        None => {
            let known_game = room.assignment.as_ref().is_some_and(|assignment| assignment.assignment.id.game == current.assignment.game)
                || room.reveal.iter().any(|entry| match entry {
                    photocraft_party::room::RevealedEntry::Artwork { assignment, .. } | photocraft_party::room::RevealedEntry::Missing { assignment, .. } => {
                        assignment.id.game == current.assignment.game
                    }
                });
            if !known_game {
                return Err(Error::Storage("Saved note does not match the known room game; preserve this database for recovery".into()));
            }
            write(transaction, &endpoint, room.id, current)?;
        }
    }
    Ok(())
}

pub(super) fn migrate(db: &mut Connection, status: &ClientStatus) -> Result<()> {
    let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate).map_err(sql)?;
    retain_current(&transaction, status)?;
    transaction.commit().map_err(sql)
}

pub(super) fn transition(transaction: &Transaction<'_>, previous: &ClientStatus, next: &mut ClientStatus) -> Result<()> {
    let changed_scope = previous.endpoint != next.endpoint || previous.room.as_ref().map(|room| room.id) != next.room.as_ref().map(|room| room.id);
    if changed_scope {
        retain_current(transaction, previous)?;
        next.note_draft = None;
    }
    Ok(())
}
