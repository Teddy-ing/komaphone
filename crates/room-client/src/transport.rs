use crate::{Error, Result};
use photocraft_party::contracts::MAX_MESSAGE_BYTES;
use photocraft_party::protocol::{Fault, MAX_RESPONSE_BYTES, Operation, RpcRequest, RpcResponse};
use std::io::Read;
use std::time::Duration;

/// HTTP is permitted only for numeric loopback addresses. Never resolve a hostname and
/// subsequently assume that all future resolutions are loopback.
pub fn normalize_endpoint(input: &str) -> Result<String> {
    if input.len() > 2048 {
        return Err(Error::Invalid("Service address is too long".into()));
    }
    let url = reqwest::Url::parse(input).map_err(|_| Error::Invalid("Use an HTTPS service address or http://127.0.0.1:port for local testing".into()))?;
    let loopback = url.host_str().and_then(|host| host.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok()).is_some_and(|ip| ip.is_loopback());
    if !matches!(url.scheme(), "https" | "http")
        || (url.scheme() == "http" && !loopback)
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(Error::Invalid("Service address must be an HTTPS origin without credentials, path, query or fragment; HTTP is loopback-only".into()));
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

pub struct Transport {
    client: reqwest::blocking::Client,
    auth: Vec<(std::sync::Arc<crate::worker::fence::AuthFence>, u64, bool)>,
}
impl Transport {
    pub(crate) fn with_auth_fence(&self, fence: std::sync::Arc<crate::worker::fence::AuthFence>, ticket: u64) -> Self {
        let mut auth = self.auth.clone();
        auth.push((fence, ticket, false));
        Self { client: self.client.clone(), auth }
    }
    pub(crate) fn with_auth_fence_current(&self, fence: std::sync::Arc<crate::worker::fence::AuthFence>, ticket: u64) -> Self {
        let mut auth = self.auth.clone();
        auth.push((fence, ticket, true));
        Self { client: self.client.clone(), auth }
    }
    fn check_auth(&self) -> Result<()> {
        for (fence, ticket, allow_paused) in &self.auth {
            if *allow_paused && fence.current(*ticket) {
                continue;
            }
            fence.check(*ticket)?;
        }
        Ok(())
    }
    pub fn call_world(
        &self,
        endpoint: &str,
        credential: &str,
        request: &photocraft_scene::world_protocol::Request,
    ) -> Result<photocraft_scene::world_protocol::Response> {
        use photocraft_scene::world_protocol as world;
        let endpoint = normalize_endpoint(endpoint)?;
        photocraft_party::protocol::validate_secret(credential).map_err(|_| Error::Credentials("Stored credential is invalid".into()))?;
        let body = serde_json::to_vec(request).map_err(|_| Error::Invalid("Cannot encode canvas request".into()))?;
        if body.len() > world::MAX_REQUEST_BYTES {
            return Err(Error::Invalid("Canvas request exceeds its byte limit".into()));
        }
        self.check_auth()?;
        let response = self
            .client
            .post(format!("{endpoint}/v1/canvas"))
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .bearer_auth(credential)
            .body(body)
            .send()
            .map_err(|_| Error::Unavailable)?;
        let status = response.status();
        if response.content_length().is_some_and(|bytes| bytes > world::MAX_RESPONSE_BYTES as u64) {
            return Err(Error::Invalid("Canvas response exceeds its byte limit".into()));
        }
        let mut bytes = Vec::new();
        response.take(world::MAX_RESPONSE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| Error::Unavailable)?;
        self.check_auth()?;
        if bytes.len() > world::MAX_RESPONSE_BYTES {
            return Err(Error::Invalid("Canvas response exceeds its byte limit".into()));
        }
        if !status.is_success() {
            let fault = serde_json::from_slice::<Fault>(&bytes)
                .map_err(|_| Error::Rejected { code: "protocol".into(), message: "Service returned an incompatible canvas response".into() })?;
            let (code, message) = public_fault(&fault.code);
            return Err(Error::Rejected { code: code.into(), message: message.into() });
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("Service returned an invalid canvas response".into()))
    }

    pub fn new(trusted_certificate_pem: Option<&[u8]>) -> Result<Self> {
        let mut builder = reqwest::blocking::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(2))
            .timeout(Duration::from_secs(5));
        if let Some(pem) = trusted_certificate_pem {
            if pem.len() > 64 * 1024 {
                return Err(Error::Invalid("Trusted certificate exceeds 64 KiB".into()));
            }
            let certificates = reqwest::Certificate::from_pem_bundle(pem).map_err(|_| Error::Invalid("Invalid trusted service certificate".into()))?;
            if certificates.is_empty() || certificates.len() > 8 {
                return Err(Error::Invalid("Trusted certificate file must contain 1–8 certificates".into()));
            }
            for certificate in certificates {
                builder = builder.add_root_certificate(certificate);
            }
        }
        Ok(Self { client: builder.build().map_err(|_| Error::Unavailable)?, auth: Vec::new() })
    }

    pub fn call(&self, endpoint: &str, credential: &str, request: &RpcRequest) -> Result<RpcResponse> {
        let endpoint = normalize_endpoint(endpoint)?;
        let body = serde_json::to_vec(request).map_err(|_| Error::Invalid("Cannot encode room request".into()))?;
        if body.len() > MAX_MESSAGE_BYTES {
            return Err(Error::Invalid("Room request exceeds 256 KiB".into()));
        }
        let mut call = self.client.post(format!("{endpoint}/v1/rpc")).header(reqwest::header::CONTENT_TYPE, "application/json").body(body);
        if !matches!(request.operation, Operation::Pair { .. }) {
            photocraft_party::protocol::validate_secret(credential).map_err(|_| Error::Credentials("Stored credential is invalid".into()))?;
            call = call.bearer_auth(credential);
        }
        self.check_auth()?;
        let response = call.send().map_err(|_| Error::Unavailable)?;
        let status = response.status();
        if response.content_length().is_some_and(|bytes| bytes > MAX_RESPONSE_BYTES as u64) {
            return Err(Error::Invalid("Service response exceeds its byte limit".into()));
        }
        let mut bytes = Vec::new();
        response.take(MAX_RESPONSE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| Error::Unavailable)?;
        self.check_auth()?;
        if bytes.len() > MAX_RESPONSE_BYTES {
            return Err(Error::Invalid("Service response exceeds its byte limit".into()));
        }
        if !status.is_success() {
            if let Ok(fault) = serde_json::from_slice::<Fault>(&bytes) {
                // Only show locally defined errors. An untrusted endpoint must not reflect
                // a bearer or invitation from the request into logs/UI/control snapshots.
                let (code, message) = public_fault(&fault.code);
                return Err(Error::Rejected { code: code.into(), message: message.into() });
            }
            return Err(Error::Rejected {
                code: "protocol".into(),
                message: "Service returned an invalid response; check the address and protocol version".into(),
            });
        }
        serde_json::from_slice(&bytes).map_err(|_| Error::Invalid("Service returned an invalid or incompatible response".into()))
    }
}

