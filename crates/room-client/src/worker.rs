use crate::store::random_hex;
use crate::{ClientStatus, CredentialStore, Error, FreezePin, Invitation, Outbox, Result, Transport, WindowsCredentials};
use photocraft_party::contracts::{MAX_ASSET_BYTES, MAX_PROJECT_BYTES};
use photocraft_party::protocol::{CHUNK_BYTES, Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse};
use photocraft_party::room::Action;
use photocraft_party::{AssetHash, GameId, Mode, ProjectId, RequestId, RoomId, Submission};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError};
use std::time::{Duration, Instant};

/// Background requests have no Debug implementation because invitations are capabilities.
pub enum ClientCommand {
    Connect {
        endpoint: String,
        display_name: String,
    },
    Create {
        mode: Mode,
        timer_ms: Option<u64>,
    },
    Join {
        room: RoomId,
        invitation: String,
    },
    Invite {
        expires_in_ms: u64,
        uses: u8,
    },
    Revoke {
        invitation_id: RequestId,
    },
    Action(Box<Action>),
    Start,
    /// Byte objects must include every manifest reference that is not already uploaded.
    /// This API does not turn an unsaved editor document into a durable submission.
    Submit {
        submission: Box<Submission>,
        assets: Vec<Vec<u8>>,
        freeze: bool,
    },
    /// Explicitly queue one bounded protocol operation for headless integration/automation.
    Enqueue(Operation),
    Disconnect,
    Retry,
    RetainRejected,
    PinFreeze(FreezePin),
    SaveNote(crate::NoteDraft),
    SaveReveal(crate::RevealPosition),
    Notes,
    ImageAck {
        room: RoomId,
        hash: AssetHash,
    },
}

pub enum ClientEvent {
    Status { handled_sequence: u64, status: Box<ClientStatus> },
    Invitation(Invitation),
    Image { room: RoomId, asset: photocraft_party::AssetRef, bytes: std::sync::Arc<Vec<u8>> },
    Notes { room: RoomId, game: GameId, epoch: u64, notes: Vec<(photocraft_party::AssignmentId, String)> },
    MediaError(String),
}

#[derive(Default)]
pub struct ClientOptions {
    pub trusted_certificate_pem: Option<Vec<u8>>,
}

pub struct Worker {
    tx: SyncSender<CommandEnvelope>,
    rx: Receiver<ClientEvent>,
    sequence: AtomicU64,
}
struct CommandEnvelope {
    sequence: u64,
    command: ClientCommand,
}
impl Worker {
    pub fn start(database: PathBuf, options: ClientOptions) -> Result<Self> {
        Self::start_with_credentials(database, options, Box::new(WindowsCredentials))
    }
    pub fn start_with_credentials(database: PathBuf, options: ClientOptions, credentials: Box<dyn CredentialStore>) -> Result<Self> {
        let (tx, commands) = mpsc::sync_channel(8);
        let (events, rx) = mpsc::sync_channel(8);
        std::thread::Builder::new()
            .name("komaphone-rooms".into())
            .spawn(move || run(database, options, credentials, commands, events))
            .map_err(|e| Error::Storage(format!("Cannot start room worker: {e}")))?;
        Ok(Self { tx, rx, sequence: AtomicU64::new(1) })
    }
    pub fn send(&self, command: ClientCommand) -> Result<u64> {
        let sequence = self.sequence.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |value| value.checked_add(1)).map_err(|_| Error::Busy)?;
        self.tx.try_send(CommandEnvelope { sequence, command }).map_err(|_| Error::Busy)?;
        Ok(sequence)
    }
    pub fn poll(&self) -> std::result::Result<ClientEvent, TryRecvError> {
        self.rx.try_recv()
    }
}

