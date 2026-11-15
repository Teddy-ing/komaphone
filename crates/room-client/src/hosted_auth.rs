//! Native system-browser Google sign-in. Provider tokens never enter ordinary state.
//!
//! The service owns admission, Google signature verification and profile linking.
//! This adapter owns the PKCE secret and a single bounded loopback callback. It
//! deliberately does not persist credentials or launch a shell: the caller supplies
//! the system-browser effect and saves the returned session in Credential Manager.

use crate::{Error, Result, normalize_endpoint};
use photocraft_party::{DeviceId, Participant, ParticipantId, RequestId};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

mod callback;
mod installed_app;
#[cfg(test)]
mod tests;

pub const SERVICE_ENDPOINT: &str = "https://komaphone.tedsoftware.org";
pub const GOOGLE_CLIENT_ID: &str = "981926394354-07f4s3dss4vgp7mteh0qe8e6dhk3s99o.apps.googleusercontent.com";
const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
const MAX_AUTH_BYTES: usize = 32 * 1024;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(5 * 60);

/// This public configuration contains no account, nonce or credentials.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Configuration {
    pub protocol_version: u16,
    pub client_id: String,
    pub hosted: bool,
}

/// Secret-bearing inputs intentionally have no Debug or Serialize implementation.
pub struct LoginOptions {
    pub endpoint: String,
    pub device: DeviceId,
    /// Persist this proof before starting login, including for a new device.
    pub device_proof: String,
    pub admission: Option<String>,
    pub label: String,
}

