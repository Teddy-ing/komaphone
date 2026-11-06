//! Untrusted wire decoding and high-entropy credential shape, independently exercised.
use photocraft_party::{
    Error, MAX_MESSAGE_BYTES, RequestId,
    protocol::{CHUNK_BYTES, Operation, PROTOCOL_VERSION, RpcRequest, validate_secret},
};
use serde_json::json;

fn wire() -> serde_json::Value {
    json!({
        "protocol_version":PROTOCOL_VERSION,
        "request_id":"11111111111111111111111111111111",
        "operation":{"kind":"poll","room":"22222222222222222222222222222222","after_revision":0}
    })
}

#[test]
fn protocol_requires_exact_known_version_and_fields() {
    let mut value = wire();
    assert!(RpcRequest::decode(&serde_json::to_vec(&value).unwrap()).is_ok());
    value["protocol_version"] = json!(2);
    assert!(matches!(RpcRequest::decode(&serde_json::to_vec(&value).unwrap()), Err(Error::Version(2))));
    value["protocol_version"] = json!(PROTOCOL_VERSION);
    value["actor"] = json!("33333333333333333333333333333333");
    assert!(RpcRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err(), "body cannot claim a principal");
    value.as_object_mut().unwrap().remove("actor");
    value["operation"]["actor"] = json!("33333333333333333333333333333333");
    assert!(RpcRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    value["operation"] = json!({"kind":"trusted_tick","now_ms":1});
    assert!(RpcRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err(), "trusted clock must not be a client method");
}

#[test]
fn malformed_ids_types_and_oversize_bytes_fail_without_panicking() {
    for malformed in ["", "0", "00000000000000000000000000000000", "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "éééééééééééééééé"] {
        let mut value = wire();
        value["request_id"] = json!(malformed);
        assert!(RpcRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for revision in [json!(-1), json!(1.5), json!("1"), json!(null)] {
        let mut value = wire();
        value["operation"]["after_revision"] = revision;
        assert!(RpcRequest::decode(&serde_json::to_vec(&value).unwrap()).is_err());
    }
    for bytes in [vec![b' '; MAX_MESSAGE_BYTES + 1], b"{\"protocol_version\":1".to_vec(), vec![0xff]] {
        let result = std::panic::catch_unwind(|| RpcRequest::decode(&bytes));
        assert!(result.is_ok());
        assert!(result.unwrap().is_err());
    }
}

#[test]
fn maximum_binary_chunk_fits_json_envelope_and_round_trips_exactly() {
    let request = RpcRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new("11111111111111111111111111111111").unwrap(),
        operation: Operation::Upload {
            room: photocraft_party::RoomId::new("22222222222222222222222222222222").unwrap(),
            hash: photocraft_party::AssetHash::new(&"3".repeat(64)).unwrap(),
            total: CHUNK_BYTES as u64,
            offset: 0,
            bytes: vec![255; CHUNK_BYTES],
            finish: true,
        },
    };
    let bytes = serde_json::to_vec(&request).unwrap();
    assert!(bytes.len() <= MAX_MESSAGE_BYTES);
    assert!(RpcRequest::decode(&bytes).unwrap() == request);
}

#[test]
fn secret_validation_rejects_short_zero_unicode_or_noncanonical_values() {
    assert!(validate_secret(&"1".repeat(64)).is_ok());
    for value in ["1".repeat(63), "1".repeat(65), "0".repeat(64), "A".repeat(64), "é".repeat(32), "g".repeat(64)] {
        assert!(validate_secret(&value).is_err());
    }
}
