//! K3 full-game HTTP acceptance: every round, private references, reconnect and immutable reveal.
use image::ImageEncoder;
use photocraft_party::{
    AssetHash, AssetRef, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RequestId, RoomId,
    Submission, SubmissionId,
    protocol::{CHUNK_BYTES, MAX_RESPONSE_BYTES, Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse},
    room::{Action, Phase, Reference, RevealedEntry, RoomView},
};
use photocraft_sync::{StoreOptions, server};
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

static SEQUENCE: AtomicU64 = AtomicU64::new(1);
fn request(operation: Operation) -> RpcRequest {
    RpcRequest {
        protocol_version: PROTOCOL_VERSION,
        request_id: RequestId::new(&format!("{:032x}", SEQUENCE.fetch_add(1, Ordering::Relaxed))).unwrap(),
        operation,
    }
}
fn person(n: u16) -> Participant {
    Participant {
        id: ParticipantId::new(&format!("{n:032x}")).unwrap(),
        device: DeviceId::new(&format!("{:032x}", n + 100)).unwrap(),
        display_name: format!("Player {n}"),
    }
}
struct Client {
    endpoint: String,
    http: reqwest::Client,
    token: String,
    profile: Participant,
    _root: tempfile::TempDir,
}
impl Client {
    async fn pair(address: std::net::SocketAddr, n: u16) -> Self {
        let root = tempfile::tempdir().unwrap();
        let profile = person(n);
        let token = format!("{:064x}", n + 1000);
        // Independent client roots; this fixture is not the native Windows credential adapter.
        std::fs::write(root.path().join("profile.json"), serde_json::to_vec(&profile).unwrap()).unwrap();
        let client = Self {
            endpoint: format!("http://{address}/v1/rpc"),
            http: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).timeout(Duration::from_secs(20)).build().unwrap(),
            token,
            profile: profile.clone(),
            _root: root,
        };
        let response =
            client.http.post(&client.endpoint).json(&request(Operation::Pair { participant: profile, credential: client.token.clone() })).send().await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        client
    }
    async fn send(&self, message: &RpcRequest) -> RpcResponse {
        let response = self.http.post(&self.endpoint).bearer_auth(&self.token).json(message).send().await.unwrap();
        let status = response.status();
        let bytes = response.bytes().await.unwrap();
        assert_eq!(status, StatusCode::OK, "{}", String::from_utf8_lossy(&bytes));
        assert!(bytes.len() <= MAX_RESPONSE_BYTES);
        serde_json::from_slice(&bytes).unwrap()
    }
    async fn call(&self, operation: Operation) -> RpcResponse {
        self.send(&request(operation)).await
    }
    async fn rejected(&self, operation: Operation, code: StatusCode) {
        let response = self.http.post(&self.endpoint).bearer_auth(&self.token).json(&request(operation)).send().await.unwrap();
        assert_eq!(response.status(), code, "{}", response.text().await.unwrap());
    }
    async fn poll(&self, room: RoomId) -> RoomView {
        room_view(self.call(Operation::Poll { room, after_revision: 0 }).await)
    }
    async fn apply(&self, room: RoomId, action: Action) -> RoomView {
        let before = self.poll(room).await;
        room_view(self.call(Operation::Apply { room, expected_revision: before.revision, action: Box::new(action) }).await)
    }
    async fn upload(&self, room: RoomId, bytes: &[u8]) -> AssetRef {
        let asset = AssetRef { hash: AssetHash::new(&format!("{:x}", Sha256::digest(bytes))).unwrap(), bytes: bytes.len() as u64 };
        for (index, chunk) in bytes.chunks(CHUNK_BYTES).enumerate() {
            let offset = index * CHUNK_BYTES;
            let finish = offset + chunk.len() == bytes.len();
            let response =
                self.call(Operation::Upload { room, hash: asset.hash, total: asset.bytes, offset: offset as u64, bytes: chunk.to_vec(), finish }).await;
            assert_eq!(response, RpcResponse::Uploaded { asset: finish.then_some(asset), next_offset: (offset + chunk.len()) as u64 });
        }
        asset
    }
}
fn room_view(response: RpcResponse) -> RoomView {
    match response {
        RpcResponse::Room { view, .. } => *view,
        other => panic!("Expected room: {other:?}"),
    }
}
async fn room_with(service: &server::RunningService, count: u16, mode: Mode) -> (Vec<Client>, RoomId) {
    let mut clients = Vec::new();
    for n in 1..=count {
        clients.push(Client::pair(service.address, n).await);
    }
    let room = room_view(clients[0].call(Operation::Create { mode, timer_ms: None }).await).id;
    let invitation = "a".repeat(64);
    clients[0].call(Operation::Invite { room, secret: invitation.clone(), expires_in_ms: 60_000, uses: 2 }).await;
    for client in clients.iter().skip(1) {
        client.call(Operation::Join { room, invitation: invitation.clone() }).await;
    }
    for client in &clients {
        client.apply(room, Action::Ready { ready: true }).await;
    }
    let rounds = mode.rounds(usize::from(count)).unwrap();
    let projects = (0..u16::from(rounds) * count).map(|n| ProjectId::new(&format!("{:032x}", n + 1000)).unwrap()).collect();
    clients[0].apply(room, Action::Start { game: GameId::new(&format!("{:032x}", 500)).unwrap(), projects }).await;
    (clients, room)
}
fn png(width: u32, height: u32, pixels: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes).write_image(pixels, width, height, image::ExtendedColorType::Rgba8).unwrap();
    bytes
}
async fn submission(client: &Client, room: RoomId, index: u16, mode: Mode) -> Submission {
    use photocraft_doc::{Color, ColorMode, Document, SampleType, Size};
    let assignment = client.poll(room).await.assignment.unwrap().assignment;
    let (width, height) = if mode == Mode::ExquisiteCorpse { (1024, 512) } else { (1536, 1024) };
    let document = Document::with_background(
        "Synthetic service privacy fixture",
        Size { width, height },
        ColorMode::Rgb,
        SampleType::U16,
        Color::rgb(f32::from(index) / 255.0, 0.2, 0.6),
    );
    let checkpoint_bytes = photocraft_format::save_to_bytes(&document, &Default::default()).unwrap();
    let mut rendered = photocraft_compose::flatten(&document);
    photocraft_party::canonical_color::to_srgb(&document, &mut rendered.px).unwrap();
    let rendered = rendered.to_rgba8();
    let image = client.upload(room, &png(width, height, &rendered.pixels)).await;
    let checkpoint = CheckpointRef { id: CheckpointId::new(&format!("{:032x}", index + 2000)).unwrap(), asset: client.upload(room, &checkpoint_bytes).await };
    let seam = if mode == Mode::ExquisiteCorpse {
        let offset = ((height - 24) * width * 4) as usize;
        Some(client.upload(room, &png(width, 24, &rendered.pixels[offset..])).await)
    } else {
        None
    };
    let mut assets = vec![image, checkpoint.asset];
    assets.extend(seam);
    Submission {
        id: SubmissionId::new(&format!("{:032x}", index + 3000)).unwrap(),
        assignment: assignment.id,
        author: client.profile.id,
        project: assignment.project,
        revision: 1,
        image,
        checkpoint,
        replay: ReplayRef {
            id: ReplayId::new(&format!("{:032x}", index + 4000)).unwrap(),
            baseline: checkpoint,
            end_event: None,
            event_count: 0,
            duration_ms: 0,
            manifest: None,
        },
        assets,
        note: format!("private note {index}"),
        seam,
    }
}