fn run(database: PathBuf, options: ClientOptions, credentials: Box<dyn CredentialStore>, commands: Receiver<CommandEnvelope>, events: SyncSender<ClientEvent>) {
    let opened =
        Outbox::open(database, credentials).and_then(|store| Transport::new(options.trusted_certificate_pem.as_deref()).map(|transport| (store, transport)));
    let (mut store, transport) = match opened {
        Ok(value) => value,
        Err(error) => {
            let _ = events.try_send(ClientEvent::Status {
                handled_sequence: 0,
                status: Box::new(ClientStatus { error: Some(error.to_string()), ..ClientStatus::default() }),
            });
            return;
        }
    };
    let mut handled_sequence = 0;
    let mut downloads = crate::download::Downloads::default();
    let mut pending_notes = None;
    publish(&store, &events, handled_sequence);
    let mut pending_invitation = store.invitation().ok().flatten();
    let mut next_network = Instant::now();
    let mut failures = 0u32;
    let mut last_poll = Instant::now().checked_sub(Duration::from_secs(1)).unwrap_or_else(Instant::now);
    loop {
        if store.status().invitation_id.is_none() {
            pending_invitation = None;
        }
        if let Some(invitation) = pending_invitation.take()
            && let Err(mpsc::TrySendError::Full(ClientEvent::Invitation(invitation))) = events.try_send(ClientEvent::Invitation(invitation))
        {
            pending_invitation = Some(invitation);
        }
        match commands.recv_timeout(Duration::from_millis(50)) {
            Ok(envelope) => {
                let result = match envelope.command {
                    ClientCommand::Notes => crate::download::notes(&store, &transport).map(|event| pending_notes = Some(event)),
                    ClientCommand::ImageAck { room, hash } => {
                        downloads.acknowledge(room, hash);
                        Ok(())
                    }
                    command => handle(&mut store, command),
                };
                if let Err(error) = result {
                    let _ = store.mark_error(&error, None, false);
                }
                handled_sequence = envelope.sequence;
                next_network = Instant::now();
                publish(&store, &events, handled_sequence);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if let Some(event) = pending_notes.take()
            && let Err(mpsc::TrySendError::Full(event)) = events.try_send(event)
        {
            pending_notes = Some(event);
        }
        if let Err(error) = downloads.step(&store, &transport, &events) {
            let _ = events.try_send(ClientEvent::MediaError(error.to_string()));
        }
        if Instant::now() < next_network {
            continue;
        }
        match network_step(&mut store, &transport, &mut last_poll) {
            Ok(invitation) => {
                failures = 0;
                if let Some(invitation) = invitation {
                    pending_invitation = Some(invitation);
                }
                let status = store.status();
                let delay = if status.pending > 0 && !status.blocked {
                    10
                } else if status.room.is_some() {
                    250
                } else {
                    1000
                };
                next_network = Instant::now() + Duration::from_millis(delay);
            }
            Err(error) => {
                failures = failures.saturating_add(1).min(5);
                let _ = store.mark_error(&error, None, false);
                next_network = Instant::now() + Duration::from_millis(250 * (1u64 << failures));
            }
        }
        publish(&store, &events, handled_sequence);
    }
}

fn publish(store: &Outbox, events: &SyncSender<ClientEvent>, handled_sequence: u64) {
    let _ = events.try_send(ClientEvent::Status { handled_sequence, status: Box::new(store.status()) });
}

fn room_and_revision(store: &Outbox) -> Result<(RoomId, u64)> {
    let status = store.status();
    if status.pending > 0 {
        return Err(Error::Busy);
    }
    status.room.map(|room| (room.id, room.revision)).ok_or_else(|| Error::Invalid("Create or join a room first".into()))
}

fn handle(store: &mut Outbox, command: ClientCommand) -> Result<()> {
    match command {
        ClientCommand::Connect { endpoint, display_name } => store.configure(&endpoint, &display_name),
        ClientCommand::Create { mode, timer_ms } => {
            mode.validate().map_err(|e| Error::Invalid(e.to_string()))?;
            if store.status().pending > 0 {
                return Err(Error::Busy);
            }
            store.enqueue(Operation::Create { mode, timer_ms }).map(|_| ())
        }
        ClientCommand::Join { room, invitation } => {
            if store.status().pending > 0 {
                return Err(Error::Busy);
            }
            store.enqueue(Operation::Join { room, invitation }).map(|_| ())
        }
        ClientCommand::Invite { expires_in_ms, uses } => {
            let (room, _) = room_and_revision(store)?;
            store.enqueue(Operation::Invite { room, secret: random_hex(32)?, expires_in_ms, uses }).map(|_| ())
        }
        ClientCommand::Revoke { invitation_id } => {
            let (room, _) = room_and_revision(store)?;
            store.enqueue(Operation::RevokeInvite { room, invitation_id }).map(|_| ())
        }
        ClientCommand::Action(action) => {
            let (room, expected_revision) = room_and_revision(store)?;
            store.enqueue(Operation::Apply { room, expected_revision, action }).map(|_| ())
        }
        ClientCommand::Start => start(store),
        ClientCommand::Submit { submission, assets, freeze } => submit(store, *submission, assets, freeze),
        ClientCommand::Enqueue(operation) => store.enqueue(operation).map(|_| ()),
        ClientCommand::Disconnect => {
            let status = store.status();
            if let Some(room) = status.room {
                store.enqueue(Operation::Disconnect { room: room.id })?;
            }
            // Stop heartbeats immediately, even if the endpoint is unavailable. Pending
            // disconnect is retained for the next explicit reconnect.
            store.stop_polling()
        }
        ClientCommand::Retry => store.retry(),
        ClientCommand::RetainRejected => store.retain_rejected(),
        ClientCommand::PinFreeze(pin) => store.pin_freeze(pin),
        ClientCommand::SaveNote(note) => store.save_note(note),
        ClientCommand::SaveReveal(position) => store.save_reveal_position(position),
        ClientCommand::Notes => Err(Error::Invalid("Notes require an authenticated service request".into())),
        ClientCommand::ImageAck { .. } => Ok(()),
    }
}

fn start(store: &mut Outbox) -> Result<()> {
    let (room, expected_revision) = room_and_revision(store)?;
    let view = store.status().room.ok_or_else(|| Error::Invalid("Room is unavailable".into()))?;
    let rounds = view.mode.rounds(view.members.len()).map_err(|e| Error::Invalid(e.to_string()))?;
    let count = usize::from(rounds).saturating_mul(view.members.len());
    if count > 36 {
        return Err(Error::Invalid("Room assignment limit exceeded".into()));
    }
    let projects = (0..count).map(|_| ProjectId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))).collect::<Result<Vec<_>>>()?;
    let game = GameId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?;
    store.enqueue(Operation::Apply { room, expected_revision, action: Box::new(Action::Start { game, projects }) }).map(|_| ())
}

