//! Deterministic in-memory test transport/storage. NOT authentication, a disk store, or a
//! deployed service. A successful response here proves transaction ordering, not durability.
use crate::contracts::{
    AssetHash, AssetRef, AssignmentId, MAX_ASSET_BYTES, MAX_ASSETS, MAX_MESSAGE_BYTES, MAX_PROJECT_BYTES, ParticipantId, RequestId, Submission, decode_bounded,
    validate_version,
};
use crate::room::{Action, ApplyOutcome, Room};
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

const MAX_REQUESTS: usize = 4096;
const MAX_QUEUED: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema_version: u16,
    pub request_id: RequestId,
    /// Injected authenticated identity in K2; a network body must never choose its principal.
    pub actor: ParticipantId,
    pub expected_revision: u64,
    pub action: Action,
}
impl Request {
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let request: Self = decode_bounded(bytes)?;
        validate_version(request.schema_version)?;
        Ok(request)
    }
}

/// Content hashing is real; the fixture bytes need not be valid image/native documents.
/// K1/K2 must decode bounded headers and verify the rendered image/seam against the checkpoint.
#[derive(Debug, Clone, Default)]
pub struct MemoryAssets {
    objects: BTreeMap<AssetHash, Vec<u8>>,
    upload_grants: BTreeSet<(ParticipantId, AssetHash)>,
    seams: BTreeMap<AssignmentId, AssetRef>,
    bytes: u64,
}
impl MemoryAssets {
    pub fn insert_for(&mut self, authenticated_actor: ParticipantId, bytes: &[u8]) -> Result<AssetRef> {
        if bytes.is_empty() || bytes.len() as u64 > MAX_ASSET_BYTES {
            return Err(Error::Limit("asset bytes"));
        }
        let hash = AssetHash::new(&format!("{:x}", Sha256::digest(bytes)))?;
        let asset = AssetRef { hash, bytes: bytes.len() as u64 };
        asset.validate()?;
        let grant = (authenticated_actor, hash);
        if !self.upload_grants.contains(&grant) && self.upload_grants.len() >= MAX_ASSETS * 36 * 3 {
            return Err(Error::Limit("upload grants"));
        }
        if self.objects.contains_key(&hash) {
            self.upload_grants.insert(grant);
            return Ok(asset);
        }
        if self.objects.len() >= MAX_ASSETS * 36 {
            return Err(Error::Limit("stored assets"));
        }
        let next = self.bytes.checked_add(asset.bytes).ok_or(Error::Limit("asset store"))?;
        if next > MAX_PROJECT_BYTES {
            return Err(Error::Limit("asset store"));
        }
        self.objects.insert(hash, bytes.to_vec());
        self.upload_grants.insert(grant);
        self.bytes = next;
        Ok(asset)
    }
    /// Trusted fixture renderer hook. Never expose this as a client endpoint.
    pub fn register_derived_seam(&mut self, assignment: AssignmentId, asset: AssetRef) -> Result<()> {
        assignment.validate()?;
        self.require(asset)?;
        if let Some(previous) = self.seams.get(&assignment) {
            return if *previous == asset { Ok(()) } else { Err(Error::Conflict) };
        }
        if self.seams.len() >= 36 {
            return Err(Error::Limit("seam fixtures"));
        }
        self.seams.insert(assignment, asset);
        Ok(())
    }
    fn require(&self, asset: AssetRef) -> Result<()> {
        asset.validate()?;
        match self.objects.get(&asset.hash) {
            Some(bytes) if bytes.len() as u64 == asset.bytes => Ok(()),
            _ => Err(Error::Invalid("incomplete or mismatched asset")),
        }
    }
    fn validate_submission(&self, room: &Room, submission: &Submission) -> Result<()> {
        submission.validate()?;
        for asset in &submission.assets {
            self.require(*asset)?;
            if !self.upload_grants.contains(&(submission.author, asset.hash)) && !room.can_fetch(submission.author, asset.hash) {
                return Err(Error::Unauthorized);
            }
        }
        if let Some(seam) = submission.seam
            && self.seams.get(&submission.assignment) != Some(&seam)
        {
            return Err(Error::Unauthorized);
        }
        Ok(())
    }
    pub fn fetch<'a>(&'a self, room: &Room, actor: ParticipantId, hash: AssetHash) -> Result<&'a [u8]> {
        if !room.can_fetch(actor, hash) {
            return Err(Error::Unauthorized);
        }
        self.objects.get(&hash).map(Vec::as_slice).ok_or(Error::Invalid("missing asset"))
    }
}

