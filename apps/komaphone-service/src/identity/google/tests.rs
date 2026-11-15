use super::*;
use jsonwebtoken::{EncodingKey, Header, encode};
use serde_json::{Value, json};

const CLIENT: &str = "test.apps.googleusercontent.com";
fn transaction() -> RequestId {
    RequestId::new("00000000000000000000000000000001").unwrap()
}
fn key() -> DecodingKey {
    DecodingKey::from_rsa_der(include_bytes!("../../../tests/fixtures/tls/localhost-public.der"))
}
fn claims() -> Value {
    json!({"iss":"https://accounts.google.com","sub":"google-stable-subject","aud":CLIENT,"iat":1000,"exp":4600,"nonce":"1".repeat(64)})
}
fn token(claims: &Value) -> String {
    let mut header = Header::new(Algorithm::RS256);
    header.kid = Some("test-key".into());
    encode(&header, claims, &EncodingKey::from_rsa_der(include_bytes!("../../../tests/fixtures/tls/localhost-private.der"))).unwrap()
}

#[test]
fn signed_google_shape_requires_correct_claims_and_real_signature() {
    let original = claims();
    let valid = token(&original);
    let verified = verify_with_key(CLIENT, transaction(), &valid, &key(), 1_001_000).unwrap();
    assert_eq!(verified.subject, "google-stable-subject");
    assert_eq!(verified.nonce_hash, crate::store::digest("1".repeat(64).as_bytes()));
    for (field, value) in [
        ("aud", json!("attacker.apps.googleusercontent.com")),
        ("iss", json!("https://attacker.invalid")),
        ("exp", json!(1001)),
        ("iat", json!(1062)),
        ("exp", json!(10000)),
        ("nonce", json!("bad")),
        ("sub", json!("")),
        ("azp", json!("other-client")),
        ("aud", json!([CLIENT, "other-client"])),
        ("nbf", json!(1002)),
    ] {
        let mut wrong = original.clone();
        wrong[field] = value;
        assert!(verify_with_key(CLIENT, transaction(), &token(&wrong), &key(), 1_001_000).is_err(), "{field}");
    }
    for field in ["exp", "iat", "sub", "nonce", "aud", "iss"] {
        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(verify_with_key(CLIENT, transaction(), &token(&missing), &key(), 1_001_000).is_err(), "missing {field}");
    }
    let mut changed = valid.into_bytes();
    let signature = changed.iter().rposition(|b| *b == b'.').unwrap() + 1;
    changed[signature] = if changed[signature] == b'A' { b'B' } else { b'A' };
    assert!(verify_with_key(CLIENT, transaction(), std::str::from_utf8(&changed).unwrap(), &key(), 1_001_000).is_err());
    let mut alternate = original;
    alternate["iss"] = json!("accounts.google.com");
    assert_eq!(verify_with_key(CLIENT, transaction(), &token(&alternate), &key(), 1_001_000).unwrap().issuer, super::super::GOOGLE_ISSUER);
}

