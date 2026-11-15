//! Google ID-token verifier. No caller-provided key URLs or discovery documents.
//! See https://developers.google.com/identity/sign-in/web/backend-auth.
use super::{VerifiedLogin, denied};
use crate::{Error, Result};
use jsonwebtoken::{Algorithm, DecodingKey, Validation, decode, decode_header};
use photocraft_party::RequestId;
use serde::Deserialize;
use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::sync::Mutex;

const KEYS_URL: &str = "https://www.googleapis.com/oauth2/v3/certs";
const MAX_KEYS_BYTES: usize = 64 * 1024;
pub const MAX_TOKEN_BYTES: usize = 16 * 1024;

#[derive(Clone)]
pub struct GoogleVerifier {
    client_id: String,
    client: reqwest::Client,
    cache: Arc<Mutex<KeyCache>>,
}
struct KeyCache {
    keys: BTreeMap<String, DecodingKey>,
    expires: Instant,
    last_attempt: Option<Instant>,
}
#[derive(Deserialize)]
struct Keys {
    keys: Vec<Key>,
}
#[derive(Deserialize)]
struct Key {
    kid: String,
    kty: String,
    alg: String,
    #[serde(rename = "use")]
    usage: String,
    n: String,
    e: String,
}
#[derive(Clone, Deserialize)]
struct Claims {
    iss: String,
    sub: String,
    aud: String,
    exp: u64,
    iat: u64,
    nonce: String,
    #[serde(default)]
    azp: Option<String>,
    #[serde(default)]
    nbf: Option<u64>,
}

impl GoogleVerifier {
    pub fn new(client_id: &str) -> Result<Self> {
        if client_id.len() > 256
            || !client_id.ends_with(".apps.googleusercontent.com")
            || !client_id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-'))
        {
            return Err(Error::Invalid("Google desktop client ID"));
        }
        let client = reqwest::Client::builder()
            .https_only(true)
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(5))
            .timeout(Duration::from_secs(10))
            .user_agent("Komaphone-service")
            .build()
            .map_err(|_| Error::Invalid("Google verifier TLS client"))?;
        Ok(Self {
            client_id: client_id.into(),
            client,
            cache: Arc::new(Mutex::new(KeyCache { keys: BTreeMap::new(), expires: Instant::now(), last_attempt: None })),
        })
    }
    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    pub(crate) async fn verify(&self, transaction: RequestId, token: &str, now_ms: u64) -> Result<VerifiedLogin> {
        let kid = token_key_id(token)?;
        let mut cache = self.cache.lock().await;
        let now = Instant::now();
        if now >= cache.expires || !cache.keys.contains_key(&kid) {
            // A bad kid cannot trigger unbounded Google traffic. Stale keys fail closed.
            if cache.last_attempt.is_some_and(|last| now.saturating_duration_since(last) < Duration::from_secs(60)) {
                return Err(denied());
            }
            cache.last_attempt = Some(now);
            let mut response = self.client.get(KEYS_URL).send().await.map_err(|_| Error::IdentityProvider)?;
            if !response.status().is_success() || response.content_length().is_some_and(|n| n > MAX_KEYS_BYTES as u64) {
                return Err(Error::IdentityProvider);
            }
            let max_age =
                response.headers().get(reqwest::header::CACHE_CONTROL).and_then(|v| v.to_str().ok()).and_then(cache_seconds).unwrap_or(300).min(6 * 60 * 60);
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|_| Error::IdentityProvider)? {
                if bytes.len().saturating_add(chunk.len()) > MAX_KEYS_BYTES {
                    return Err(Error::IdentityProvider);
                }
                bytes.extend_from_slice(&chunk);
            }
            cache.keys = parse_keys(&bytes)?;
            cache.expires = now + Duration::from_secs(max_age);
        }
        let key = cache.keys.get(&kid).ok_or_else(denied)?;
        verify_with_key(&self.client_id, transaction, token, key, now_ms)
    }
}
fn cache_seconds(value: &str) -> Option<u64> {
    value.split(',').find_map(|part| part.trim().strip_prefix("max-age=").and_then(|n| n.parse().ok()))
}
fn token_key_id(token: &str) -> Result<String> {
    if token.is_empty() || token.len() > MAX_TOKEN_BYTES {
        return Err(denied());
    }
    let header = decode_header(token).map_err(|_| denied())?;
    if header.alg != Algorithm::RS256 {
        return Err(denied());
    }
    let kid = header.kid.ok_or_else(denied)?;
    if kid.is_empty() || kid.len() > 128 || !kid.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')) {
        return Err(denied());
    }
    Ok(kid)
}
fn parse_keys(bytes: &[u8]) -> Result<BTreeMap<String, DecodingKey>> {
    if bytes.len() > MAX_KEYS_BYTES {
        return Err(Error::IdentityProvider);
    }
    let keys: Keys = serde_json::from_slice(bytes).map_err(|_| Error::IdentityProvider)?;
    if keys.keys.is_empty() || keys.keys.len() > 16 {
        return Err(Error::IdentityProvider);
    }
    let mut result = BTreeMap::new();
    for key in keys.keys {
        if key.kty != "RSA"
            || key.alg != "RS256"
            || key.usage != "sig"
            || key.kid.is_empty()
            || key.kid.len() > 128
            || !(256..=1024).contains(&key.n.len())
            || key.e.len() > 16
            || result.contains_key(&key.kid)
        {
            return Err(Error::IdentityProvider);
        }
        let decoded = DecodingKey::from_rsa_components(&key.n, &key.e).map_err(|_| Error::IdentityProvider)?;
        result.insert(key.kid, decoded);
    }
    Ok(result)
}
fn verify_with_key(client_id: &str, transaction: RequestId, token: &str, key: &DecodingKey, now_ms: u64) -> Result<VerifiedLogin> {
    token_key_id(token)?;
    let mut validation = Validation::new(Algorithm::RS256);
    validation.set_audience(&[client_id]);
    validation.set_issuer(&["https://accounts.google.com", "accounts.google.com"]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub", "iat", "nonce"]);
    // Use the authority's monotonic UTC clock, including deterministic tests.
    validation.validate_exp = false;
    validation.validate_nbf = false;
    let claims = decode::<Claims>(token, key, &validation).map_err(|_| denied())?.claims;
    let now = now_ms / 1000;
    if claims.aud != client_id
        || !matches!(claims.iss.as_str(), "accounts.google.com" | "https://accounts.google.com")
        || claims.exp <= now
        || claims.iat > now.saturating_add(60)
        || claims.exp <= claims.iat
        || claims.exp.saturating_sub(claims.iat) > 2 * 60 * 60
        || claims.azp.as_ref().is_some_and(|azp| azp != client_id)
        || claims.nbf.is_some_and(|nbf| nbf > now)
        || claims.sub.is_empty()
        || claims.sub.len() > 255
        || !claims.sub.bytes().all(|b| b.is_ascii_graphic())
        || photocraft_party::protocol::validate_secret(&claims.nonce).is_err()
    {
        return Err(denied());
    }
    Ok(VerifiedLogin {
        transaction,
        nonce_hash: crate::store::digest(claims.nonce.as_bytes()),
        issuer: super::GOOGLE_ISSUER.into(),
        subject: claims.sub,
        expires_at_ms: claims.exp.checked_mul(1000).ok_or_else(denied)?,
    })
}

#[cfg(test)]
mod tests;
