//! Transient ordered presence; never part of the durable world or its event sequence.
use super::{Limits, grant};
use crate::{Error, Result, Store};
use photocraft_party::Error as Domain;
use photocraft_scene::shared::{DeviceId, Edit, OpId, WorldOp};
use photocraft_scene::world_protocol::{self as wire, Action, Response};
use photocraft_scene::{CanvasId, OwnerId, StrokeId};
use std::collections::{BTreeMap, BTreeSet};

const PEER_TTL_MS: u64 = 2_000;
const LEASE_TTL_MS: u64 = 30_000;
const MAX_COMPLETED: usize = 128;
type Key = (CanvasId, DeviceId);

#[derive(Default)]
pub(crate) struct Presence {
    // One entry per connection, including leases whose visible peer has expired.
    connections: BTreeMap<Key, Connection>,
}
struct Connection {
    profile: OwnerId,
    epoch: u64,
    peer: Option<wire::Peer>,
    lease: Option<Lease>,
}
struct Lease {
    nonce: OpId,
    stream: OpId,
    serial: u64,
    observation: u64,
    expires_at_ms: u64,
    completed: BTreeSet<StrokeId>,
}

impl Presence {
    pub(super) fn remove_canvas(&mut self, canvas: CanvasId) {
        self.connections.retain(|(scope, _), _| *scope != canvas);
    }
    pub(super) fn remove_member(&mut self, canvas: CanvasId, profile: OwnerId) {
        self.connections.retain(|(scope, _), connection| *scope != canvas || connection.profile != profile);
    }
    pub(crate) fn remove_device(&mut self, device: &str) {
        self.connections.retain(|(_, id), _| id.as_str() != device);
    }
    pub(super) fn committed(&mut self, operation: &WorldOp) {
        let (Edit::AddStroke { stroke } | Edit::CopyStroke { stroke, .. }) = &operation.action else { return };
        self.connections.retain(|(canvas, device), connection| {
            if *canvas != operation.canvas {
                return true;
            }
            let matching = connection.peer.as_ref().is_some_and(|peer| peer.stroke.as_ref().is_some_and(|style| style.id == stroke.id));
            if matching && let Some(peer) = &mut connection.peer {
                peer.stroke = None;
                peer.preview.clear();
            }
            if (*device == operation.device || matching)
                && let Some(lease) = &mut connection.lease
            {
                if !lease.completed.contains(&stroke.id) && lease.completed.len() >= MAX_COMPLETED {
                    // Never evict a tombstone while its stream could still accept delayed packets.
                    // The durable save already succeeded; only this disposable connection rotates.
                    return false;
                }
                lease.completed.insert(stroke.id);
            }
            true
        });
    }
    fn prune(&mut self, now: u64) {
        self.connections.retain(|_, connection| {
            if connection.peer.as_ref().is_some_and(|peer| peer.expires_at_ms <= now) {
                connection.peer = None;
            }
            if connection.lease.as_ref().is_some_and(|lease| lease.expires_at_ms <= now) {
                connection.lease = None;
            }
            connection.peer.is_some() || connection.lease.is_some()
        });
    }
    fn capacity(&self, key: Key, limits: Limits) -> Result<()> {
        if !self.connections.contains_key(&key) && self.connections.len() >= limits.presence_connections as usize {
            return Err(Domain::Limit("concurrent presence connections").into());
        }
        Ok(())
    }
    fn lease(&self, key: Key, profile: OwnerId, epoch: u64, stream: OpId, now: u64) -> Result<&Lease> {
        self.connections
            .get(&key)
            .filter(|connection| connection.profile == profile && connection.epoch == epoch)
            .and_then(|connection| connection.lease.as_ref())
            .filter(|lease| lease.stream == stream && lease.expires_at_ms > now)
            .ok_or(Error::PresenceStream)
    }
}