#[tokio::test]
async fn real_http_hosted_login_admission_party_session_and_revocation() {
    use crate::{Store, StoreOptions, server};
    use photocraft_party::{
        DeviceId, Mode, Participant, ParticipantId,
        protocol::{Operation, RpcRequest},
    };
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("service.sqlite3");
    let now = u64::try_from(chrono::Utc::now().timestamp_millis()).unwrap();
    let mut store = Store::open(&path, StoreOptions { hosted: true, ..Default::default() }, now).unwrap();
    let admission = store.identity_create_admission(60_000, now).unwrap();
    drop(store);
    let google = GoogleVerifier::new(CLIENT).unwrap();
    {
        let mut cache = google.cache.lock().await;
        cache.keys.insert("test-key".into(), key());
        cache.expires = Instant::now() + Duration::from_secs(600);
    }
    let service = server::spawn_hosted("127.0.0.1:0".parse().unwrap(), &path, StoreOptions::default(), google).await.unwrap();
    let endpoint = format!("http://{}", service.address);
    let client = reqwest::Client::new();
    let config: Value = client.get(format!("{endpoint}/v1/auth/config")).send().await.unwrap().json().await.unwrap();
    assert_eq!(config["client_id"], CLIENT);
    assert_eq!(config["hosted"], true);
    let device = DeviceId::new("00000000000000000000000000000001").unwrap();
    let proof = "4".repeat(64);
    let pair = RpcRequest {
        protocol_version: 1,
        request_id: transaction(),
        operation: Operation::Pair {
            participant: Participant { id: ParticipantId::new(device.as_str()).unwrap(), device, display_name: "Uninvited".into() },
            credential: proof.clone(),
        },
    };
    assert_eq!(client.post(format!("{endpoint}/v1/rpc")).json(&pair).send().await.unwrap().status(), reqwest::StatusCode::FORBIDDEN);
    let challenge: Value =
        client.post(format!("{endpoint}/v1/auth")).json(&json!({"action":"begin","device":device})).send().await.unwrap().json().await.unwrap();
    let mut claims = claims();
    claims["iat"] = json!(now / 1000);
    claims["exp"] = json!(now / 1000 + 3600);
    claims["nonce"] = challenge["nonce"].clone();
    let mut finish = json!({"action":"finish","transaction":challenge["transaction"],"id_token":token(&claims),"device_proof":proof,"label":"Alice"});
    assert_eq!(client.post(format!("{endpoint}/v1/auth")).json(&finish).send().await.unwrap().status(), reqwest::StatusCode::FORBIDDEN);
    finish["admission"] = json!(admission);
    let response = client.post(format!("{endpoint}/v1/auth")).json(&finish).send().await.unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::OK);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let session: Value = response.json().await.unwrap();
    let session_token = session["token"].as_str().unwrap();
    assert_eq!(session["device_credential"], proof);
    assert_eq!(client.post(format!("{endpoint}/v1/auth")).json(&finish).send().await.unwrap().status(), reqwest::StatusCode::FORBIDDEN);
    let create =
        RpcRequest { protocol_version: 1, request_id: transaction(), operation: Operation::Create { mode: Mode::Animation { rotations: 1 }, timer_ms: None } };
    let created = client.post(format!("{endpoint}/v1/rpc")).bearer_auth(session_token).json(&create).send().await.unwrap();
    assert_eq!(created.status(), reqwest::StatusCode::OK);
    let denied = client.post(format!("{endpoint}/v1/rpc")).bearer_auth(&proof).json(&create).send().await.unwrap();
    assert_eq!(denied.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(denied.json::<Value>().await.unwrap()["code"], "session");
    assert_eq!(
        client.post(format!("{endpoint}/v1/auth")).bearer_auth(session_token).json(&json!({"action":"revoke"})).send().await.unwrap().status(),
        reqwest::StatusCode::OK
    );
    let revoked = client.post(format!("{endpoint}/v1/rpc")).bearer_auth(session_token).json(&create).send().await.unwrap();
    assert_eq!(revoked.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(revoked.json::<Value>().await.unwrap()["code"], "session");
    service.shutdown().await.unwrap();
    // Simulate known expired session after restart; it has the same reconnect code.
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE identity_sessions SET revoked=0,expires=1", []).unwrap();
    drop(db);
    let service = server::spawn_hosted("127.0.0.1:0".parse().unwrap(), &path, StoreOptions::default(), GoogleVerifier::new(CLIENT).unwrap()).await.unwrap();
    let expired = client.post(format!("http://{}/v1/rpc", service.address)).bearer_auth(session_token).json(&create).send().await.unwrap();
    assert_eq!(expired.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_eq!(expired.json::<Value>().await.unwrap()["code"], "session");
    service.shutdown().await.unwrap();
}

#[test]
fn algorithm_confusion_malformed_tokens_and_untrusted_key_inputs_fail_closed() {
    let mut header = Header::new(Algorithm::HS256);
    header.kid = Some("test-key".into());
    let hs = encode(&header, &claims(), &EncodingKey::from_secret(b"arbitrary-public-key")).unwrap();
    for bad in [hs, "".into(), "not.a.token".into(), "a".repeat(MAX_TOKEN_BYTES + 1)] {
        assert!(verify_with_key(CLIENT, transaction(), &bad, &key(), 1_001_000).is_err());
    }
    for bytes in [b"{}".as_slice(), b"{\"keys\":[]}".as_slice(), b"not-json".as_slice()] {
        assert!(parse_keys(bytes).is_err());
    }
    assert!(parse_keys(&vec![b' '; MAX_KEYS_BYTES + 1]).is_err());
    assert_eq!(cache_seconds("public, max-age=60, must-revalidate"), Some(60));
    for id in ["", "https://evil.apps.googleusercontent.com", "test.apps.googleusercontent.com/evil"] {
        assert!(GoogleVerifier::new(id).is_err());
    }
}