fn submit(store: &mut Outbox, submission: Submission, assets: Vec<Vec<u8>>, freeze: bool) -> Result<()> {
    submission.validate().map_err(|e| Error::Invalid(e.to_string()))?;
    let (room, expected_revision) = room_and_revision(store)?;
    let status = store.status();
    let view = status.room.ok_or_else(|| Error::Invalid("Room is unavailable".into()))?;
    let assignment = view.assignment.as_ref().ok_or_else(|| Error::Invalid("No assignment is available".into()))?;
    if status.profile.as_ref().map(|p| p.id) != Some(submission.author)
        || assignment.assignment.id != submission.assignment
        || assignment.assignment.project != submission.project
        || assignment.submitted
    {
        return Err(Error::Invalid("Submission must use the current player's durable assigned project".into()));
    }
    if status.freeze_pin.as_ref().is_some_and(|pin| {
        pin.room == room
            && pin.epoch == view.epoch
            && pin.assignment == submission.assignment
            && pin.project == submission.project
            && pin.revision != submission.revision
    }) {
        return Err(Error::Invalid("This deadline already pinned a different acknowledged revision; submit the first pinned revision".into()));
    }
    let total = assets.iter().map(|bytes| bytes.len() as u64).fold(0u64, u64::saturating_add);
    if total > MAX_PROJECT_BYTES || assets.len() > 256 {
        return Err(Error::Invalid("Submission asset budget exceeded; the local draft is retained".into()));
    }
    let mut operations = Vec::new();
    if freeze {
        operations.push(Operation::Apply {
            room,
            expected_revision,
            action: Box::new(Action::FreezeReport { epoch: view.epoch, submission: submission.clone() }),
        });
    }
    for bytes in assets {
        if bytes.is_empty() || bytes.len() as u64 > MAX_ASSET_BYTES {
            return Err(Error::Invalid("Submission object must be 1 byte through 64 MiB".into()));
        }
        let hash = AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).map_err(|e| Error::Invalid(e.to_string()))?;
        let total = bytes.len() as u64;
        for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
            let offset = index.saturating_mul(CHUNK_BYTES) as u64;
            operations.push(Operation::Upload { room, hash, total, offset, bytes: chunk.to_vec(), finish: offset.saturating_add(chunk.len() as u64) == total });
            if operations.len() > 8190 {
                return Err(Error::Busy);
            }
        }
    }
    operations.push(Operation::Apply {
        room,
        expected_revision: expected_revision.saturating_add(u64::from(freeze)),
        action: Box::new(Action::Submit { epoch: view.epoch, submission }),
    });
    store.enqueue_batch(operations).map(|_| ())
}