fn random_stream() -> Result<OpId> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(|_| Error::Storage)?;
    let hex: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    OpId::new(&hex).map_err(|_| Error::Storage)
}
fn peer(profile: OwnerId, device: DeviceId, action: &Action, now: u64) -> Result<wire::Peer> {
    let (Action::Presence { canvas, camera, preview, cursor, stroke, .. } | Action::PresenceUpdate { canvas, camera, preview, cursor, stroke, .. }) = action
    else {
        return Err(Error::Invalid("presence sample"));
    };
    if camera.canvas != *canvas || preview.len() > wire::MAX_PREVIEW_POINTS || preview.iter().flatten().any(|n| !n.is_finite() || n.abs() > 8192.0) {
        return Err(Error::Invalid("canvas presence"));
    }
    photocraft_scene::coordinates::center(camera).map_err(|_| Error::Invalid("presence camera"))?;
    if let Some(cursor) = cursor {
        photocraft_scene::coordinates::validate_point(*cursor).map_err(|_| Error::Invalid("presence cursor"))?;
    }
    if let Some(style) = stroke {
        let candidate = photocraft_scene::Stroke {
            id: style.id,
            canvas: *canvas,
            owner: profile,
            revision: 0,
            anchor: style.anchor,
            points: preview.clone(),
            width: style.width,
            color: style.color,
            erase: style.erase,
            brush: style.brush.clone(),
            samples: style.samples.clone(),
        };
        candidate.validate_preview().map_err(|_| Error::Invalid("presence stroke preview"))?;
    } else if !preview.is_empty() {
        return Err(Error::Invalid("preview points require a stable stroke identity/style"));
    }
    Ok(wire::Peer {
        profile,
        device,
        camera: camera.clone(),
        preview: preview.clone(),
        cursor: *cursor,
        stroke: stroke.clone(),
        expires_at_ms: now.saturating_add(PEER_TTL_MS),
    })
}

