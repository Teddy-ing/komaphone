//! Deterministic party authority. Time and connection changes are trusted service inputs.
//! This module neither authenticates a transport nor promises durable storage.

use crate::contracts::{
    AssetHash, AssetRef, AssignmentId, CheckpointRef, GameId, MAX_PLAYERS, MAX_ROOM_SNAPSHOT_BYTES, MAX_TIMER_MS, Mode, Participant, ParticipantId, ProjectId,
    ReplayRef, RoomId, Submission, TRANSFER_GRACE_MS,
};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

const ROOM_SCHEMA: u16 = 1;
const MAX_REASON_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Lobby,
    Drawing,
    Transfer,
    Reveal,
    Archived,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PauseReason {
    Moderator,
    Disconnected,
    Restart,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Member {
    participant: Participant,
    connected: bool,
    ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assignment {
    pub id: AssignmentId,
    pub player: ParticipantId,
    pub project: ProjectId,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Entry {
    Submitted { value: Box<Submission>, accepted_revision: u64 },
    Missing { assignment: AssignmentId, reason: String },
}

impl Entry {
    fn assignment(&self) -> AssignmentId {
        match self {
            Self::Submitted { value, .. } => value.assignment,
            Self::Missing { assignment, .. } => *assignment,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema_version: u16,
    id: RoomId,
    moderator: ParticipantId,
    members: Vec<Member>,
    #[serde(default)]
    waiting: Vec<Member>,
    mode: Mode,
    timer_ms: Option<u64>,
    revision: u64,
    epoch: u64,
    phase: Phase,
    round: u8,
    game: Option<GameId>,
    assignments: Vec<Assignment>,
    entries: Vec<Entry>,
    frozen: Vec<Submission>,
    paused: Option<PauseReason>,
    deadline_ms: Option<u64>,
    remaining_ms: Option<u64>,
    last_now_ms: u64,
}

/// Only validated constructors/restores create a room. Never send this state to a client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Room {
    state: State,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Action {
    Join {
        participant: Participant,
    },
    Ready {
        ready: bool,
    },
    SetOptions {
        mode: Mode,
        timer_ms: Option<u64>,
    },
    Leave,
    /// The durable adapter archives the terminal snapshot in the same transaction.
    NextGame,
    /// Projects are unique fresh IDs, ordered by round then chain.
    Start {
        game: GameId,
        projects: Vec<ProjectId>,
    },
    Submit {
        epoch: u64,
        submission: Submission,
    },
    /// Freeze the last locally durable revision/manifest before transferring its assets.
    FreezeReport {
        epoch: u64,
        submission: Submission,
    },
    Pause,
    Resume,
    ClaimModerator,
    Skip {
        participant: ParticipantId,
        reason: String,
    },
    End {
        reason: String,
    },
    Archive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyOutcome {
    pub revision: u64,
    pub duplicate: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemberView {
    pub participant: Participant,
    pub connected: bool,
    pub ready: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reference {
    Animation {
        image: AssetRef,
        note: String,
    },
    /// A separately derived asset, never the full previous segment.
    Seam {
        image: AssetRef,
        width: u32,
        height: u32,
    },
    Missing,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssignmentView {
    pub assignment: Assignment,
    pub submitted: bool,
    pub reference: Option<Reference>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RevealedEntry {
    Artwork { assignment: Assignment, revision: u64, image: AssetRef, checkpoint: CheckpointRef, replay: Box<ReplayRef>, assets: Vec<AssetRef> },
    Missing { assignment: Assignment, reason: String },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomView {
    pub id: RoomId,
    pub revision: u64,
    pub epoch: u64,
    pub phase: Phase,
    pub round: u8,
    pub mode: Mode,
    pub timer_ms: Option<u64>,
    pub moderator: ParticipantId,
    pub members: Vec<MemberView>,
    pub waiting: Vec<MemberView>,
    pub paused: Option<PauseReason>,
    pub deadline_ms: Option<u64>,
    pub assignment: Option<AssignmentView>,
    /// Empty before reveal. Notes require the separate explicit notes request.
    pub reveal: Vec<RevealedEntry>,
}

impl Room {
    pub fn new(id: RoomId, host: Participant, mode: Mode, timer_ms: Option<u64>) -> Result<Self> {
        host.validate()?;
        mode.validate()?;
        validate_timer(timer_ms)?;
        let moderator = host.id;
        Ok(Self {
            state: State {
                schema_version: ROOM_SCHEMA,
                id,
                moderator,
                members: vec![Member { participant: host, connected: true, ready: false }],
                waiting: Vec::new(),
                mode,
                timer_ms,
                revision: 0,
                epoch: 0,
                phase: Phase::Lobby,
                round: 0,
                game: None,
                assignments: Vec::new(),
                entries: Vec::new(),
                frozen: Vec::new(),
                paused: None,
                deadline_ms: None,
                remaining_ms: None,
                last_now_ms: 0,
            },
        })
    }

    pub fn revision(&self) -> u64 {
        self.state.revision
    }
    pub fn phase(&self) -> Phase {
        self.state.phase
    }
    pub fn epoch(&self) -> u64 {
        self.state.epoch
    }
    pub fn id(&self) -> RoomId {
        self.state.id
    }
    /// Trusted service membership enumeration; artwork stays in participant projections.
    pub fn participants(&self) -> Vec<Participant> {
        self.state.members.iter().chain(&self.state.waiting).map(|m| m.participant.clone()).collect()
    }

    /// The adapter authenticates `actor`, checks asset completeness, and durably commits
    /// the resulting snapshot before returning this outcome to a network client.
    pub fn apply(&mut self, actor: ParticipantId, expected_revision: u64, now_ms: u64, action: Action) -> Result<ApplyOutcome> {
        // Retry identity is immutable assignment + submission payload, not the current phase.
        if let Action::Submit { submission, .. } = &action {
            self.member(actor)?;
            if submission.author != actor {
                return Err(Error::Unauthorized);
            }
            if let Some(entry) = self.entry(submission.assignment) {
                return match entry {
                    Entry::Submitted { value, accepted_revision } if value.as_ref() == submission => {
                        Ok(ApplyOutcome { revision: *accepted_revision, duplicate: true })
                    }
                    _ => Err(Error::Conflict),
                };
            }
        }
        // Independent immutable submissions may arrive after a peer's submission or a
        // heartbeat. Assignment + epoch + payload are their preconditions. Metadata and
        // moderator transitions retain compare-and-set semantics.
        let immutable = matches!(action, Action::Submit { .. } | Action::FreezeReport { .. });
        if expected_revision > self.state.revision || (!immutable && expected_revision != self.state.revision) {
            return Err(Error::Conflict);
        }
        let mut candidate = self.clone();
        candidate.check_time(now_ms)?;
        candidate.apply_inner(actor, now_ms, action)?;
        candidate.bump()?;
        let result = ApplyOutcome { revision: candidate.revision(), duplicate: false };
        *self = candidate;
        Ok(result)
    }

    fn apply_inner(&mut self, actor: ParticipantId, now_ms: u64, action: Action) -> Result<()> {
        if let Action::Join { participant } = action {
            participant.validate()?;
            if participant.id != actor {
                return Err(Error::Unauthorized);
            }
            if self.state.members.iter().chain(&self.state.waiting).any(|m| m.participant.id == actor || m.participant.device == participant.device) {
                return Err(Error::Conflict);
            }
            if self.state.members.len().saturating_add(self.state.waiting.len()) >= MAX_PLAYERS {
                return Err(Error::Limit("players"));
            }
            let member = Member { participant, connected: true, ready: false };
            if self.phase() == Phase::Lobby {
                self.state.members.push(member);
            } else {
                self.state.waiting.push(member);
            }
            return Ok(());
        }
        if self.state.waiting.iter().any(|m| m.participant.id == actor) {
            if matches!(action, Action::Leave) {
                self.state.waiting.retain(|m| m.participant.id != actor);
                return Ok(());
            }
            return Err(Error::Phase);
        }
        if !self.member(actor)?.connected {
            return Err(Error::Unauthorized);
        }
        match action {
            Action::SetOptions { mode, timer_ms } => {
                self.require_moderator(actor)?;
                if self.phase() != Phase::Lobby {
                    return Err(Error::Phase);
                }
                mode.validate()?;
                validate_timer(timer_ms)?;
                self.state.mode = mode;
                self.state.timer_ms = timer_ms;
                for member in &mut self.state.members {
                    member.ready = false;
                }
            }
            Action::Leave => {
                if self.phase() == Phase::Lobby {
                    // The final member disconnects so the durable room remains recoverable.
                    if self.state.members.len() == 1 {
                        self.member_mut(actor)?.connected = false;
                        self.member_mut(actor)?.ready = false;
                    } else {
                        self.state.members.retain(|m| m.participant.id != actor);
                        if self.state.moderator == actor {
                            self.state.moderator = self.state.members.first().ok_or(Error::Invalid("roster"))?.participant.id;
                        }
                    }
                } else {
                    self.member_mut(actor)?.connected = false;
                    if matches!(self.phase(), Phase::Drawing | Phase::Transfer) {
                        self.pause(now_ms, PauseReason::Disconnected);
                    }
                }
            }
            Action::NextGame => {
                self.require_moderator(actor)?;
                if !matches!(self.phase(), Phase::Reveal | Phase::Archived) {
                    return Err(Error::Phase);
                }
                self.state.members.append(&mut self.state.waiting);
                for member in &mut self.state.members {
                    member.ready = false;
                }
                self.state.phase = Phase::Lobby;
                self.state.game = None;
                self.state.round = 0;
                self.state.epoch = 0;
                self.state.assignments.clear();
                self.state.entries.clear();
                self.clear_timer();
            }
            Action::Ready { ready } => {
                if self.phase() != Phase::Lobby {
                    return Err(Error::Phase);
                }
                self.member_mut(actor)?.ready = ready;
            }
            Action::Start { game, projects } => {
                self.require_moderator(actor)?;
                if self.phase() != Phase::Lobby || self.state.members.len() < 2 || self.state.members.iter().any(|m| !m.connected || !m.ready) {
                    return Err(Error::Phase);
                }
                let rounds = self.state.mode.rounds(self.state.members.len())?;
                let count = usize::from(rounds).checked_mul(self.state.members.len()).ok_or(Error::Limit("assignments"))?;
                if projects.len() != count || projects.iter().collect::<BTreeSet<_>>().len() != count {
                    return Err(Error::Invalid("project IDs must be unique and cover every assignment"));
                }
                let mut projects = projects.into_iter();
                for round in 0..rounds {
                    for chain in 0..self.state.members.len() {
                        let player_index = (chain + usize::from(round)) % self.state.members.len();
                        let player = self.state.members.get(player_index).ok_or(Error::Invalid("roster"))?.participant.id;
                        let project = projects.next().ok_or(Error::Invalid("projects"))?;
                        self.state.assignments.push(Assignment {
                            id: AssignmentId { game, chain: u8::try_from(chain).map_err(|_| Error::Limit("chain"))?, round },
                            player,
                            project,
                        });
                    }
                }
                self.state.game = Some(game);
                self.state.phase = Phase::Drawing;
                self.next_epoch()?;
                self.start_timer(now_ms, self.state.timer_ms)?;
            }
            Action::Submit { epoch, submission } => {
                if !matches!(self.phase(), Phase::Drawing | Phase::Transfer) || epoch != self.epoch() {
                    return Err(Error::Phase);
                }
                if self.state.paused.is_none() && self.state.deadline_ms.is_some_and(|deadline| now_ms >= deadline) {
                    return Err(Error::Phase); // Authority must process the due tick first.
                }
                submission.validate()?;
                if self.phase() == Phase::Transfer && !self.state.frozen.iter().any(|frozen| frozen == &submission) {
                    return Err(Error::Conflict);
                }
                let assignment = self.current_assignment(actor)?;
                if submission.assignment != assignment.id || submission.author != actor || submission.project != assignment.project {
                    return Err(Error::Unauthorized);
                }
                if self.state.entries.iter().any(|e| matches!(e, Entry::Submitted { value, .. } if value.id == submission.id)) {
                    return Err(Error::Conflict);
                }
                if matches!(self.state.mode, Mode::ExquisiteCorpse) && submission.seam.is_none() {
                    return Err(Error::Invalid("corpse submission requires a derived seam"));
                }
                let accepted_revision = self.revision().checked_add(1).ok_or(Error::Limit("revision"))?;
                self.state.entries.push(Entry::Submitted { value: Box::new(submission), accepted_revision });
                self.advance_if_complete(now_ms)?;
            }
            Action::FreezeReport { epoch, submission } => {
                if self.phase() != Phase::Transfer
                    || epoch != self.epoch()
                    || self.state.paused.is_none() && self.state.deadline_ms.is_some_and(|deadline| now_ms >= deadline)
                {
                    return Err(Error::Phase);
                }
                submission.validate()?;
                let assignment = self.current_assignment(actor)?;
                if submission.assignment != assignment.id || submission.author != actor || submission.project != assignment.project {
                    return Err(Error::Unauthorized);
                }
                if self.entry(assignment.id).is_some() {
                    return Err(Error::Conflict);
                }
                if let Some(frozen) = self.state.frozen.iter().find(|frozen| frozen.assignment == submission.assignment) {
                    if frozen != &submission {
                        return Err(Error::Conflict);
                    }
                } else {
                    self.state.frozen.push(submission);
                }
            }
            Action::Pause => {
                self.require_moderator(actor)?;
                self.require_active()?;
                self.pause(now_ms, PauseReason::Moderator);
            }
            Action::Resume => {
                self.require_moderator(actor)?;
                self.require_active()?;
                if self.state.paused.is_none() {
                    return Err(Error::Phase);
                }
                if self.state.members.iter().any(|m| !m.connected && self.current_assignment(m.participant.id).is_ok_and(|a| self.entry(a.id).is_none())) {
                    return Err(Error::Phase);
                }
                self.state.paused = None;
                let remaining = self.state.remaining_ms.take();
                self.start_timer(now_ms, remaining)?;
                self.advance_if_complete(now_ms)?;
            }
            Action::ClaimModerator => {
                if self.member(self.state.moderator)?.connected {
                    return Err(Error::Conflict);
                }
                self.state.moderator = actor;
            }
            Action::Skip { participant, reason } => {
                self.require_moderator(actor)?;
                self.require_active()?;
                validate_reason(&reason)?;
                let assignment = self.current_assignment(participant)?.id;
                if self.entry(assignment).is_some() {
                    return Err(Error::Conflict);
                }
                self.state.entries.push(Entry::Missing { assignment, reason });
                self.advance_if_complete(now_ms)?;
            }
            Action::End { reason } => {
                self.require_moderator(actor)?;
                self.require_active()?;
                validate_reason(&reason)?;
                for assignment in self.state.assignments.clone() {
                    if self.entry(assignment.id).is_none() {
                        self.state.entries.push(Entry::Missing { assignment: assignment.id, reason: reason.clone() });
                    }
                }
                self.state.phase = Phase::Archived;
                self.clear_timer();
            }
            Action::Archive => {
                self.require_moderator(actor)?;
                if self.phase() != Phase::Reveal {
                    return Err(Error::Phase);
                }
                self.state.phase = Phase::Archived;
            }
            Action::Join { .. } => return Err(Error::Invalid("join dispatch")),
        }
        Ok(())
    }

    /// Trusted connection observation, never a remotely accepted role claim.
    pub fn set_connected(&mut self, participant: ParticipantId, connected: bool, now_ms: u64) -> Result<()> {
        let mut candidate = self.clone();
        candidate.check_time(now_ms)?;
        let waiting = candidate.state.waiting.iter().any(|m| m.participant.id == participant);
        let member = if waiting {
            candidate.state.waiting.iter_mut().find(|m| m.participant.id == participant).ok_or(Error::Unauthorized)?
        } else {
            candidate.member_mut(participant)?
        };
        if member.connected == connected {
            return Ok(());
        }
        member.connected = connected;
        if !connected && !waiting && matches!(candidate.phase(), Phase::Drawing | Phase::Transfer) {
            candidate.pause(now_ms, PauseReason::Disconnected);
        }
        candidate.bump()?;
        *self = candidate;
        Ok(())
    }

    /// A deadline tick freezes drawing, then a separate transfer deadline labels missing work.
    pub fn tick(&mut self, now_ms: u64) -> Result<bool> {
        let mut candidate = self.clone();
        candidate.check_time(now_ms)?;
        if candidate.state.paused.is_some() || candidate.state.deadline_ms.is_none_or(|d| now_ms < d) {
            // Observing time alone is not an accepted aggregate revision.
            self.state.last_now_ms = now_ms;
            return Ok(false);
        }
        match candidate.phase() {
            Phase::Drawing => {
                candidate.state.phase = Phase::Transfer;
                candidate.start_timer(now_ms, Some(TRANSFER_GRACE_MS))?;
            }
            Phase::Transfer => {
                let round = candidate.state.round;
                for assignment in candidate.state.assignments.clone().into_iter().filter(|a| a.id.round == round) {
                    if candidate.entry(assignment.id).is_none() {
                        candidate.state.entries.push(Entry::Missing { assignment: assignment.id, reason: "Transfer deadline elapsed".into() });
                    }
                }
                candidate.advance_if_complete(now_ms)?;
            }
            _ => return Err(Error::Phase),
        }
        candidate.bump()?;
        *self = candidate;
        Ok(true)
    }

    /// Retain countdown duration; a new monotonic clock origin never expires saved work.
    pub fn restart(&mut self, now_ms: u64) -> Result<()> {
        let mut candidate = self.clone();
        if matches!(candidate.phase(), Phase::Drawing | Phase::Transfer) {
            candidate.pause(candidate.state.last_now_ms, PauseReason::Restart);
            candidate.state.paused = Some(PauseReason::Restart);
        }
        for member in candidate.state.members.iter_mut().chain(&mut candidate.state.waiting) {
            member.connected = false;
        }
        candidate.state.last_now_ms = now_ms;
        candidate.bump()?;
        *self = candidate;
        Ok(())
    }

    pub fn snapshot_bytes(&self) -> Result<Vec<u8>> {
        let bytes = serde_json::to_vec(&self.state).map_err(|e| Error::Json(e.to_string()))?;
        if bytes.len() > MAX_ROOM_SNAPSHOT_BYTES {
            return Err(Error::Limit("room snapshot"));
        }
        Ok(bytes)
    }

    /// Decode stored state without running user commands. Caller invokes `restart` before serving.
    pub fn from_snapshot(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > MAX_ROOM_SNAPSHOT_BYTES {
            return Err(Error::Limit("room snapshot"));
        }
        let state: State = serde_json::from_slice(bytes).map_err(|e| Error::Json(e.to_string()))?;
        let room = Self { state };
        room.validate_state()?;
        Ok(room)
    }

    pub fn view(&self, participant: ParticipantId) -> Result<RoomView> {
        let waiting = self.state.waiting.iter().any(|m| m.participant.id == participant);
        if !waiting {
            self.member(participant)?;
        }
        let assignment = if !waiting && matches!(self.phase(), Phase::Drawing | Phase::Transfer) {
            let assignment = self.current_assignment(participant)?.clone();
            let reference = if assignment.id.round == 0 {
                None
            } else {
                let previous = AssignmentId { round: assignment.id.round - 1, ..assignment.id };
                Some(match self.entry(previous) {
                    Some(Entry::Submitted { value, .. }) => match self.state.mode {
                        Mode::Animation { .. } => Reference::Animation { image: value.image, note: value.note.clone() },
                        Mode::ExquisiteCorpse => Reference::Seam { image: value.seam.ok_or(Error::Invalid("missing derived seam"))?, width: 1024, height: 24 },
                    },
                    _ => Reference::Missing,
                })
            };
            Some(AssignmentView { submitted: self.entry(assignment.id).is_some(), assignment, reference })
        } else {
            None
        };
        let mut reveal = Vec::new();
        if !waiting && matches!(self.phase(), Phase::Reveal | Phase::Archived) {
            for assignment in &self.state.assignments {
                match self.entry(assignment.id) {
                    Some(Entry::Submitted { value, .. }) => reveal.push(RevealedEntry::Artwork {
                        assignment: assignment.clone(),
                        revision: value.revision,
                        image: value.image,
                        checkpoint: value.checkpoint,
                        replay: Box::new(value.replay),
                        assets: value.assets.clone(),
                    }),
                    Some(Entry::Missing { reason, .. }) => reveal.push(RevealedEntry::Missing { assignment: assignment.clone(), reason: reason.clone() }),
                    None => return Err(Error::Invalid("incomplete terminal state")),
                }
            }
        }
        Ok(RoomView {
            id: self.id(),
            revision: self.revision(),
            epoch: self.epoch(),
            phase: self.phase(),
            round: self.state.round,
            mode: self.state.mode,
            timer_ms: self.state.timer_ms,
            moderator: self.state.moderator,
            members: self.state.members.iter().map(|m| MemberView { participant: m.participant.clone(), connected: m.connected, ready: m.ready }).collect(),
            waiting: self.state.waiting.iter().map(|m| MemberView { participant: m.participant.clone(), connected: m.connected, ready: m.ready }).collect(),
            paused: self.state.paused,
            deadline_ms: self.state.deadline_ms,
            assignment,
            reveal,
        })
    }

    pub fn notes(&self, participant: ParticipantId) -> Result<Vec<(AssignmentId, String)>> {
        self.member(participant)?;
        if !matches!(self.phase(), Phase::Reveal | Phase::Archived) {
            return Err(Error::Phase);
        }
        Ok(self
            .state
            .entries
            .iter()
            .filter_map(|entry| match entry {
                Entry::Submitted { value, .. } => Some((value.assignment, value.note.clone())),
                _ => None,
            })
            .collect())
    }

    /// Hash knowledge confers no access. Every fetch must check the current projection.
    pub fn can_fetch(&self, participant: ParticipantId, hash: AssetHash) -> bool {
        let Ok(view) = self.view(participant) else {
            return false;
        };
        if let Some(assignment) = &view.assignment {
            if assignment.reference.as_ref().is_some_and(|reference| match reference {
                Reference::Animation { image, .. } | Reference::Seam { image, .. } => image.hash == hash,
                Reference::Missing => false,
            }) {
                return true;
            }
            if let Some(Entry::Submitted { value, .. }) = self.entry(assignment.assignment.id)
                && submission_has_asset(value, hash)
            {
                return true;
            }
        }
        view.reveal.iter().any(|entry| match entry {
            RevealedEntry::Artwork { image, checkpoint, replay, assets, .. } => {
                image.hash == hash || checkpoint.asset.hash == hash || replay.baseline.asset.hash == hash || assets.iter().any(|asset| asset.hash == hash)
            }
            _ => false,
        })
    }

    fn validate_state(&self) -> Result<()> {
        let state = &self.state;
        if state.schema_version != ROOM_SCHEMA {
            return Err(Error::Version(state.schema_version));
        }
        state.mode.validate()?;
        validate_timer(state.timer_ms)?;
        if state.members.is_empty() || state.members.len().saturating_add(state.waiting.len()) > MAX_PLAYERS {
            return Err(Error::Limit("players"));
        }
        let mut participants = BTreeSet::new();
        let mut devices = BTreeSet::new();
        for member in state.members.iter().chain(&state.waiting) {
            member.participant.validate()?;
            if !participants.insert(member.participant.id) || !devices.insert(member.participant.device) {
                return Err(Error::Conflict);
            }
        }
        self.member(state.moderator)?;
        if state.paused.is_some() && state.deadline_ms.is_some() || state.paused.is_none() && state.remaining_ms.is_some() {
            return Err(Error::Invalid("timer pause state"));
        }
        if state.remaining_ms.is_some_and(|t| t > MAX_TIMER_MS.max(TRANSFER_GRACE_MS)) {
            return Err(Error::Limit("remaining timer"));
        }
        if state.phase == Phase::Lobby {
            if !state.waiting.is_empty()
                || state.game.is_some()
                || !state.assignments.is_empty()
                || !state.entries.is_empty()
                || !state.frozen.is_empty()
                || state.round != 0
                || state.epoch != 0
                || state.paused.is_some()
                || state.deadline_ms.is_some()
            {
                return Err(Error::Invalid("lobby state"));
            }
            return Ok(());
        }
        if state.members.len() < 2 {
            return Err(Error::Invalid("game roster"));
        }
        let rounds = state.mode.rounds(state.members.len())?;
        let game = state.game.ok_or(Error::Invalid("game ID"))?;
        if state.round >= rounds || state.epoch != u64::from(state.round) + 1 || state.assignments.len() != usize::from(rounds) * state.members.len() {
            return Err(Error::Invalid("assignment schedule"));
        }
        let mut projects = BTreeSet::new();
        for (index, assignment) in state.assignments.iter().enumerate() {
            assignment.id.validate()?;
            let round = index / state.members.len();
            let chain = index % state.members.len();
            let member = state.members.get((chain + round) % state.members.len()).ok_or(Error::Invalid("roster"))?;
            if assignment.id.game != game
                || usize::from(assignment.id.round) != round
                || usize::from(assignment.id.chain) != chain
                || assignment.player != member.participant.id
                || !projects.insert(assignment.project)
            {
                return Err(Error::Invalid("assignment schedule"));
            }
        }
        let mut seen = BTreeSet::new();
        let mut submission_ids = BTreeSet::new();
        if state.entries.len() > state.assignments.len() {
            return Err(Error::Limit("submissions"));
        }
        if state.frozen.len() > state.members.len() || !state.frozen.is_empty() && state.phase != Phase::Transfer {
            return Err(Error::Invalid("frozen manifests"));
        }
        let mut frozen_assignments = BTreeSet::new();
        for frozen in &state.frozen {
            frozen.validate()?;
            let assignment = self.current_assignment(frozen.author)?;
            if frozen.assignment != assignment.id || frozen.project != assignment.project || !frozen_assignments.insert(frozen.assignment) {
                return Err(Error::Invalid("frozen manifest ownership"));
            }
            if let Some(Entry::Submitted { value, .. }) = self.entry(frozen.assignment)
                && value.as_ref() != frozen
            {
                return Err(Error::Conflict);
            }
        }
        for entry in &state.entries {
            let id = entry.assignment();
            let assignment = state.assignments.iter().find(|a| a.id == id).ok_or(Error::Invalid("unknown assignment"))?;
            if !seen.insert(id) || (id.round > state.round && state.phase != Phase::Archived) {
                return Err(Error::Conflict);
            }
            match entry {
                Entry::Submitted { value, accepted_revision } => {
                    value.validate()?;
                    if value.author != assignment.player
                        || id.round > state.round
                        || value.project != assignment.project
                        || *accepted_revision == 0
                        || *accepted_revision > state.revision
                        || !submission_ids.insert(value.id)
                        || matches!(state.mode, Mode::ExquisiteCorpse) && value.seam.is_none()
                    {
                        return Err(Error::Invalid("accepted submission"));
                    }
                }
                Entry::Missing { reason, .. } => validate_reason(reason)?,
            }
        }
        let terminal = matches!(state.phase, Phase::Reveal | Phase::Archived);
        if state.assignments.iter().any(|a| (a.id.round < state.round || terminal) && !seen.contains(&a.id)) {
            return Err(Error::Invalid("missing historical outcome"));
        }
        if state.phase == Phase::Reveal && state.round != rounds - 1
            || terminal && (state.paused.is_some() || state.deadline_ms.is_some() || state.remaining_ms.is_some())
        {
            return Err(Error::Invalid("terminal state"));
        }
        if matches!(state.phase, Phase::Drawing | Phase::Transfer) && state.paused.is_none() {
            let timed = state.phase == Phase::Transfer || state.timer_ms.is_some();
            if timed != state.deadline_ms.is_some() {
                return Err(Error::Invalid("active timer"));
            }
            if state.assignments.iter().filter(|a| a.id.round == state.round).all(|a| seen.contains(&a.id)) {
                return Err(Error::Invalid("completed round must advance"));
            }
            if state
                .members
                .iter()
                .any(|member| !member.connected && self.current_assignment(member.participant.id).is_ok_and(|assignment| !seen.contains(&assignment.id)))
            {
                return Err(Error::Invalid("missing player must pause"));
            }
        }
        if matches!(state.phase, Phase::Drawing | Phase::Transfer) {
            let duration = if state.phase == Phase::Transfer { Some(TRANSFER_GRACE_MS) } else { state.timer_ms };
            let remaining = state.deadline_ms.map(|deadline| deadline.saturating_sub(state.last_now_ms)).or(state.remaining_ms);
            if duration.is_some() != remaining.is_some() || remaining.zip(duration).is_some_and(|(remaining, duration)| remaining > duration) {
                return Err(Error::Invalid("countdown duration"));
            }
        }
        Ok(())
    }

    fn member(&self, id: ParticipantId) -> Result<&Member> {
        self.state.members.iter().find(|m| m.participant.id == id).ok_or(Error::Unauthorized)
    }
    fn member_mut(&mut self, id: ParticipantId) -> Result<&mut Member> {
        self.state.members.iter_mut().find(|m| m.participant.id == id).ok_or(Error::Unauthorized)
    }
    fn current_assignment(&self, id: ParticipantId) -> Result<&Assignment> {
        self.state.assignments.iter().find(|a| a.player == id && a.id.round == self.state.round).ok_or(Error::Unauthorized)
    }
    fn entry(&self, id: AssignmentId) -> Option<&Entry> {
        self.state.entries.iter().find(|e| e.assignment() == id)
    }
    fn require_moderator(&self, actor: ParticipantId) -> Result<()> {
        if actor == self.state.moderator { Ok(()) } else { Err(Error::Unauthorized) }
    }
    fn require_active(&self) -> Result<()> {
        if matches!(self.phase(), Phase::Drawing | Phase::Transfer) { Ok(()) } else { Err(Error::Phase) }
    }
    fn check_time(&mut self, now_ms: u64) -> Result<()> {
        if now_ms < self.state.last_now_ms {
            return Err(Error::Invalid("clock moved backwards"));
        }
        self.state.last_now_ms = now_ms;
        Ok(())
    }
    fn bump(&mut self) -> Result<()> {
        self.state.revision = self.state.revision.checked_add(1).ok_or(Error::Limit("revision"))?;
        Ok(())
    }
    fn next_epoch(&mut self) -> Result<()> {
        self.state.epoch = self.state.epoch.checked_add(1).ok_or(Error::Limit("phase epoch"))?;
        Ok(())
    }
    fn start_timer(&mut self, now_ms: u64, duration: Option<u64>) -> Result<()> {
        self.state.deadline_ms = duration.map(|ms| now_ms.checked_add(ms).ok_or(Error::Limit("deadline"))).transpose()?;
        self.state.remaining_ms = None;
        Ok(())
    }
    fn pause(&mut self, now_ms: u64, reason: PauseReason) {
        if self.state.paused.is_none() {
            self.state.remaining_ms = self.state.deadline_ms.take().map(|deadline| deadline.saturating_sub(now_ms));
            self.state.paused = Some(reason);
        }
    }
    fn clear_timer(&mut self) {
        self.state.paused = None;
        self.state.deadline_ms = None;
        self.state.remaining_ms = None;
        self.state.frozen.clear();
    }
    fn advance_if_complete(&mut self, now_ms: u64) -> Result<()> {
        if self.state.paused.is_some() || self.state.assignments.iter().any(|a| a.id.round == self.state.round && self.entry(a.id).is_none()) {
            return Ok(());
        }
        let rounds = self.state.mode.rounds(self.state.members.len())?;
        if self.state.round.checked_add(1).ok_or(Error::Limit("round"))? == rounds {
            self.state.phase = Phase::Reveal;
            self.clear_timer();
        } else {
            self.state.round = self.state.round.checked_add(1).ok_or(Error::Limit("round"))?;
            self.next_epoch()?;
            self.state.phase = Phase::Drawing;
            self.state.frozen.clear();
            self.start_timer(now_ms, self.state.timer_ms)?;
            if self.state.members.iter().any(|member| !member.connected) {
                self.pause(now_ms, PauseReason::Disconnected);
            }
        }
        Ok(())
    }
}

fn validate_timer(timer: Option<u64>) -> Result<()> {
    if timer.is_some_and(|ms| ms == 0 || ms > MAX_TIMER_MS) {
        return Err(Error::Limit("round timer"));
    }
    Ok(())
}

fn validate_reason(reason: &str) -> Result<()> {
    if reason.trim().is_empty() || reason.len() > MAX_REASON_BYTES || reason.chars().any(char::is_control) {
        return Err(Error::Invalid("missing-submission reason"));
    }
    Ok(())
}

fn submission_has_asset(submission: &Submission, hash: AssetHash) -> bool {
    submission.image.hash == hash
        || submission.checkpoint.asset.hash == hash
        || submission.replay.baseline.asset.hash == hash
        || submission.assets.iter().any(|asset| asset.hash == hash)
        || submission.seam.is_some_and(|asset| asset.hash == hash)
}
