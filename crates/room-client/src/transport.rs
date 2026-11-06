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
}
impl Transport {
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
            let cert = reqwest::Certificate::from_pem(pem).map_err(|_| Error::Invalid("Invalid trusted service certificate".into()))?;
            builder = builder.add_root_certificate(cert);
        }
        Ok(Self { client: builder.build().map_err(|_| Error::Unavailable)? })
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
        let response = call.send().map_err(|_| Error::Unavailable)?;
        let status = response.status();
        if response.content_length().is_some_and(|bytes| bytes > MAX_RESPONSE_BYTES as u64) {
            return Err(Error::Invalid("Service response exceeds its byte limit".into()));
        }
        let mut bytes = Vec::new();
        response.take(MAX_RESPONSE_BYTES as u64 + 1).read_to_end(&mut bytes).map_err(|_| Error::Unavailable)?;
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
