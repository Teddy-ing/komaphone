use super::error;
use crate::{Result, Session};
use photocraft_local_store::{
    Error as StoreError,
    canvas::{
        CanvasStore,
        linking::{LinkAuthority, LinkCredentials, LinkReceipt, LinkTarget},
    },
};
use photocraft_room_client::{
    CredentialStore,
    world_worker::{VerifiedWorldIdentity, WorldAccess},
};
use photocraft_scene::OwnerId;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, TryRecvError},
    },
};

#[derive(Default)]
pub(in crate::canvases) struct State {
    access: Option<WorldAccess>,
    pub certificate: Option<Vec<u8>>,
    generation: Arc<AtomicU64>,
    pending: Option<Pending>,
    pub committing: bool,
}
struct Pending {
    owner: OwnerId,
    rollback: bool,
    generation: u64,
    reply: Receiver<std::result::Result<VerifiedWorldIdentity, String>>,
    verified: Option<VerifiedWorldIdentity>,
}
pub(in crate::canvases) struct Commit {
    proof: VerifiedWorldIdentity,
    owner: OwnerId,
    rollback: bool,
    activation: Arc<AtomicU64>,
    pub generation: u64,
}
impl State {
    pub fn busy(&self) -> bool {
        self.pending.is_some() || self.committing
    }
    pub fn set_access(&mut self, access: Option<&WorldAccess>) {
        let same = access.zip(self.access.as_ref()).is_some_and(|(a, b)| a.scope() == b.scope());
        if !same {
            let _ = self.generation.fetch_update(Ordering::AcqRel, Ordering::Acquire, |value| value.checked_add(1));
            self.pending = None;
        }
        self.access = access.map(WorldAccess::copy_for_worker);
    }
}
pub(super) fn begin(session: &mut Session, rollback: bool) -> Result<()> {
    if !session.canvases.available() {
        return Err(error("Local canvas storage is unavailable"));
    }
    if session.canvases.link.busy {
        return Err(error("A local identity link is already in progress"));
    }
    let owner = session.canvases.owner.ok_or_else(|| error("Local canvas identity is still loading"))?;
    let state = &mut session.canvases.link_state;
    let access = state.access.as_ref().ok_or_else(|| error("Connect to your paired private-service profile before linking"))?;
    let account = session.worlds.account.as_ref().ok_or_else(|| error("Wait for the current private-service account to connect"))?;
    if access.scope().endpoint != account.endpoint || access.scope().profile != account.profile || access.scope().device != account.device {
        return Err(error("The active account changed; wait for its protected credential handoff"));
    }
    let access = access.copy_for_worker();
    let certificate = state.certificate.clone();
    let generation = state.generation.load(Ordering::Acquire);
    let active = Arc::clone(&state.generation);
    let (sender, reply) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("komaphone-canvas-link-verify".into())
        .spawn(move || {
            if active.load(Ordering::Acquire) != generation {
                let _ = sender.send(Err("Account changed before verification; local artwork remains unlinked".into()));
                return;
            }
            let result = access.verify_identity(certificate.as_deref()).map_err(|e| e.to_string()).and_then(|proof| {
                if active.load(Ordering::Acquire) != generation {
                    Err("Account changed during verification; local artwork remains unlinked".into())
                } else {
                    Ok(proof)
                }
            });
            let _ = sender.send(result);
        })
        .map_err(error)?;
    state.pending = Some(Pending { owner, rollback, generation, reply, verified: None });
    session.canvases.link.busy = true;
    session.canvases.link.error = None;
    Ok(())
}
pub(super) fn poll(session: &mut Session) {
    let state = &mut session.canvases.link_state;
    if let Some(pending) = &mut state.pending
        && pending.verified.is_none()
    {
        match pending.reply.try_recv() {
            Ok(Ok(proof)) => pending.verified = Some(proof),
            Ok(Err(error)) => {
                state.pending = None;
                session.canvases.link.busy = false;
                session.canvases.link.error = Some(error);
                return;
            }
            Err(TryRecvError::Disconnected) => {
                state.pending = None;
                session.canvases.link.busy = false;
                session.canvases.link.error = Some("Identity verifier stopped; local artwork remains intact".into());
                return;
            }
            Err(TryRecvError::Empty) => return,
        }
    }
    if session.canvases.has_pending() || session.canvases.link_state.committing {
        return;
    }
    let Some(mut pending) = session.canvases.link_state.pending.take() else {
        return;
    };
    let Some(proof) = pending.verified.take() else {
        session.canvases.link_state.pending = Some(pending);
        return;
    };
    if !proof.fresh() || pending.generation != session.canvases.link_state.generation.load(Ordering::Acquire) {
        session.canvases.link.busy = false;
        session.canvases.link.error = Some("Identity verification expired or account changed; retry linking".into());
        return;
    }
    let commit = Commit {
        proof,
        owner: pending.owner,
        rollback: pending.rollback,
        activation: Arc::clone(&session.canvases.link_state.generation),
        generation: pending.generation,
    };
    let result = session
        .canvases
        .worker
        .as_ref()
        .ok_or_else(|| error("Canvas worker is unavailable"))
        .and_then(|worker| worker.send(super::super::native::Request::Link(commit)));
    match result {
        Ok(()) => session.canvases.link_state.committing = true,
        Err(error) => {
            session.canvases.link.busy = false;
            session.canvases.link.error = Some(error.to_string());
        }
    }
}
struct Credentials<'a>(&'a mut dyn CredentialStore);
impl LinkCredentials for Credentials<'_> {
    fn get(&mut self, namespace: &str, key: &str) -> photocraft_local_store::Result<Option<String>> {
        self.0.get(namespace, key).map_err(|e| StoreError::Storage(e.to_string()))
    }
    fn set(&mut self, namespace: &str, key: &str, secret: &str) -> photocraft_local_store::Result<()> {
        self.0.set(namespace, key, secret).map_err(|e| StoreError::Storage(e.to_string()))
    }
}
impl Commit {
    fn current(&self) -> photocraft_local_store::Result<()> {
        if !self.proof.fresh() || self.activation.load(Ordering::Acquire) != self.generation {
            return Err(StoreError::Invalid("Account changed or verification expired before local linking committed".into()));
        }
        Ok(())
    }
    fn target(&self) -> photocraft_local_store::Result<LinkTarget> {
        let scope = self.proof.scope();
        let service =
            photocraft_party::AssetHash::new(&format!("{:x}", Sha256::digest(scope.endpoint.as_bytes()))).map_err(|e| StoreError::Invalid(e.to_string()))?;
        Ok(LinkTarget { service, profile: scope.profile, device: scope.device })
    }
}
impl LinkAuthority for Commit {
    fn authorize(&mut self, owner: OwnerId, nonce: &str) -> photocraft_local_store::Result<LinkTarget> {
        self.current()?;
        if owner != self.owner || photocraft_party::AssetHash::new(nonce).is_err() {
            return Err(StoreError::Invalid("Local owner or binding challenge changed".into()));
        }
        self.target()
    }
    fn confirm(&mut self, target: &LinkTarget) -> photocraft_local_store::Result<()> {
        self.current()?;
        if self.target()? != *target {
            return Err(StoreError::Invalid("Binding target changed before commit".into()));
        }
        Ok(())
    }
}
pub(in crate::canvases) fn commit(
    store: &mut CanvasStore,
    credentials: &mut dyn CredentialStore,
    mut proof: Commit,
    backup: &Path,
) -> photocraft_local_store::Result<LinkReceipt> {
    let mut credentials = Credentials(credentials);
    if proof.rollback { store.rollback_link(&mut credentials, &mut proof, backup) } else { store.link_identity(&mut credentials, &mut proof, backup) }
}
