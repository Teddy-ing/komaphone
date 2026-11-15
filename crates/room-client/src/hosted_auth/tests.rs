use super::*;
use std::io::{Read, Write};
use std::net::TcpListener;

#[test]
fn service_errors_never_reflect_credentials_or_account_data() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut bytes = [0u8; 1024];
        let _ = stream.read(&mut bytes).unwrap();
        let body = r#"{"code":"unauthorized","message":"sensitive-provider-token"}"#;
        write!(stream, "HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
    });
    let error = Client::new(None).unwrap().configuration(&endpoint).unwrap_err().to_string();
    assert!(!error.contains("sensitive-provider-token"));
    assert!(error.contains("invitation"));
    server.join().unwrap();
}

#[test]
fn session_must_match_device_and_durable_proof() {
    let device = DeviceId::new(&"1".repeat(32)).unwrap();
    let other_device = DeviceId::new(&"2".repeat(32)).unwrap();
    let proof = "a".repeat(64);
    let response = || Response::Session {
        profile: ParticipantId::new(&"3".repeat(32)).unwrap(),
        device,
        label: "Drawer".into(),
        token: "b".repeat(64),
        device_credential: proof.clone(),
        expires_at_ms: now_ms().unwrap() + 60_000,
    };
    assert!(validate_session(response(), device, &proof).is_ok());
    assert!(validate_session(response(), other_device, &proof).is_err());
    assert!(validate_session(response(), device, &"c".repeat(64)).is_err());
    assert!(validate_session(Response::Done, device, &proof).is_err());
}

#[test]
fn cancellation_precedes_any_network_or_browser_effect() {
    let options = LoginOptions {
        endpoint: SERVICE_ENDPOINT.into(),
        device: DeviceId::new(&"1".repeat(32)).unwrap(),
        device_proof: "a".repeat(64),
        admission: None,
        label: "Drawer".into(),
    };
    let mut browser_called = false;
    let error = Client::new(None)
        .unwrap()
        .sign_in(options, &AtomicBool::new(true), |_| {
            browser_called = true;
            Ok(())
        })
        .err()
        .unwrap();
    assert!(!browser_called);
    assert!(error.to_string().contains("canceled"));
}