/// Atomic compare-and-set plus deterministic failure injection. Requests and outcomes are
/// committed with the room snapshot, so lost replies can safely retry after restoration.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    snapshot: Vec<u8>,
    revision: u64,
    requests: BTreeMap<RequestId, (Request, ApplyOutcome)>,
    fail_next: bool,
}
impl MemoryStore {
    pub fn new(room: &Room) -> Result<Self> {
        Ok(Self { snapshot: room.snapshot_bytes()?, revision: room.revision(), requests: BTreeMap::new(), fail_next: false })
    }
    pub fn fail_next_commit(&mut self) {
        self.fail_next = true;
    }
    pub fn snapshot(&self) -> &[u8] {
        &self.snapshot
    }
    fn commit(&mut self, expected: u64, room: &Room, accepted: Option<(Request, ApplyOutcome)>) -> Result<()> {
        if self.revision != expected {
            return Err(Error::Conflict);
        }
        let snapshot = room.snapshot_bytes()?;
        if accepted.is_some() && self.requests.len() >= MAX_REQUESTS {
            return Err(Error::Limit("request receipts"));
        }
        if self.fail_next {
            self.fail_next = false;
            return Err(Error::Storage);
        }
        // No fallible operation after this point.
        self.snapshot = snapshot;
        self.revision = room.revision();
        if let Some((request, outcome)) = accepted {
            self.requests.insert(request.request_id, (request, outcome));
        }
        Ok(())
    }
}

pub struct MemoryAuthority {
    room: Room,
    store: MemoryStore,
    assets: MemoryAssets,
}
impl MemoryAuthority {
    pub fn new(room: Room) -> Result<Self> {
        let store = MemoryStore::new(&room)?;
        Ok(Self { room, store, assets: MemoryAssets::default() })
    }
    pub fn room(&self) -> &Room {
        &self.room
    }
    pub fn store(&self) -> &MemoryStore {
        &self.store
    }
    pub fn store_mut(&mut self) -> &mut MemoryStore {
        &mut self.store
    }
    pub fn assets_mut(&mut self) -> &mut MemoryAssets {
        &mut self.assets
    }
    pub fn fetch(&self, actor: ParticipantId, hash: AssetHash) -> Result<&[u8]> {
        self.assets.fetch(&self.room, actor, hash)
    }
    pub fn deliver(&mut self, authenticated_actor: ParticipantId, bytes: &[u8], now_ms: u64) -> Result<ApplyOutcome> {
        let request = Request::decode(bytes)?;
        if request.actor != authenticated_actor {
            return Err(Error::Unauthorized);
        }
        if let Some((prior, outcome)) = self.store.requests.get(&request.request_id) {
            return if prior == &request { Ok(*outcome) } else { Err(Error::Conflict) };
        }
        let mut candidate = self.room.clone();
        let outcome = candidate.apply(request.actor, request.expected_revision, now_ms, request.action.clone())?;
        // Accepted immutable content is already validated. A later identical retry must not
        // depend on a reference grant that expired when the room moved to another round.
        if !outcome.duplicate
            && let Action::Submit { submission, .. } = &request.action
        {
            self.assets.validate_submission(&self.room, submission)?;
        }
        self.store.commit(self.room.revision(), &candidate, Some((request, outcome)))?;
        self.room = candidate;
        Ok(outcome)
    }
    /// In-memory restart preserves receipts/assets. K2 replaces this with durable transactions.
    pub fn restart(&mut self, now_ms: u64) -> Result<()> {
        let mut room = Room::from_snapshot(self.store.snapshot())?;
        room.restart(now_ms)?;
        self.store.commit(self.room.revision(), &room, None)?;
        self.room = room;
        Ok(())
    }
    pub fn tick(&mut self, now_ms: u64) -> Result<bool> {
        let mut room = self.room.clone();
        let changed = room.tick(now_ms)?;
        // Also retain the latest trusted time observation for restart remaining-time calculation.
        self.store.commit(self.room.revision(), &room, None)?;
        self.room = room;
        Ok(changed)
    }
    /// Trusted adapter input after authenticating a stable participant on reconnect.
    pub fn set_connected(&mut self, actor: ParticipantId, connected: bool, now_ms: u64) -> Result<()> {
        let mut room = self.room.clone();
        room.set_connected(actor, connected, now_ms)?;
        self.store.commit(self.room.revision(), &room, None)?;
        self.room = room;
        Ok(())
    }
}

/// A bounded byte queue lets tests intentionally drop, duplicate, or reorder deliveries.
#[derive(Default)]
pub struct MemoryTransport {
    queue: VecDeque<Vec<u8>>,
}
impl MemoryTransport {
    pub fn push(&mut self, bytes: Vec<u8>) -> Result<()> {
        if bytes.len() > MAX_MESSAGE_BYTES || self.queue.len() >= MAX_QUEUED {
            return Err(Error::Limit("transport queue"));
        }
        self.queue.push_back(bytes);
        Ok(())
    }
    pub fn take(&mut self, index: usize) -> Result<Vec<u8>> {
        self.queue.remove(index).ok_or(Error::Invalid("queue index"))
    }
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
    }
}
