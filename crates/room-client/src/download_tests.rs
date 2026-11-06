//! Loopback transport fixtures exercise the real downloader and its ephemeral cache.
//! Responses are deliberately controlled; real authority is tested by the service suite.
use super::*;
use crate::CredentialStore;
use photocraft_party::room::{Action, Assignment, AssignmentView, Phase, Reference, Room};
use photocraft_party::{AssignmentId, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread::JoinHandle;
use std::time::Duration;

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

fn respond(listener: &TcpListener, response: RpcResponse) -> JoinHandle<RpcRequest> {
    let listener = listener.try_clone().unwrap();
    std::thread::spawn(move || {
        listener.set_nonblocking(true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        let (mut stream, _) = loop {
            match listener.accept() {
                Ok(stream) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                Err(error) => panic!("Fixture did not receive its loopback request: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut raw = Vec::new();
        let mut chunk = [0_u8; 4096];
        let (body_start, length) = loop {
            let read = stream.read(&mut chunk).unwrap();
            assert_ne!(read, 0, "Request ended before its headers");
            raw.extend_from_slice(&chunk[..read]);
            assert!(raw.len() <= 512 * 1024);
            if let Some(start) = raw.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                let headers = std::str::from_utf8(&raw[..start]).unwrap();
                let length = headers
                    .lines()
                    .find_map(|line| {
                        let (key, value) = line.split_once(':')?;
                        key.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                break (start + 4, length);
            }
        };
        while raw.len() < body_start + length {
            let read = stream.read(&mut chunk).unwrap();
            assert_ne!(read, 0);
            raw.extend_from_slice(&chunk[..read]);
        }
        let request: RpcRequest = serde_json::from_slice(&raw[body_start..body_start + length]).unwrap();
        let response = serde_json::to_vec(&response).unwrap();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", response.len()).unwrap();
        stream.write_all(&response).unwrap();
        request
    })
}

fn setup(bytes: &[u8]) -> (tempfile::TempDir, TcpListener, Outbox, AssetRef) {
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let mut store = Outbox::open(root.path().join("room.sqlite3"), Box::new(FixtureCredentials)).unwrap();
    store.configure(&endpoint, "Download fixture").unwrap();
    let pair = store.next().unwrap().unwrap();
    let Operation::Pair { participant, .. } = &pair.operation else { panic!("Pair required") };
    store.acknowledge(&pair, &RpcResponse::Paired { participant: participant.clone() }).unwrap();
    let mut room = Room::new(RoomId::new(&"1".repeat(32)).unwrap(), participant.clone(), Mode::ExquisiteCorpse, None).unwrap();
    let guest = Participant { id: ParticipantId::new(&"2".repeat(32)).unwrap(), device: DeviceId::new(&"3".repeat(32)).unwrap(), display_name: "Guest".into() };
    room.apply(guest.id, 0, 0, Action::Join { participant: guest }).unwrap();
    let asset = AssetRef { hash: AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap(), bytes: bytes.len() as u64 };
    let mut view = room.view(participant.id).unwrap();
    view.phase = Phase::Drawing;
    view.round = 1;
    view.epoch = 2;
    view.assignment = Some(AssignmentView {
        assignment: Assignment {
            id: AssignmentId { game: GameId::new(&"4".repeat(32)).unwrap(), chain: 1, round: 1 },
            player: participant.id,
            project: ProjectId::new(&"5".repeat(32)).unwrap(),
        },
        submitted: false,
        reference: Some(Reference::Seam { image: asset, width: 1024, height: 24 }),
    });
    store.update_poll(view).unwrap();
    (root, listener, store, asset)
}

#[test]
fn exact_chunks_are_hash_checked_before_one_image_event_and_no_source_asset_fetch() {
    let bytes = vec![7_u8; CHUNK_BYTES + 13];
    let (_root, listener, store, asset) = setup(&bytes);
    let transport = Transport::new(None).unwrap();
    let (events, received) = mpsc::sync_channel(8);
    let mut downloads = Downloads::default();
    for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
        let server = respond(&listener, RpcResponse::AssetChunk { bytes: chunk.to_vec(), total: bytes.len() as u64 });
        downloads.step(&store, &transport, &events).unwrap();
        let request = server.join().unwrap();
        assert!(request.operation == Operation::Fetch { room: store.status().room.unwrap().id, hash: asset.hash, offset: (index * CHUNK_BYTES) as u64 });
        assert!(received.try_recv().is_err(), "No partial or unchecked bytes may reach the renderer");
    }
    downloads.step(&store, &transport, &events).unwrap();
    let ClientEvent::Image { asset: delivered, bytes: actual, .. } = received.try_recv().unwrap() else { panic!("Image expected") };
    assert_eq!(delivered, asset);
    assert_eq!(*actual, bytes);
    downloads.acknowledge(store.status().room.unwrap().id, asset.hash);
    downloads.step(&store, &transport, &events).unwrap();
    assert!(received.try_recv().is_err(), "A completed image is not repeatedly sent");
}

#[test]
fn invalid_length_empty_oversized_and_wrong_checksum_chunks_never_enter_complete_cache() {
    for response in [
        RpcResponse::AssetChunk { bytes: vec![1, 2, 3], total: 4 },
        RpcResponse::AssetChunk { bytes: vec![], total: 3 },
        RpcResponse::AssetChunk { bytes: vec![1; CHUNK_BYTES + 1], total: 3 },
        RpcResponse::AssetChunk { bytes: vec![9, 9, 9], total: 3 },
        RpcResponse::AssetChunk { bytes: vec![1, 2, 3, 4], total: 3 },
    ] {
        let (_root, listener, store, _) = setup(&[1, 2, 3]);
        let transport = Transport::new(None).unwrap();
        let (events, received) = mpsc::sync_channel(8);
        let mut downloads = Downloads::default();
        let server = respond(&listener, response);
        assert!(downloads.step(&store, &transport, &events).is_err());
        server.join().unwrap();
        assert!(downloads.partial.is_none());
        assert!(downloads.complete.is_empty());
        assert!(received.try_recv().is_err());
        assert!(downloads.retry.is_some_and(|(_, next)| next > Instant::now()));
        // There is no responding server now. A second call must yield immediately
        // so the room worker can poll authoritative state or flush pending edits.
        downloads.step(&store, &transport, &events).unwrap();
    }
}

#[test]
fn assignment_change_revokes_partial_and_unsent_complete_images_before_publication() {
    for complete in [false, true] {
        let bytes = vec![7_u8; if complete { 128 } else { CHUNK_BYTES + 13 }];
        let (_root, listener, mut store, _) = setup(&bytes);
        let transport = Transport::new(None).unwrap();
        let (events, received) = mpsc::sync_channel(8);
        let mut downloads = Downloads::default();
        let first = bytes.iter().take(CHUNK_BYTES).copied().collect();
        let server = respond(&listener, RpcResponse::AssetChunk { bytes: first, total: bytes.len() as u64 });
        downloads.step(&store, &transport, &events).unwrap();
        server.join().unwrap();
        assert_eq!(downloads.partial.is_none(), complete);
        assert_eq!(downloads.complete.is_empty(), !complete);
        let mut next = store.status().room.unwrap();
        next.revision += 1;
        next.round = 2;
        next.epoch += 1;
        next.assignment.as_mut().unwrap().assignment.id.round = 2;
        next.assignment.as_mut().unwrap().reference = Some(Reference::Missing);
        store.update_poll(next).unwrap();
        downloads.step(&store, &transport, &events).unwrap();
        assert!(downloads.partial.is_none());
        assert!(downloads.complete.is_empty());
        assert!(received.try_recv().is_err());
    }
}

#[test]
fn pending_durable_work_precedes_presentation_fetches_and_early_notes_make_no_request() {
    let (_root, _listener, mut store, _) = setup(&[1, 2, 3]);
    let transport = Transport::new(None).unwrap();
    let (events, received) = mpsc::sync_channel(8);
    let mut downloads = Downloads::default();
    let view = store.status().room.unwrap();
    store.enqueue(Operation::Apply { room: view.id, expected_revision: view.revision, action: Box::new(Action::Pause) }).unwrap();
    downloads.step(&store, &transport, &events).unwrap();
    assert!(downloads.partial.is_none());
    assert!(downloads.complete.is_empty());
    assert!(received.try_recv().is_err());
    assert!(notes(&store, &transport).is_err());
}

#[test]
fn image_rejected_during_status_race_is_republished_until_exact_room_ack() {
    let bytes = [1, 2, 3];
    let (_root, listener, store, asset) = setup(&bytes);
    let transport = Transport::new(None).unwrap();
    let (events, received) = mpsc::sync_channel(8);
    let mut downloads = Downloads::default();
    let server = respond(&listener, RpcResponse::AssetChunk { bytes: bytes.to_vec(), total: 3 });
    downloads.step(&store, &transport, &events).unwrap();
    server.join().unwrap();
    downloads.step(&store, &transport, &events).unwrap();
    assert!(matches!(received.try_recv().unwrap(), ClientEvent::Image { .. }));
    downloads.step(&store, &transport, &events).unwrap();
    assert!(received.try_recv().is_err(), "Publication retries are throttled");
    downloads.acknowledge(RoomId::new(&"9".repeat(32)).unwrap(), asset.hash);
    // Simulate the one-second publication deadline without adding a slow sleep.
    downloads.complete.get_mut(&asset.hash).unwrap().next_publication = Some(Instant::now());
    downloads.step(&store, &transport, &events).unwrap();
    assert!(matches!(received.try_recv().unwrap(), ClientEvent::Image { .. }));
    downloads.acknowledge(store.status().room.unwrap().id, asset.hash);
    downloads.complete.get_mut(&asset.hash).unwrap().next_publication = Some(Instant::now());
    downloads.step(&store, &transport, &events).unwrap();
    assert!(received.try_recv().is_err(), "Only a consumer ACK suppresses retransmission");
}

#[test]
fn failed_image_retry_is_bounded_and_a_revoked_grant_clears_the_backoff() {
    let (_root, _listener, mut store, asset) = setup(&[1, 2, 3]);
    let mut downloads = Downloads { room: Some(store.status().room.as_ref().unwrap().id), ..Downloads::default() };
    for _ in 0..100 {
        downloads.defer(asset);
    }
    assert_eq!(downloads.failures, 5);
    let until = downloads.retry.unwrap().1.saturating_duration_since(Instant::now());
    assert!(until <= Duration::from_secs(8) && until > Duration::from_secs(7));
    let mut view = store.status().room.unwrap();
    view.assignment.as_mut().unwrap().reference = Some(Reference::Missing);
    store.update_poll(view).unwrap();
    let (events, _) = mpsc::sync_channel(8);
    downloads.step(&store, &Transport::new(None).unwrap(), &events).unwrap();
    assert!(downloads.retry.is_none());
    assert_eq!(downloads.failures, 0);
}

#[test]
fn explicit_notes_response_carries_the_requested_game_identity_to_its_consumer() {
    use photocraft_party::room::RevealedEntry;
    use photocraft_party::{CheckpointId, CheckpointRef, ReplayId, ReplayRef};
    let (_root, listener, mut store, asset) = setup(&[1, 2, 3]);
    let mut view = store.status().room.unwrap();
    let assigned = view.assignment.as_ref().unwrap().assignment.clone();
    view.phase = Phase::Reveal;
    let checkpoint = CheckpointRef { id: CheckpointId::new(&"6".repeat(32)).unwrap(), asset };
    view.reveal = vec![RevealedEntry::Artwork {
        assignment: assigned.clone(),
        revision: 1,
        image: asset,
        checkpoint,
        replay: Box::new(ReplayRef {
            id: ReplayId::new(&"7".repeat(32)).unwrap(),
            manifest: None,
            baseline: checkpoint,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
        }),
        assets: vec![asset],
    }];
    view.assignment = None;
    store.update_poll(view.clone()).unwrap();
    let expected = vec![(assigned.id, "Handoff text only".into())];
    let server = respond(&listener, RpcResponse::Notes { notes: expected.clone() });
    let event = notes(&store, &Transport::new(None).unwrap()).unwrap();
    assert!(server.join().unwrap().operation == Operation::Notes { room: view.id });
    let ClientEvent::Notes { room, game, epoch, notes } = event else { panic!("Notes event expected") };
    assert_eq!(room, view.id);
    assert_eq!(game, assigned.id.game);
    assert_eq!(epoch, view.epoch);
    assert_eq!(notes, expected);
}