impl Store {
    pub(super) fn world_presence_request(&mut self, profile: OwnerId, device: DeviceId, action: &Action, now: u64, limits: Limits) -> Result<Response> {
        let canvas = match action {
            Action::Presence { canvas, .. }
            | Action::Peers { canvas }
            | Action::PresenceOpen { canvas, .. }
            | Action::PresenceUpdate { canvas, .. }
            | Action::PresenceRead { canvas, .. } => *canvas,
            _ => return Err(Error::Invalid("presence action")),
        };
        // Authentication already checked the device. Current membership/epoch precedes all
        // transient mutation and stream errors, including expired or guessed stream tokens.
        let epoch = grant(&self.db, canvas, profile)?;
        if let Action::Presence { epoch: requested, .. } | Action::PresenceOpen { epoch: requested, .. } | Action::PresenceUpdate { epoch: requested, .. } =
            action
            && *requested != epoch
        {
            return Err(Domain::Unauthorized.into());
        }
        let key = (canvas, device);
        match action {
            Action::PresenceOpen { nonce, .. } => {
                if let Some(lease) = self
                    .world_presence
                    .connections
                    .get(&key)
                    .filter(|entry| entry.profile == profile && entry.epoch == epoch)
                    .and_then(|entry| entry.lease.as_ref())
                    .filter(|lease| lease.expires_at_ms > now && lease.nonce == *nonce)
                {
                    return Ok(Response::PresenceReady { canvas, epoch, nonce: *nonce, stream: lease.stream });
                }
                let stream = random_stream()?;
                self.world_presence.prune(now);
                self.world_presence.capacity(key, limits)?;
                self.world_presence.connections.insert(
                    key,
                    Connection {
                        profile,
                        epoch,
                        peer: None,
                        lease: Some(Lease {
                            nonce: *nonce,
                            stream,
                            serial: 0,
                            observation: 0,
                            expires_at_ms: now.saturating_add(LEASE_TTL_MS),
                            completed: BTreeSet::new(),
                        }),
                    },
                );
                Ok(Response::PresenceReady { canvas, epoch, nonce: *nonce, stream })
            }
            Action::PresenceUpdate { stream, serial, .. } => {
                let lease = self.world_presence.lease(key, profile, epoch, *stream, now)?;
                if *serial == 0 {
                    return Err(Error::Invalid("presence serial must be positive"));
                }
                let observation = lease.observation.checked_add(1).ok_or(Error::PresenceStream)?;
                let newer = *serial > lease.serial;
                let mut sample = peer(profile, device, action, now)?;
                if sample.stroke.as_ref().is_some_and(|style| lease.completed.contains(&style.id)) {
                    sample.stroke = None;
                    sample.preview.clear();
                }
                self.world_presence.prune(now);
                if newer {
                    let connection = self.world_presence.connections.get_mut(&key).ok_or(Error::PresenceStream)?;
                    let lease = connection.lease.as_mut().ok_or(Error::PresenceStream)?;
                    lease.serial = *serial;
                    lease.expires_at_ms = now.saturating_add(LEASE_TTL_MS);
                    connection.peer = Some(sample);
                }
                self.presence_snapshot(key, *stream, observation, now)
            }
            Action::PresenceRead { stream, .. } => {
                let observation = self.world_presence.lease(key, profile, epoch, *stream, now)?.observation.checked_add(1).ok_or(Error::PresenceStream)?;
                self.world_presence.prune(now);
                self.presence_snapshot(key, *stream, observation, now)
            }
            Action::Presence { .. } => {
                let sample = peer(profile, device, action, now)?;
                if self.world_presence.connections.get(&key).and_then(|entry| entry.lease.as_ref()).is_some_and(|lease| lease.expires_at_ms > now) {
                    return Err(Error::PresenceStream);
                }
                self.world_presence.prune(now);
                self.world_presence.capacity(key, limits)?;
                self.world_presence.connections.insert(key, Connection { profile, epoch, peer: Some(sample), lease: None });
                Ok(Response::Peers { peers: self.presence_peers(canvas, now)? })
            }
            Action::Peers { .. } => {
                self.world_presence.prune(now);
                Ok(Response::Peers { peers: self.presence_peers(canvas, now)? })
            }
            _ => Err(Error::Invalid("presence action")),
        }
    }
    fn presence_peers(&self, canvas: CanvasId, now: u64) -> Result<Vec<wire::Peer>> {
        let mut peers = Vec::new();
        for ((scope, device), connection) in &self.world_presence.connections {
            if *scope != canvas {
                continue;
            }
            let Some(peer) = connection.peer.as_ref().filter(|peer| peer.expires_at_ms > now) else { continue };
            let device_active: bool = self.db.query_row(
                "SELECT EXISTS(SELECT 1 FROM identity_devices WHERE device=?1 AND profile=?2 AND revoked=0)",
                rusqlite::params![device.as_str(), connection.profile.as_str()],
                |row| row.get(0),
            )?;
            if !device_active {
                continue;
            }
            match grant(&self.db, canvas, connection.profile) {
                Ok(epoch) if epoch == connection.epoch => peers.push(peer.clone()),
                Ok(_) | Err(Error::Domain(Domain::Unauthorized)) => {}
                Err(error) => return Err(error),
            }
        }
        Ok(peers)
    }
    fn presence_snapshot(&mut self, key: Key, stream: OpId, observation: u64, now: u64) -> Result<Response> {
        let peers = self.presence_peers(key.0, now)?;
        let lease = self.world_presence.connections.get_mut(&key).and_then(|entry| entry.lease.as_mut()).ok_or(Error::PresenceStream)?;
        lease.observation = observation;
        Ok(Response::PresenceSnapshot { canvas: key.0, stream, observation, peers })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use photocraft_party::{
        DeviceId as PartyDevice, Participant, ParticipantId, RequestId,
        protocol::{Operation, RpcRequest},
    };
    use photocraft_scene::{Srgba, coordinates};

    fn fixture() -> (tempfile::TempDir, Store, String, OwnerId, DeviceId, CanvasId) {
        let root = tempfile::tempdir().unwrap();
        let mut store = Store::open(root.path().join("service.db"), Default::default(), 0).unwrap();
        let profile = OwnerId::new(&format!("{:032x}", 1)).unwrap();
        let device = DeviceId::new(&format!("{:032x}", 2)).unwrap();
        let canvas = CanvasId::new(&format!("{:032x}", 3)).unwrap();
        let token = format!("{:064x}", 1000);
        store
            .execute(
                None,
                &RpcRequest {
                    protocol_version: 1,
                    request_id: RequestId::new(profile.as_str()).unwrap(),
                    operation: Operation::Pair {
                        participant: Participant {
                            id: ParticipantId::new(profile.as_str()).unwrap(),
                            device: PartyDevice::new(device.as_str()).unwrap(),
                            display_name: "Synthetic".into(),
                        },
                        credential: token.clone(),
                    },
                },
                100,
            )
            .unwrap();
        store
            .world_execute(&token, &wire::Request { version: wire::VERSION, action: Action::Create { canvas, name: "Bounded presence".into() } }, 100)
            .unwrap();
        (root, store, token, profile, device, canvas)
    }

    #[test]
    fn observation_exhaustion_does_not_wrap_or_mutate_a_sample_and_fresh_nonce_recovers() {
        let (_root, mut store, token, profile, device, canvas) = fixture();
        let nonce = OpId::new(&format!("{:032x}", 4)).unwrap();
        let ready = store.world_presence_request(profile, device, &Action::PresenceOpen { canvas, epoch: 1, nonce }, 100, Limits::default()).unwrap();
        let Response::PresenceReady { stream, .. } = ready else { panic!() };
        store.world_presence.connections.get_mut(&(canvas, device)).unwrap().lease.as_mut().unwrap().observation = u64::MAX;
        let action = Action::PresenceUpdate {
            canvas,
            epoch: 1,
            stream,
            serial: u64::MAX,
            camera: coordinates::home(canvas),
            preview: vec![],
            cursor: None,
            stroke: None,
        };
        assert!(matches!(store.world_execute(&token, &wire::Request { version: wire::VERSION, action }, 101), Err(Error::PresenceStream)));
        let connection = &store.world_presence.connections[&(canvas, device)];
        assert!(connection.peer.is_none());
        assert_eq!(connection.lease.as_ref().unwrap().serial, 0);
        let next = OpId::new(&format!("{:032x}", 5)).unwrap();
        let Response::PresenceReady { stream: fresh, .. } =
            store.world_presence_request(profile, device, &Action::PresenceOpen { canvas, epoch: 1, nonce: next }, 102, Limits::default()).unwrap()
        else {
            panic!()
        };
        assert_ne!(fresh, stream);
    }

    #[test]
    fn aggregate_snapshot_byte_limit_is_explicit_and_authorization_still_precedes_it() {
        let (_root, mut store, token, profile, device, canvas) = fixture();
        // Populate 512 valid device-bound samples directly so the regression exercises the
        // aggregate response bound without hundreds of unrelated durable setup requests.
        let tx = store.db.transaction().unwrap();
        for n in 10..522 {
            tx.execute(
                "INSERT INTO identity_devices(device,profile,verifier) VALUES (?1,?2,?3)",
                rusqlite::params![format!("{n:032x}"), profile.as_str(), format!("{n:064x}")],
            )
            .unwrap();
        }
        tx.commit().unwrap();
        for n in 10..522 {
            let other = DeviceId::new(&format!("{n:032x}")).unwrap();
            let action = Action::Presence {
                canvas,
                epoch: 1,
                camera: coordinates::home(canvas),
                preview: vec![[8191.123456789012, -8191.123456789012]; wire::MAX_PREVIEW_POINTS],
                cursor: None,
                stroke: Some(wire::PreviewStyle {
                    id: StrokeId::new(&format!("{n:032x}")).unwrap(),
                    anchor: Default::default(),
                    width: 1.,
                    color: Srgba { rgba: [0., 0., 0., 1.] },
                    erase: false,
                    brush: None,
                    samples: None,
                }),
            };
            let sample = peer(profile, other, &action, 100).unwrap();
            store.world_presence.connections.insert((canvas, other), Connection { profile, epoch: 1, peer: Some(sample), lease: None });
        }
        let nonce = OpId::new(&format!("{:032x}", 4)).unwrap();
        let Response::PresenceReady { stream, .. } = store
            .world_presence_request(
                profile,
                device,
                &Action::PresenceOpen { canvas, epoch: 1, nonce },
                100,
                Limits { presence_connections: 1024, ..Limits::default() },
            )
            .unwrap()
        else {
            panic!()
        };
        for action in [Action::Peers { canvas }, Action::PresenceRead { canvas, stream }] {
            assert!(matches!(store.world_execute(&token, &wire::Request { version: wire::VERSION, action }, 101), Err(Error::Domain(Domain::Limit(_)))));
        }
        assert!(matches!(
            store.world_execute(&format!("{:064x}", 9999), &wire::Request { version: wire::VERSION, action: Action::Peers { canvas } }, 101),
            Err(Error::Session)
        ));
        let Response::Canvas { info } = store.world_execute(&token, &wire::Request { version: wire::VERSION, action: Action::Info { canvas } }, 101).unwrap()
        else {
            panic!()
        };
        assert_eq!(info.revision, 0);
    }
}