fn public_fault(code: &str) -> (&'static str, &'static str) {
    match code {
        "presence_stream" => ("presence_stream", "The transient presence stream changed; reconnect the preview stream"),
        "session" => ("session", "Application sign-in has expired or been revoked; reconnect to continue syncing saved artwork"),
        "unauthorized" => ("unauthorized", "Pairing or room membership does not permit this action"),
        "conflict" => ("conflict", "The room changed or this submission conflicts; retained local work is unchanged"),
        "phase" => ("phase", "This action is not available in the current room phase"),
        "version" => ("version", "The service uses an incompatible protocol; upgrade before reconnecting"),
        "limit" | "quota" => ("limit", "The service resource limit was reached; pending work remains local"),
        "storage" => ("storage", "The service could not commit the change; pending work remains local"),
        "busy" | "rate_limit" => ("busy", "The service is busy; retry shortly"),
        _ => ("invalid", "The service rejected this request; retain the draft and check room state"),
    }
}

#[cfg(test)]
mod presence_fault_tests {
    #[test]
    fn transient_stream_fault_keeps_its_distinct_redacted_code() {
        let (code, message) = super::public_fault("presence_stream");
        assert_eq!(code, "presence_stream");
        assert!(message.contains("preview stream"));
        assert_ne!(super::public_fault("unauthorized").0, code);
        assert_ne!(super::public_fault("session").0, code);
    }
}

#[cfg(test)]
mod account_fence_tests {
    use super::*;
    use crate::worker::fence::AuthFence;
    use std::io::Write;
    use std::sync::{Arc, mpsc};
    #[test]
    fn account_fence_prevents_http_and_rejects_inflight_reply() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let fence = Arc::new(AuthFence::default());
        let transport = Transport::new(None).unwrap().with_auth_fence(Arc::clone(&fence), 0);
        let request = photocraft_scene::world_protocol::Request {
            version: photocraft_scene::world_protocol::VERSION,
            action: photocraft_scene::world_protocol::Action::Identity {},
        };
        let (sent, ready) = mpsc::channel();
        let (release, wait) = mpsc::channel();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut bytes = [0; 4096];
            let _ = stream.read(&mut bytes).unwrap();
            sent.send(()).unwrap();
            wait.recv_timeout(Duration::from_secs(3)).unwrap();
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").unwrap();
        });
        let control = Arc::clone(&fence);
        let pause = std::thread::spawn(move || {
            ready.recv_timeout(Duration::from_secs(3)).unwrap();
            control.pause();
            release.send(()).unwrap();
        });
        assert!(matches!(transport.call_world(&endpoint,&"a".repeat(64),&request),Err(Error::Rejected {code,..}) if code=="account_changed"));
        pause.join().unwrap();
        server.join().unwrap();
        // The listener has closed: a second call must fail at the fence rather
        // than contacting it and returning a network-unavailable error.
        assert!(matches!(transport.call_world(&endpoint,&"a".repeat(64),&request),Err(Error::Rejected {code,..}) if code=="account_changed"));
    }
}
