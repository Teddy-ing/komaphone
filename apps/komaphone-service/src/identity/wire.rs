//! Bounded hosted-login messages. Secret-bearing values intentionally lack Debug.
use super::{IssuedSession, VerifiedLogin};
use photocraft_party::{DeviceId, ParticipantId, RequestId};
use serde::{Deserialize, Serialize};

pub const MAX_REQUEST_BYTES: usize = 20 * 1024;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Begin {
        device: DeviceId,
    },
    Finish {
        transaction: RequestId,
        id_token: String,
        device_proof: String,
        #[serde(default)]
        admission: Option<String>,
        label: String,
    },
    Cancel {
        transaction: RequestId,
        nonce: String,
    },
    Revoke,
}
pub(crate) enum VerifiedRequest {
    Begin { device: DeviceId },
    Finish { verified: VerifiedLogin, device_proof: String, admission: Option<String>, label: String },
    Cancel { transaction: RequestId, nonce: String },
    Revoke,
}
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Challenge { transaction: RequestId, nonce: String, expires_at_ms: u64, client_id: String },
    Session { profile: ParticipantId, device: DeviceId, label: String, token: String, device_credential: String, expires_at_ms: u64 },
    Done,
}
impl Response {
    pub(crate) fn session(value: IssuedSession, label: String) -> Self {
        Self::Session {
            profile: value.context.profile,
            device: value.context.device,
            label,
            token: value.token,
            device_credential: value.device_credential,
            expires_at_ms: value.expires_at_ms,
        }
    }
}
impl crate::Store {
    pub(crate) fn identity_execute(&mut self, request: VerifiedRequest, credential: Option<&str>, client_id: &str, now: u64) -> crate::Result<Response> {
        match request {
            VerifiedRequest::Begin { device } => {
                let login = self.identity_begin_login(device, now)?;
                Ok(Response::Challenge { transaction: login.id, nonce: login.nonce, expires_at_ms: login.expires_at_ms, client_id: client_id.into() })
            }
            VerifiedRequest::Finish { verified, device_proof, admission, label } => {
                let session = self.identity_finish_admitted_login(verified, Some(&device_proof), admission.as_deref(), &label, now)?;
                let label = self.identity_participant(session.context)?.display_name;
                Ok(Response::session(session, label))
            }
            VerifiedRequest::Cancel { transaction, nonce } => {
                self.identity_cancel_login(transaction, &nonce)?;
                Ok(Response::Done)
            }
            VerifiedRequest::Revoke => {
                self.identity_revoke_session(credential.ok_or_else(super::denied)?, now)?;
                Ok(Response::Done)
            }
        }
    }
}
