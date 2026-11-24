//! Actual loopback HTTPS, including trust failure and stalled handshakes.
use photocraft_sync::{StoreOptions, server, tls::TlsConfig};
use std::time::Duration;

const CERT: &[u8] = include_bytes!("fixtures/tls/localhost.pem");
const KEY: &[u8] = include_bytes!("fixtures/tls/localhost-key.pem");

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_requires_trust_and_keeps_working_beside_stalled_or_bad_handshakes() {
    let root = tempfile::tempdir().unwrap();
    let service = server::spawn_tls(
        "127.0.0.1:0".parse().unwrap(),
        root.path().join("service.sqlite3"),
        StoreOptions::default(),
        TlsConfig::from_pem(CERT, KEY).unwrap(),
    )
    .await
    .unwrap();
    let origin = format!("https://{}", service.address);
    let untrusted = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(3)).build().unwrap();
    assert!(untrusted.get(format!("{origin}/health")).send().await.is_err(), "a self-signed endpoint is never implicitly trusted");
    let trusted = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .add_root_certificate(reqwest::Certificate::from_pem(CERT).unwrap())
        .build()
        .unwrap();
    let wrong_host = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(3))
        .add_root_certificate(reqwest::Certificate::from_pem(CERT).unwrap())
        .resolve("wrong-name.invalid", service.address)
        .build()
        .unwrap();
    assert!(wrong_host.get(format!("https://wrong-name.invalid:{}/health", service.address.port())).send().await.is_err());
    let stalled = tokio::net::TcpStream::connect(service.address).await.unwrap();
    let response = trusted.get(format!("{origin}/health")).send().await.unwrap();
    assert!(response.status().is_success());
    let health: serde_json::Value = response.json().await.unwrap();
    assert_eq!(health["service"], "Komaphone");
    let plain = format!("http://{}/health", service.address);
    assert!(trusted.get(plain).send().await.is_err(), "TLS route rejects plaintext");
    let who = photocraft_party::Participant {
        id: photocraft_party::ParticipantId::new(&"1".repeat(32)).unwrap(),
        device: photocraft_party::DeviceId::new(&"2".repeat(32)).unwrap(),
        display_name: "HTTPS test".into(),
    };
    let pair = photocraft_party::protocol::RpcRequest {
        protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
        request_id: photocraft_party::RequestId::new(&"3".repeat(32)).unwrap(),
        operation: photocraft_party::protocol::Operation::Pair { participant: who.clone(), credential: "4".repeat(64) },
    };
    let response = trusted.post(format!("{origin}/v1/rpc")).json(&pair).send().await.unwrap();
    assert!(response.status().is_success());
    assert_eq!(response.json::<photocraft_party::protocol::RpcResponse>().await.unwrap(), photocraft_party::protocol::RpcResponse::Paired { participant: who });
    drop(stalled);
    drop(trusted);
    drop(untrusted);
    drop(wrong_host);
    tokio::time::timeout(Duration::from_secs(5), service.shutdown()).await.unwrap().unwrap();
}

#[test]
fn invalid_pem_and_excessive_chain_input_fail_without_panicking() {
    assert!(TlsConfig::from_pem(b"not a certificate", KEY).is_err());
    assert!(TlsConfig::from_pem(CERT, b"not a key").is_err());
    assert!(TlsConfig::from_pem(&vec![0; 65537], KEY).is_err());
    assert!(TlsConfig::from_pem(CERT, &vec![0; 65537]).is_err());
    assert!(TlsConfig::from_pem(&CERT.repeat(9), KEY).is_err());
}