/// In-process result only. Store the two secrets separately through CredentialStore.
pub struct Session {
    pub profile: ParticipantId,
    pub device: DeviceId,
    pub label: String,
    pub token: String,
    pub device_credential: String,
    pub expires_at_ms: u64,
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum Request<'a> {
    Begin { device: DeviceId },
    Finish { transaction: RequestId, id_token: &'a str, device_proof: &'a str, admission: Option<&'a str>, label: &'a str },
    Cancel { transaction: RequestId, nonce: &'a str },
    Revoke,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Response {
    Challenge { transaction: RequestId, nonce: String, expires_at_ms: u64, client_id: String },
    Session { profile: ParticipantId, device: DeviceId, label: String, token: String, device_credential: String, expires_at_ms: u64 },
    Done,
}

/// Blocking network operations belong on the room worker, never the UI thread.
pub struct Client {
    service: reqwest::blocking::Client,
    google: reqwest::blocking::Client,
    client_secret: Option<String>,
}

impl Client {
    pub fn new(trusted_certificate_pem: Option<&[u8]>) -> Result<Self> {
        Self::http_client(trusted_certificate_pem)
    }
    fn http_client(trusted_certificate_pem: Option<&[u8]>) -> Result<Self> {
        let mut service = http_builder();
        if let Some(pem) = trusted_certificate_pem {
            if pem.len() > 64 * 1024 {
                return Err(invalid("Trusted service certificate exceeds its limit"));
            }
            let certificates = reqwest::Certificate::from_pem_bundle(pem).map_err(|_| invalid("Invalid trusted service certificate"))?;
            if certificates.is_empty() || certificates.len() > 8 {
                return Err(invalid("Trusted service certificate must contain 1–8 certificates"));
            }
            for certificate in certificates {
                service = service.add_root_certificate(certificate);
            }
        }
        // A development service CA must never expand trust for Google's token server.
        Ok(Self {
            service: service.build().map_err(|_| Error::Unavailable)?,
            google: http_builder().build().map_err(|_| Error::Unavailable)?,
            client_secret: None,
        })
    }

    /// Load Google's downloaded Desktop client configuration. It is not a user
    /// credential or an authorization boundary, but must never enter logs/state.
    /// Downloaded endpoint fields are ignored: provider URLs remain pinned here.
    pub fn with_installed_client_json(trusted_certificate_pem: Option<&[u8]>, bytes: &[u8]) -> Result<Self> {
        let secret = installed_client_secret(bytes)?;
        let mut client = Self::http_client(trusted_certificate_pem)?;
        client.client_secret = Some(secret);
        Ok(client)
    }

    pub fn configuration(&self, endpoint: &str) -> Result<Configuration> {
        let endpoint = normalize_endpoint(endpoint)?;
        let response = self.service.get(format!("{endpoint}/v1/auth/config")).send().map_err(|_| Error::Unavailable)?;
        let bytes = response_bytes(response)?;
        let configuration: Configuration = serde_json::from_slice(&bytes).map_err(|_| invalid("Service sign-in configuration is incompatible"))?;
        if configuration.protocol_version != 1 || !configuration.hosted || configuration.client_id != GOOGLE_CLIENT_ID {
            return Err(invalid("This service is not configured for this Komaphone Google sign-in client"));
        }
        Ok(configuration)
    }

    /// Open exactly the provided URL in the system browser. Do not log, serialize,
    /// copy to ordinary status, or reconstruct it with a shell command.
    pub fn sign_in(&self, options: LoginOptions, cancel: &AtomicBool, mut open_browser: impl FnMut(&str) -> Result<()>) -> Result<Session> {
        check_cancel(cancel)?;
        if self.client_secret.is_none() {
            return Err(invalid("This build needs its Google Desktop client configuration before sign-in; local drawing remains available"));
        }
        let endpoint = normalize_endpoint(&options.endpoint)?;
        validate_secret(&options.device_proof)?;
        if let Some(admission) = &options.admission {
            validate_secret(admission)?;
        }
        photocraft_party::contracts::validate_label(&options.label, photocraft_party::contracts::MAX_NAME_BYTES, false)
            .map_err(|_| invalid("Choose a display name of at most 96 bytes"))?;
        let Response::Challenge { transaction, nonce, expires_at_ms, client_id } = self.request(&endpoint, None, &Request::Begin { device: options.device })?
        else {
            return Err(invalid("Service returned an incompatible sign-in challenge"));
        };
        if client_id != GOOGLE_CLIENT_ID
            || nonce.len() < 32
            || nonce.len() > 256
            || !nonce.bytes().all(|value| value.is_ascii_alphanumeric() || b"-_".contains(&value))
        {
            return Err(invalid("Service returned an invalid sign-in challenge"));
        }
        let remaining_ms = expires_at_ms.saturating_sub(now_ms()?);
        if remaining_ms == 0 || remaining_ms > 10 * 60 * 1000 {
            return Err(invalid("Sign-in challenge expired; try again"));
        }
        let result = (|| {
            check_cancel(cancel)?;
            let callback = callback::Callback::bind()?;
            let proof = callback::Proof::new()?;
            let url = proof.authorization_url(&client_id, &nonce, &callback.redirect_uri())?;
            open_browser(url.as_str())?;
            let timeout = LOGIN_TIMEOUT.min(Duration::from_millis(remaining_ms));
            let code = callback.receive(&proof.state, cancel, Instant::now() + timeout)?;
            check_cancel(cancel)?;
            let id_token = self.exchange(&client_id, &code, &proof.verifier, &callback.redirect_uri())?;
            check_cancel(cancel)?;
            let reply = self.request(
                &endpoint,
                None,
                &Request::Finish {
                    transaction,
                    id_token: &id_token,
                    device_proof: &options.device_proof,
                    admission: options.admission.as_deref(),
                    label: &options.label,
                },
            )?;
            let session = validate_session(reply, options.device, &options.device_proof)?;
            if check_cancel(cancel).is_err() {
                let _ = self.revoke(&endpoint, &session.token);
                return Err(canceled());
            }
            Ok(session)
        })();
        if result.is_err() {
            let _ = self.request(&endpoint, None, &Request::Cancel { transaction, nonce: &nonce });
        }
        result
    }

    pub fn revoke(&self, endpoint: &str, token: &str) -> Result<()> {
        validate_secret(token)?;
        match self.request(&normalize_endpoint(endpoint)?, Some(token), &Request::Revoke)? {
            Response::Done => Ok(()),
            _ => Err(invalid("Service returned an incompatible sign-out response")),
        }
    }

    fn request(&self, endpoint: &str, bearer: Option<&str>, request: &Request<'_>) -> Result<Response> {
        let mut call = self.service.post(format!("{endpoint}/v1/auth")).json(request);
        if let Some(bearer) = bearer {
            call = call.bearer_auth(bearer);
        }
        let bytes = response_bytes(call.send().map_err(|_| Error::Unavailable)?)?;
        serde_json::from_slice(&bytes).map_err(|_| invalid("Service returned an incompatible sign-in response"))
    }

    fn exchange(&self, client_id: &str, code: &str, verifier: &str, redirect_uri: &str) -> Result<String> {
        let form = token_form(client_id, code, verifier, redirect_uri, self.client_secret.as_deref())?;
        let response = self
            .google
            .post(TOKEN_ENDPOINT)
            .header(reqwest::header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(form)
            .send()
            .map_err(|_| Error::Unavailable)?;
        let bytes = provider_bytes(response)?;
        #[derive(Deserialize)]
        struct Tokens {
            id_token: String,
        }
        // Access/refresh tokens, if returned by Google, are discarded rather than persisted.
        let tokens: Tokens = serde_json::from_slice(&bytes).map_err(|_| invalid("Google did not return an identity token; try signing in again"))?;
        if tokens.id_token.is_empty() || tokens.id_token.len() > 16 * 1024 || !tokens.id_token.is_ascii() {
            return Err(invalid("Google returned an invalid identity token"));
        }
        Ok(tokens.id_token)
    }
}

fn installed_client_secret(bytes: &[u8]) -> Result<String> {
    if bytes.is_empty() || bytes.len() > MAX_AUTH_BYTES {
        return Err(invalid("Desktop OAuth client configuration must contain 1–32768 bytes"));
    }
    #[derive(Deserialize)]
    struct Download {
        installed: Installed,
    }
    #[derive(Deserialize)]
    struct Installed {
        client_id: String,
        client_secret: String,
    }
    let configuration: Download = serde_json::from_slice(bytes).map_err(|_| invalid("Choose the downloaded Google Desktop client JSON configuration"))?;
    if configuration.installed.client_id != GOOGLE_CLIENT_ID {
        return Err(invalid("Desktop OAuth configuration does not match this Komaphone client"));
    }
    let secret = configuration.installed.client_secret;
    if secret.is_empty() || secret.len() > 256 || !secret.bytes().all(|value| value.is_ascii_graphic()) {
        return Err(invalid("Desktop OAuth client configuration contains an invalid client secret"));
    }
    Ok(secret)
}

fn token_form(client_id: &str, code: &str, verifier: &str, redirect_uri: &str, client_secret: Option<&str>) -> Result<String> {
    let mut fields =
        vec![("client_id", client_id), ("code", code), ("code_verifier", verifier), ("redirect_uri", redirect_uri), ("grant_type", "authorization_code")];
    if let Some(secret) = client_secret {
        fields.push(("client_secret", secret));
    }
    let body = reqwest::Url::parse_with_params(TOKEN_ENDPOINT, &fields).map_err(|_| invalid("Cannot prepare Google sign-in"))?;
    Ok(body.query().unwrap_or_default().to_owned())
}

fn http_builder() -> reqwest::blocking::ClientBuilder {
    reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
}

fn response_bytes(response: reqwest::blocking::Response) -> Result<Vec<u8>> {
    let status = response.status();
    if response.content_length().is_some_and(|length| length > MAX_AUTH_BYTES as u64) {
        return Err(invalid("Sign-in response exceeds its byte limit"));
    }
    let mut bytes = Vec::new();
    response.take(MAX_AUTH_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| Error::Unavailable)?;
    if bytes.len() > MAX_AUTH_BYTES {
        return Err(invalid("Sign-in response exceeds its byte limit"));
    }
    if !status.is_success() {
        // Never reflect provider/service text: it may contain a token or private account data.
        return Err(match status.as_u16() {
            401 => Error::Rejected { code: "session".into(), message: "Sign-in expired or was revoked; sign in again".into() },
            403 => {
                Error::Rejected { code: "unauthorized".into(), message: "Sign-in was not admitted. Check your invitation and selected Google account".into() }
            }
            429 => Error::Rejected { code: "busy".into(), message: "Too many sign-in attempts; wait briefly and try again".into() },
            _ => invalid("Sign-in could not finish; check the selected account and try again"),
        });
    }
    Ok(bytes)
}

fn provider_bytes(response: reqwest::blocking::Response) -> Result<Vec<u8>> {
    if response.status().is_success() {
        return response_bytes(response);
    }
    if response.content_length().is_some_and(|length| length > MAX_AUTH_BYTES as u64) {
        return Err(invalid("Google sign-in response exceeds its byte limit"));
    }
    let mut bytes = Vec::new();
    response.take(MAX_AUTH_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| Error::Unavailable)?;
    if bytes.len() > MAX_AUTH_BYTES {
        return Err(invalid("Google sign-in response exceeds its byte limit"));
    }
    Err(provider_error(&bytes))
}

fn provider_error(bytes: &[u8]) -> Error {
    #[derive(Deserialize)]
    struct ProviderError {
        error: String,
        #[serde(default)]
        error_description: Option<String>,
    }
    let error = serde_json::from_slice::<ProviderError>(bytes).ok();
    // Only fixed public protocol codes are surfaced. Provider descriptions and
    // all unrecognized strings remain excluded from diagnostics and screenshots.
    let (code, message) = match error.as_ref().map(|error| error.error.as_str()) {
        Some("invalid_request")
            if error.as_ref().and_then(|error| error.error_description.as_deref()).is_some_and(|description| {
                matches!(description, "client_secret is missing." | "client_secret is missing" | "Missing required parameter: client_secret")
            }) =>
        {
            (
                "oauth_client_secret",
                "Google requires this Desktop client's downloaded configuration for token exchange; configure the installed OAuth client before retrying sign-in",
            )
        }
        Some("invalid_request") => ("oauth_request", "Google rejected the desktop token request; check the installed OAuth client configuration"),
        Some("invalid_client" | "unauthorized_client") => {
            ("oauth_client", "Google rejected this desktop OAuth client configuration; check the installed-app setup")
        }
        Some("invalid_grant") => ("oauth_grant", "Google sign-in code expired or could not be verified; start sign-in again"),
        Some("access_denied") => ("canceled", "Google sign-in was declined. Local drawing remains available"),
        Some("invalid_scope") => ("oauth_scope", "Google rejected the configured sign-in scopes; check the desktop OAuth setup"),
        _ => ("oauth_provider", "Google sign-in could not finish; check the selected account and try again"),
    };
    Error::Rejected { code: code.into(), message: message.into() }
}

fn validate_session(reply: Response, expected_device: DeviceId, proof: &str) -> Result<Session> {
    let Response::Session { profile, device, label, token, device_credential, expires_at_ms } = reply else {
        return Err(invalid("Service returned an incompatible sign-in result"));
    };
    validate_secret(&token)?;
    validate_secret(&device_credential)?;
    Participant { id: profile, device, display_name: label.clone() }.validate().map_err(|_| invalid("Service returned an invalid profile label"))?;
    if device != expected_device || !callback::constant_time_equal(&device_credential, proof) || expires_at_ms <= now_ms()? {
        return Err(invalid("Service sign-in identity does not match this device; preserve local artwork and retry"));
    }
    Ok(Session { profile, device, label, token, device_credential, expires_at_ms })
}

fn validate_secret(secret: &str) -> Result<()> {
    photocraft_party::protocol::validate_secret(secret).map_err(|_| invalid("Sign-in credential or invitation is invalid"))
}

fn now_ms() -> Result<u64> {
    let now: chrono::DateTime<chrono::Utc> = SystemTime::now().into();
    u64::try_from(now.timestamp_millis()).map_err(|_| invalid("System clock is invalid; correct it before signing in"))
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Acquire) { Err(canceled()) } else { Ok(()) }
}
fn canceled() -> Error {
    Error::Rejected { code: "canceled".into(), message: "Sign-in canceled. Local drawing remains available".into() }
}
fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
