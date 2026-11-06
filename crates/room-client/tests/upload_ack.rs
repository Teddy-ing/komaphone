//! Regression for identical native artwork uploaded again under fresh request IDs.
//! Responses here exercise real SQLite outbox commits; the native four-game gate
//! independently exercises the same deduplication against the actual HTTP service.
#![cfg(not(target_arch = "wasm32"))]
use photocraft_party::protocol::{CHUNK_BYTES, Operation, RpcResponse};
use photocraft_party::{AssetHash, AssetRef, RoomId};
use photocraft_room_client::{CredentialStore, Outbox, Result};
use sha2::{Digest, Sha256};
use std::path::Path;

struct FixtureCredentials;
impl CredentialStore for FixtureCredentials {
    fn get(&self, _: &str, _: &str) -> Result<Option<String>> {
        Ok(Some("a".repeat(64)))
    }
    fn set(&self, _: &str, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
    fn remove(&self, _: &str, _: &str) -> Result<()> {
        Ok(())
    }
}
fn paired(path: &Path) -> Outbox {
    let mut store = Outbox::open(path, Box::new(FixtureCredentials)).unwrap();
    store.configure("http://127.0.0.1:43127", "Upload recovery fixture").unwrap();
    let pair = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &pair.operation else { panic!("Pair required") };
    store.acknowledge(&pair, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    store
}
fn fixture() -> (AssetRef, Vec<Operation>) {
    let bytes: Vec<u8> = (0..2 * CHUNK_BYTES + 7).map(|index| (index % 251) as u8).collect();
    let asset = AssetRef { hash: AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap(), bytes: bytes.len() as u64 };
    let room = RoomId::new(&"1".repeat(32)).unwrap();
    let operations = bytes
        .chunks(CHUNK_BYTES)
        .enumerate()
        .map(|(index, chunk)| {
            let offset = (index * CHUNK_BYTES) as u64;
            Operation::Upload { room, hash: asset.hash, total: asset.bytes, offset, bytes: chunk.to_vec(), finish: offset + chunk.len() as u64 == asset.bytes }
        })
        .collect();
    (asset, operations)
}

#[test]
fn completed_upload_ack_accepts_fresh_requests_for_identical_content_after_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rooms.sqlite3");
    let mut store = paired(&path);
    let (asset, operations) = fixture();
    let original_ids = store.enqueue_batch(operations.clone()).unwrap();
    while let Some(request) = store.next().unwrap() {
        let Operation::Upload { offset, bytes, finish, .. } = &request.operation else { panic!("Upload required") };
        store.acknowledge(&request, &RpcResponse::Uploaded { asset: finish.then_some(asset), next_offset: offset + bytes.len() as u64 }).unwrap();
    }
    let repeated_ids = store.enqueue_batch(operations).unwrap();
    assert!(repeated_ids.iter().all(|id| !original_ids.contains(id)));
    let first = store.next().unwrap().unwrap();
    // The server already has every byte, so even the first retried chunk names
    // the complete asset and reports the full durable prefix.
    store.acknowledge(&first, &RpcResponse::Uploaded { asset: Some(asset), next_offset: asset.bytes }).unwrap();
    assert_eq!(store.status().pending, 2);
    drop(store);
    let mut reopened = Outbox::open(&path, Box::new(FixtureCredentials)).unwrap();
    while let Some(request) = reopened.next().unwrap() {
        reopened.acknowledge(&request, &RpcResponse::Uploaded { asset: Some(asset), next_offset: asset.bytes }).unwrap();
    }
    assert_eq!(reopened.status().pending, 0);
    assert!(!reopened.status().blocked);
}

#[test]
fn a_later_durable_partial_prefix_acknowledges_retried_chunks_without_claiming_completion() {
    let directory = tempfile::tempdir().unwrap();
    let mut store = paired(&directory.path().join("rooms.sqlite3"));
    let (asset, operations) = fixture();
    store.enqueue_batch(operations).unwrap();
    for _ in 0..2 {
        let request = store.next().unwrap().unwrap();
        store.acknowledge(&request, &RpcResponse::Uploaded { asset: None, next_offset: (2 * CHUNK_BYTES) as u64 }).unwrap();
    }
    let final_chunk = store.next().unwrap().unwrap();
    assert!(store.acknowledge(&final_chunk, &RpcResponse::Uploaded { asset: None, next_offset: asset.bytes }).is_err());
    assert_eq!(store.status().pending, 1);
    assert!(store.next().unwrap().unwrap() == final_chunk);
    store.acknowledge(&final_chunk, &RpcResponse::Uploaded { asset: Some(asset), next_offset: asset.bytes }).unwrap();
    assert_eq!(store.status().pending, 0);
}

#[test]
fn forged_upload_ack_preserves_the_exact_pending_request_and_saved_queue() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("rooms.sqlite3");
    let mut store = paired(&path);
    let (asset, operations) = fixture();
    store.enqueue_batch(operations).unwrap();
    let request = store.next().unwrap().unwrap();
    let wrong_hash = AssetHash::new(&"f".repeat(64)).unwrap();
    for response in [
        RpcResponse::Uploaded { asset: None, next_offset: CHUNK_BYTES as u64 - 1 },
        RpcResponse::Uploaded { asset: None, next_offset: asset.bytes + 1 },
        RpcResponse::Uploaded { asset: Some(asset), next_offset: CHUNK_BYTES as u64 },
        RpcResponse::Uploaded { asset: Some(AssetRef { hash: wrong_hash, ..asset }), next_offset: asset.bytes },
        RpcResponse::Uploaded { asset: Some(AssetRef { bytes: asset.bytes - 1, ..asset }), next_offset: asset.bytes },
    ] {
        assert!(store.acknowledge(&request, &response).is_err(), "Forged acknowledgment accepted: {response:?}");
        assert_eq!(store.status().pending, 3);
        assert!(store.next().unwrap().unwrap() == request);
    }
    drop(store);
    let reopened = Outbox::open(&path, Box::new(FixtureCredentials)).unwrap();
    assert_eq!(reopened.status().pending, 3);
    assert!(reopened.next().unwrap().unwrap() == request);
}

#[test]
fn malformed_pending_upload_offsets_and_early_finish_cannot_be_acknowledged() {
    for (offset, total, finish) in [(u64::MAX, u64::MAX, false), (10, 5, false), (0, 2, true)] {
        let directory = tempfile::tempdir().unwrap();
        let mut store = paired(&directory.path().join("rooms.sqlite3"));
        let asset = AssetRef { hash: AssetHash::new(&"e".repeat(64)).unwrap(), bytes: total };
        store.enqueue(Operation::Upload { room: RoomId::new(&"1".repeat(32)).unwrap(), hash: asset.hash, total, offset, bytes: vec![1], finish }).unwrap();
        let request = store.next().unwrap().unwrap();
        assert!(store.acknowledge(&request, &RpcResponse::Uploaded { asset: Some(asset), next_offset: total }).is_err());
        assert_eq!(store.status().pending, 1);
        assert!(store.next().unwrap().unwrap() == request);
    }
}
