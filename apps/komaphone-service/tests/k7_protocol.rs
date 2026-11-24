//! Real HTTP version failures preserve accepted canvas state and do not echo input.
use photocraft_party::{
    DeviceId, Participant, ParticipantId, RequestId,
    protocol::{Fault, Operation, RpcRequest},
};
use photocraft_scene::world_protocol::{Action, Request, Response, VERSION};
use photocraft_sync::{StoreOptions, server};
use reqwest::StatusCode;

#[tokio::test]
async fn canvas_version_mismatch_is_upgrade_required_and_preserves_existing_canvas() {
    let directory = tempfile::tempdir().unwrap();
    let service = server::spawn("127.0.0.1:0".parse().unwrap(), directory.path().join("service.sqlite3"), StoreOptions::default()).await.unwrap();
    let client = reqwest::Client::new();
    let base = format!("http://{}", service.address);
    let token = "1".repeat(64);
    let participant = Participant {
        id: ParticipantId::new(&format!("{:032x}", 1)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", 2)).unwrap(),
        display_name: "Protocol fixture".into(),
    };
    let paired = client
        .post(format!("{base}/v1/rpc"))
        .json(&RpcRequest {
            protocol_version: photocraft_party::protocol::PROTOCOL_VERSION,
            request_id: RequestId::new(&format!("{:032x}", 3)).unwrap(),
            operation: Operation::Pair { participant, credential: token.clone() },
        })
        .send()
        .await
        .unwrap();
    assert_eq!(paired.status(), StatusCode::OK);
    let canvas = photocraft_scene::CanvasId::new(&format!("{:032x}", 4)).unwrap();
    let endpoint = format!("{base}/v1/canvas");
    let created = client
        .post(&endpoint)
        .bearer_auth(&token)
        .json(&Request { version: VERSION, action: Action::Create { canvas, name: "Acknowledged canvas".into() } })
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let before: serde_json::Value = created.json().await.unwrap();
    for version in [0, 1, VERSION + 1, u16::MAX] {
        let failed = client
            .post(&endpoint)
            .bearer_auth(&token)
            .json(&Request {
                version,
                action: Action::Create { canvas: photocraft_scene::CanvasId::new(&format!("{:032x}", 5)).unwrap(), name: "Must not be created".into() },
            })
            .send()
            .await
            .unwrap();
        assert_eq!(failed.status(), StatusCode::UPGRADE_REQUIRED);
        let fault: Fault = failed.json().await.unwrap();
        assert_eq!(fault.code, "version");
        assert!(fault.message.contains("version"));
    }
    let info = client.post(&endpoint).bearer_auth(&token).json(&Request { version: VERSION, action: Action::Info { canvas } }).send().await.unwrap();
    assert_eq!(info.status(), StatusCode::OK);
    assert_eq!(info.json::<serde_json::Value>().await.unwrap(), before);
    let list =
        client.post(&endpoint).bearer_auth(&token).json(&Request { version: VERSION, action: Action::List { after: None, limit: 10 } }).send().await.unwrap();
    let Response::Canvases { items, .. } = list.json().await.unwrap() else { panic!("canvas list") };
    assert_eq!(items.len(), 1);
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn malformed_canvas_requests_remain_invalid_without_reflecting_payloads() {
    let directory = tempfile::tempdir().unwrap();
    let service = server::spawn("127.0.0.1:0".parse().unwrap(), directory.path().join("service.sqlite3"), StoreOptions::default()).await.unwrap();
    let client = reqwest::Client::new();
    for body in [
        r#"{"version":1,"action":{"kind":"PRIVATE_INVALID_PAYLOAD"}}"#,
        r#"{"version":999,"action":{"kind":"PRIVATE_INVALID_PAYLOAD"}}"#,
        r#"{"version":"PRIVATE_INVALID_PAYLOAD","action":{"kind":"identity"}}"#,
        r#"{"version":1,"action":{"kind":"identity"},"PRIVATE_INVALID_PAYLOAD":1}"#,
        "PRIVATE_INVALID_PAYLOAD",
    ] {
        let response = client
            .post(format!("http://{}/v1/canvas", service.address))
            .bearer_auth("1".repeat(64))
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let fault: Fault = response.json().await.unwrap();
        assert_eq!(fault.code, "invalid");
        assert!(!fault.message.contains("PRIVATE_INVALID_PAYLOAD"));
    }
    service.shutdown().await.unwrap();
}

#[tokio::test]
async fn party_rpc_v1_client_gets_upgrade_required_and_preserves_room() {
    use photocraft_party::{Mode, protocol::PROTOCOL_VERSION};
    let directory = tempfile::tempdir().unwrap();
    let service = server::spawn("127.0.0.1:0".parse().unwrap(), directory.path().join("service.sqlite3"), StoreOptions::default()).await.unwrap();
    let client = reqwest::Client::new();
    let base = format!("http://{}", service.address);
    let token = "a".repeat(64);
    let participant = Participant {
        id: ParticipantId::new(&format!("{:032x}", 10)).unwrap(),
        device: DeviceId::new(&format!("{:032x}", 11)).unwrap(),
        display_name: "Protocol v2 fixture".into(),
    };
    let health: serde_json::Value = client.get(format!("{base}/health")).send().await.unwrap().json().await.unwrap();
    assert_eq!(health["protocol_version"], PROTOCOL_VERSION);
    let paired = client
        .post(format!("{base}/v1/rpc"))
        .json(&RpcRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(&format!("{:032x}", 12)).unwrap(),
            operation: Operation::Pair { participant, credential: token.clone() },
        })
        .send()
        .await
        .unwrap();
    assert_eq!(paired.status(), StatusCode::OK);
    let created = client
        .post(format!("{base}/v1/rpc"))
        .bearer_auth(&token)
        .json(&RpcRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(&format!("{:032x}", 13)).unwrap(),
            operation: Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None },
        })
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::OK);
    let before: serde_json::Value = created.json().await.unwrap();
    let room_id: photocraft_party::RoomId = serde_json::from_value(before["view"]["id"].clone()).unwrap();
    for bad_version in [0u16, 1, PROTOCOL_VERSION + 1] {
        let failed = client
            .post(format!("{base}/v1/rpc"))
            .bearer_auth(&token)
            .json(&RpcRequest {
                protocol_version: bad_version,
                request_id: RequestId::new(&format!("{:032x}", 20 + u32::from(bad_version))).unwrap(),
                operation: Operation::Poll { room: room_id, after_revision: 0 },
            })
            .send()
            .await
            .unwrap();
        assert_eq!(failed.status(), StatusCode::UPGRADE_REQUIRED);
        let fault: Fault = failed.json().await.unwrap();
        assert_eq!(fault.code, "version");
    }
    let after = client
        .post(format!("{base}/v1/rpc"))
        .bearer_auth(&token)
        .json(&RpcRequest {
            protocol_version: PROTOCOL_VERSION,
            request_id: RequestId::new(&format!("{:032x}", 99)).unwrap(),
            operation: Operation::Poll { room: room_id, after_revision: 0 },
        })
        .send()
        .await
        .unwrap();
    assert_eq!(after.status(), StatusCode::OK);
    assert_eq!(after.json::<serde_json::Value>().await.unwrap()["view"], before["view"]);
    service.shutdown().await.unwrap();
}
