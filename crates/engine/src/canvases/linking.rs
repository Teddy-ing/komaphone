//! Explicit local-installation binding; canvas IDs and historical owners stay intact.
use crate::{EngineError, Result, Session};
use photocraft_scene::{OwnerId, shared::DeviceId};
use serde::Serialize;

#[cfg(not(target_arch = "wasm32"))]
pub(super) mod native;

#[derive(Clone, Debug, Serialize)]
pub struct Binding {
    pub local_owner: OwnerId,
    pub service: photocraft_party::AssetHash,
    pub profile: OwnerId,
    pub device: DeviceId,
    pub revision: u32,
    pub active: bool,
}
#[derive(Default, Serialize)]
pub struct Status {
    pub busy: bool,
    pub binding: Option<Binding>,
    pub error: Option<String>,
    pub backup: Option<String>,
}
impl Session {
    pub fn canvas_link_identity(&mut self, rollback: bool) -> Result<()> {
        #[cfg(not(target_arch = "wasm32"))]
        {
            native::begin(self, rollback)
        }
        #[cfg(target_arch = "wasm32")]
        {
            let _ = rollback;
            Err(EngineError::Other("Local identity linking requires the native app".into()))
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn configure_canvas_linking(&mut self, certificate: Option<Vec<u8>>) {
        self.canvases.link_state.certificate = certificate;
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub fn canvas_link_access(&mut self, access: Option<&photocraft_room_client::world_worker::WorldAccess>) {
        self.canvases.link_state.set_access(access);
        self.canvases.link.busy = self.canvases.link_state.busy();
        self.canvases.link.error = None;
    }
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn poll_canvas_linking(&mut self) {
        native::poll(self);
    }
}
#[cfg(not(target_arch = "wasm32"))]
impl From<photocraft_local_store::canvas::linking::LocalBinding> for Binding {
    fn from(value: photocraft_local_store::canvas::linking::LocalBinding) -> Self {
        Self {
            local_owner: value.local_owner,
            service: value.target.service,
            profile: value.target.profile,
            device: value.target.device,
            revision: value.revision,
            active: value.active,
        }
    }
}
#[cfg(not(target_arch = "wasm32"))]
fn error(message: impl std::fmt::Display) -> EngineError {
    EngineError::Other(message.to_string())
}