async fn fetch(client: &Client, room: RoomId, asset: AssetRef) -> Vec<u8> {
    let mut complete = Vec::new();
    while (complete.len() as u64) < asset.bytes {
        match client.call(Operation::Fetch { room, hash: asset.hash, offset: complete.len() as u64 }).await {
            RpcResponse::AssetChunk { bytes, total } => {
                assert_eq!(total, asset.bytes);
                assert!(!bytes.is_empty());
                complete.extend(bytes);
            }
            other => panic!("Expected asset chunk, got {other:?}"),
        }
    }
    assert_eq!(complete.len() as u64, asset.bytes);
    assert_eq!(format!("{:x}", Sha256::digest(&complete)), asset.hash.as_str());
    complete
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn full_two_and_three_player_games_keep_each_handoff_private_and_survive_reveal_restart() {
    for count in [2, 3] {
        for mode in [Mode::Animation { rotations: 2 }, Mode::ExquisiteCorpse] {
            let directory = tempfile::tempdir().unwrap();
            let database = directory.path().join("service.sqlite3");
            let options = StoreOptions { heartbeat_timeout_ms: 60_000, ..Default::default() };
            let service = server::spawn("127.0.0.1:0".parse().unwrap(), &database, options).await.unwrap();
            let (mut clients, room) = room_with(&service, count, mode).await;
            let outsider = Client::pair(service.address, 9).await;
            let rounds = mode.rounds(usize::from(count)).unwrap();
            let mut accepted: Vec<Submission> = Vec::new();
            let mut requests = Vec::new();
            for round in 0..rounds {
                let mut pending = Vec::new();
                for (player, client) in clients.iter().enumerate() {
                    let view = client.poll(room).await;
                    assert_eq!(view.phase, Phase::Drawing);
                    assert_eq!(view.round, round);
                    assert!(view.reveal.is_empty());
                    let assignment = view.assignment.as_ref().unwrap();
                    assert_eq!(assignment.assignment.player, client.profile.id);
                    assert_eq!((usize::from(assignment.assignment.id.chain) + usize::from(round)) % usize::from(count), player);
                    client.rejected(Operation::Notes { room }, StatusCode::CONFLICT).await;
                    let previous = accepted.iter().find(|s| s.assignment.chain == assignment.assignment.id.chain && s.assignment.round + 1 == round);
                    let payload = serde_json::to_string(&view).unwrap();
                    if let Some(previous) = previous {
                        let permitted = match assignment.reference.as_ref().unwrap() {
                            Reference::Animation { image, note } => {
                                assert!(matches!(mode, Mode::Animation { .. }));
                                assert_eq!(*image, previous.image);
                                assert_eq!(note, &previous.note);
                                *image
                            }
                            Reference::Seam { image, width, height } => {
                                assert_eq!(mode, Mode::ExquisiteCorpse);
                                assert_eq!((*width, *height), (1024, 24));
                                assert_eq!(Some(*image), previous.seam);
                                assert!(!payload.contains(&previous.note));
                                client.rejected(Operation::Fetch { room, hash: previous.image.hash, offset: 0 }, StatusCode::FORBIDDEN).await;
                                *image
                            }
                            Reference::Missing => panic!("Complete game has no missing references"),
                        };
                        let bytes = fetch(client, room, permitted).await;
                        let image = image::load_from_memory(&bytes).unwrap();
                        let expected = if mode == Mode::ExquisiteCorpse { (1024, 24) } else { (1536, 1024) };
                        assert_eq!((image.width(), image.height()), expected);
                        outsider.rejected(Operation::Fetch { room, hash: permitted.hash, offset: 0 }, StatusCode::FORBIDDEN).await;
                        client.rejected(Operation::Fetch { room, hash: previous.checkpoint.asset.hash, offset: 0 }, StatusCode::FORBIDDEN).await;
                    } else {
                        assert_eq!(round, 0);
                        assert!(assignment.reference.is_none());
                    }
                    // Known hashes are not authority: all older/non-adjacent images stay hidden.
                    for old in &accepted {
                        assert!(!payload.contains(old.checkpoint.asset.hash.as_str()));
                        if Some(old.assignment) != previous.map(|s| s.assignment) || mode == Mode::ExquisiteCorpse {
                            assert!(!payload.contains(old.image.hash.as_str()));
                            client.rejected(Operation::Fetch { room, hash: old.image.hash, offset: 0 }, StatusCode::FORBIDDEN).await;
                        }
                    }
                    pending.push(submission(client, room, u16::from(round) * count + player as u16 + 1, mode).await);
                }
                // Simultaneous-client base revisions and reversed delivery must accept once.
                let base = clients[0].poll(room).await;
                let messages: Vec<_> = pending
                    .iter()
                    .map(|value| {
                        request(Operation::Apply {
                            room,
                            expected_revision: base.revision,
                            action: Box::new(Action::Submit { epoch: base.epoch, submission: value.clone() }),
                        })
                    })
                    .collect();
                for (client, message) in clients.iter().zip(&messages).rev() {
                    client.send(message).await;
                }
                let advanced = clients[0].poll(room).await;
                for (client, message) in clients.iter().zip(&messages) {
                    client.send(message).await;
                }
                assert_eq!(clients[0].poll(room).await.revision, advanced.revision);
                requests.extend(messages.into_iter().enumerate());
                accepted.extend(pending);
            }
            let final_view = clients[0].poll(room).await;
            assert_eq!(final_view.phase, Phase::Reveal);
            assert_eq!(final_view.reveal.len(), usize::from(count) * usize::from(rounds));
            assert!(final_view.assignment.is_none());
            let payload = serde_json::to_string(&final_view).unwrap();
            for source in &accepted {
                assert!(!payload.contains(&source.note), "Notes must be explicitly opened");
            }
            for chain in 0..count as u8 {
                let parts: Vec<_> = final_view
                    .reveal
                    .iter()
                    .filter_map(|entry| match entry {
                        RevealedEntry::Artwork { assignment, image, .. } if assignment.id.chain == chain => Some((assignment, image)),
                        _ => None,
                    })
                    .collect();
                assert_eq!(parts.len(), usize::from(rounds));
                for (round, (assignment, image)) in parts.iter().enumerate() {
                    assert_eq!(usize::from(assignment.id.round), round);
                    assert_eq!(assignment.player, clients[(usize::from(chain) + round) % usize::from(count)].profile.id);
                    let source = accepted.iter().find(|s| s.assignment == assignment.id).unwrap();
                    assert_eq!(**image, source.image);
                    fetch(&clients[0], room, **image).await;
                }
                if mode == Mode::ExquisiteCorpse && count == 2 {
                    assert_eq!(parts[0].0.player, parts[2].0.player);
                    assert_ne!(parts[0].0.player, parts[1].0.player);
                }
            }
            match clients[0].call(Operation::Notes { room }).await {
                RpcResponse::Notes { notes } => {
                    assert_eq!(notes.len(), accepted.len());
                    for source in &accepted {
                        assert!(notes.contains(&(source.assignment, source.note.clone())));
                    }
                }
                other => panic!("Expected explicit reveal notes, got {other:?}"),
            }
            service.shutdown().await.unwrap();
            let restarted = server::spawn("127.0.0.1:0".parse().unwrap(), &database, options).await.unwrap();
            for client in &mut clients {
                client.endpoint = format!("http://{}/v1/rpc", restarted.address);
                let restored = client.poll(room).await;
                assert_eq!(restored.phase, Phase::Reveal);
                assert_eq!(restored.reveal, final_view.reveal);
            }
            // Replaying old accepted requests after restart must retain exactly the same art.
            for (index, message) in &requests {
                assert_eq!(room_view(clients[*index].send(message).await).reveal, final_view.reveal);
            }
            let archived = clients[0].apply(room, Action::Archive).await;
            assert_eq!(archived.reveal, final_view.reveal);
            restarted.shutdown().await.unwrap();
        }
    }
}