fn network_step(store: &mut Outbox, transport: &Transport, last_poll: &mut Instant) -> Result<Option<Invitation>> {
    let status = store.status();
    let Some(endpoint) = status.endpoint else { return Ok(None) };
    let credential = store.credential()?;
    if store.active()
        && status.paired
        && last_poll.elapsed() >= Duration::from_millis(250)
        && let Some(room) = status.room.as_ref()
    {
        let request = RpcRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(&random_hex(16)?).map_err(|e| Error::Invalid(e.to_string()))?,
            operation: Operation::Poll { room: room.id, after_revision: room.revision },
        };
        match transport.call(&endpoint, &credential, &request)? {
            RpcResponse::Room { view, .. } if view.id == room.id => store.update_poll(*view)?,
            _ => return Err(Error::Invalid("Service returned the wrong room view".into())),
        }
        *last_poll = Instant::now();
    }
    store.ensure_transfer_freeze()?;
    if !store.status().blocked
        && let Some(request) = store.next()?
    {
        match transport.call(&endpoint, &credential, &request) {
            Ok(response) => {
                store.acknowledge(&request, &response)?;
                if matches!(response, RpcResponse::Invitation { .. }) {
                    return store.invitation();
                }
            }
            Err(error) => {
                let permanent =
                    !matches!(error, Error::Unavailable) && !matches!(&error, Error::Rejected { code, .. } if matches!(code.as_str(), "busy" | "storage"));
                store.mark_error(&error, Some(request.request_id), permanent)?;
                return Err(error);
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_party::room::{Assignment, AssignmentView, Phase, Room};
    use photocraft_party::{AssetRef, AssignmentId, CheckpointId, CheckpointRef, ReplayId, ReplayRef, SubmissionId};
    struct FixtureCredentials;
    impl CredentialStore for FixtureCredentials {
        fn get(&self, _: &str, _: &str) -> Result<Option<String>> {
            Ok(Some("a".repeat(64)))
        }
        fn set(&self, _: &str, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
        fn remove(&self, _: &str, _: &str) -> Result<()> {
            Ok(())
        }
    }

    #[test]
    fn wrong_pinned_revision_never_queues_artifacts_or_a_submission() {
        let root = tempfile::tempdir().unwrap();
        let mut store = Outbox::open(root.path().join("client.sqlite3"), Box::new(FixtureCredentials)).unwrap();
        store.configure("http://127.0.0.1:43127", "Pin fixture").unwrap();
        let pair = store.next().unwrap().unwrap();
        let profile = store.status().profile.unwrap();
        store.acknowledge(&pair, &RpcResponse::Paired { participant: profile.clone() }).unwrap();
        let room = Room::new(RoomId::new(&"1".repeat(32)).unwrap(), profile.clone(), Mode::Animation { rotations: 1 }, Some(1000)).unwrap();
        let mut view = room.view(profile.id).unwrap();
        view.phase = Phase::Transfer;
        view.epoch = 2;
        let assignment = Assignment {
            id: AssignmentId { game: GameId::new(&"2".repeat(32)).unwrap(), chain: 0, round: 0 },
            player: profile.id,
            project: ProjectId::new(&"3".repeat(32)).unwrap(),
        };
        view.assignment = Some(AssignmentView { assignment: assignment.clone(), submitted: false, reference: None });
        store.update_poll(view.clone()).unwrap();
        store.pin_freeze(FreezePin { room: view.id, epoch: view.epoch, assignment: assignment.id, project: assignment.project, revision: 1 }).unwrap();
        let image = AssetRef { hash: AssetHash::new(&"4".repeat(64)).unwrap(), bytes: 4 };
        let checkpoint =
            CheckpointRef { id: CheckpointId::new(&"5".repeat(32)).unwrap(), asset: AssetRef { hash: AssetHash::new(&"6".repeat(64)).unwrap(), bytes: 4 } };
        let submission = Submission {
            id: SubmissionId::new(&"7".repeat(32)).unwrap(),
            assignment: assignment.id,
            author: profile.id,
            project: assignment.project,
            revision: 2,
            image,
            checkpoint,
            replay: ReplayRef {
                id: ReplayId::new(&"8".repeat(32)).unwrap(),
                manifest: None,
                baseline: checkpoint,
                end_event: None,
                event_count: 0,
                duration_ms: 0,
            },
            assets: vec![image, checkpoint.asset],
            note: String::new(),
            seam: None,
        };
        for freeze in [true, false] {
            assert!(submit(&mut store, submission.clone(), vec![b"future artifact".to_vec()], freeze).is_err());
            assert_eq!(store.status().pending, 0);
            assert!(store.next().unwrap().is_none());
            assert!(store.status().submission_project.is_none());
        }
        let mut original = submission;
        original.revision = 1;
        submit(&mut store, original, Vec::new(), true).unwrap();
        assert_eq!(store.status().pending, 2);
    }
}