#[test]
fn browser_cancellation_cleans_transaction_without_sending_device_proof() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..2 {
            let (mut stream, _) = listener.accept().unwrap();
            stream.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = Vec::new();
            let mut buffer = [0u8; 1024];
            loop {
                let count = stream.read(&mut buffer).unwrap();
                assert!(count > 0);
                request.extend_from_slice(&buffer[..count]);
                if let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    let header = std::str::from_utf8(&request[..header_end]).unwrap();
                    let length: usize = header
                        .lines()
                        .find_map(|line| line.to_ascii_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse().unwrap()))
                        .unwrap();
                    if request.len() >= header_end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(request).unwrap());
            let response = if index == 0 {
                serde_json::json!({"kind":"challenge","transaction":"1".repeat(32),"nonce":"2".repeat(64),"expires_at_ms":now_ms().unwrap()+60_000,"client_id":GOOGLE_CLIENT_ID}).to_string()
            } else {
                r#"{"kind":"done"}"#.into()
            };
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len())
                .unwrap();
        }
        requests
    });
    let cancel = AtomicBool::new(false);
    let options = LoginOptions {
        endpoint,
        device: DeviceId::new(&"3".repeat(32)).unwrap(),
        device_proof: "a".repeat(64),
        admission: Some("b".repeat(64)),
        label: "Drawer".into(),
    };
    let configuration = serde_json::to_vec(&serde_json::json!({"installed":{"client_id":GOOGLE_CLIENT_ID,"client_secret":"synthetic-client-config"}})).unwrap();
    let error = Client::with_installed_client_json(None, &configuration)
        .unwrap()
        .sign_in(options, &cancel, |url| {
            assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
            assert!(!url.contains(&"a".repeat(64)));
            cancel.store(true, Ordering::Release);
            Ok(())
        })
        .err()
        .unwrap();
    assert!(error.to_string().contains("canceled"));
    let requests = server.join().unwrap();
    assert!(requests[0].contains(r#""action":"begin""#));
    assert!(requests[1].contains(r#""action":"cancel""#));
    for request in requests {
        assert!(!request.contains(&"a".repeat(64)));
        assert!(!request.contains(&"b".repeat(64)));
    }
}

#[test]
fn desktop_configuration_requires_pinned_installed_client_and_bounded_secret() {
    let bytes = serde_json::to_vec(&serde_json::json!({"installed":{"client_id":GOOGLE_CLIENT_ID,"client_secret":"synthetic-test-only&=secret","token_uri":"https://untrusted.invalid/ignored"}})).unwrap();
    let client = Client::with_installed_client_json(None, &bytes).unwrap();
    assert_eq!(client.client_secret.as_deref(), Some("synthetic-test-only&=secret"));
    for configuration in [
        serde_json::json!({"web":{"client_id":GOOGLE_CLIENT_ID,"client_secret":"synthetic"}}),
        serde_json::json!({"installed":{"client_id":"other.apps.googleusercontent.com","client_secret":"synthetic"}}),
        serde_json::json!({"installed":{"client_id":GOOGLE_CLIENT_ID,"client_secret":""}}),
        serde_json::json!({"installed":{"client_id":GOOGLE_CLIENT_ID,"client_secret":"x".repeat(257)}}),
        serde_json::json!({"installed":{"client_id":GOOGLE_CLIENT_ID,"client_secret":"private\nvalue"}}),
    ] {
        let failure = Client::with_installed_client_json(None, &serde_json::to_vec(&configuration).unwrap()).err().unwrap().to_string();
        assert!(!failure.contains("synthetic"));
        assert!(!failure.contains("private\nvalue"));
    }
    assert!(installed_client_secret(&vec![b'x'; MAX_AUTH_BYTES + 1]).is_err());
    assert!(installed_client_secret(b"not JSON").is_err());
}
#[test]
fn desktop_token_form_encodes_secret_without_changing_provider_destination_or_pkce() {
    let form = token_form(GOOGLE_CLIENT_ID, "synthetic-code", "synthetic-verifier", "http://127.0.0.1:12345/", Some("synthetic&secret=value")).unwrap();
    let url = reqwest::Url::parse(&format!("{TOKEN_ENDPOINT}?{form}")).unwrap();
    let fields: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(url.origin().ascii_serialization(), "https://oauth2.googleapis.com");
    assert_eq!(fields.get("client_secret").unwrap(), "synthetic&secret=value");
    assert_eq!(fields.get("code_verifier").unwrap(), "synthetic-verifier");
    assert_eq!(fields.get("client_id").unwrap(), GOOGLE_CLIENT_ID);
    let without = token_form(GOOGLE_CLIENT_ID, "synthetic-code", "synthetic-verifier", "http://127.0.0.1:12345/", None).unwrap();
    assert!(!without.contains("client_secret"));
}
#[test]
fn provider_diagnostics_allowlist_missing_client_secret_without_reflecting_description() {
    for (body, expected) in [
        (r#"{"error":"invalid_request","error_description":"client_secret is missing."}"#, "oauth_client_secret"),
        (r#"{"error":"invalid_request","error_description":"private-provider-token"}"#, "oauth_request"),
        (r#"{"error":"invalid_grant","error_description":"private-provider-token"}"#, "oauth_grant"),
        (r#"{"error":"private-provider-token","error_description":"client_secret is missing."}"#, "oauth_provider"),
    ] {
        let error = provider_error(body.as_bytes());
        assert!(matches!(&error,Error::Rejected {code,..} if code==expected));
        assert!(!error.to_string().contains("private-provider-token"));
        assert!(!error.to_string().contains("client_secret is missing."));
    }
}

#[test]
fn unconfigured_build_does_not_start_browser_or_contact_service() {
    let options = LoginOptions {
        endpoint: "http://127.0.0.1:1".into(),
        device: DeviceId::new(&"1".repeat(32)).unwrap(),
        device_proof: "a".repeat(64),
        admission: None,
        label: "Drawer".into(),
    };
    let mut opened = false;
    let error = Client::http_client(None)
        .unwrap()
        .sign_in(options, &AtomicBool::new(false), |_| {
            opened = true;
            Ok(())
        })
        .err()
        .unwrap();
    assert!(!opened);
    assert!(error.to_string().contains("Desktop client configuration"));
}
