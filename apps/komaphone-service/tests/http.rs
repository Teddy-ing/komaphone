//! Real loopback HTTP clients exercising authentication, authority and restart behavior.
use image::ImageEncoder;
use photocraft_party::{
    AssetHash, AssetRef, CheckpointId, CheckpointRef, DeviceId, GameId, Mode, Participant, ParticipantId, ProjectId, ReplayId, ReplayRef, RequestId, RoomId,
    Submission, SubmissionId,
    protocol::{CHUNK_BYTES, MAX_RESPONSE_BYTES, Operation, PROTOCOL_VERSION, RpcRequest, RpcResponse},
    room::{Action, RoomView},
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
        Color::rgb(f32::from(index) / 4.0, 0.2, 0.6),
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn two_and_three_http_clients_submit_reordered_without_hidden_asset_leaks() {
    for count in [2, 3] {
        for mode in [Mode::Animation { rotations: 1 }, Mode::ExquisiteCorpse] {
            let directory = tempfile::tempdir().unwrap();
            let service = server::spawn(
                "127.0.0.1:0".parse().unwrap(),
                directory.path().join("service.sqlite3"),
                StoreOptions { heartbeat_timeout_ms: 60_000, ..Default::default() },
            )
            .await
            .unwrap();
            let (clients, room) = room_with(&service, count, mode).await;
            let outsider = Client::pair(service.address, 9).await;
            let mut submissions = Vec::new();
            for (index, client) in clients.iter().enumerate() {
                submissions.push(submission(client, room, index as u16 + 1, mode).await);
            }
            let base = clients[0].poll(room).await;
            let messages: Vec<_> = submissions
                .iter()
                .map(|value| {
                    request(Operation::Apply {
                        room,
                        expected_revision: base.revision,
                        action: Box::new(Action::Submit { epoch: base.epoch, submission: value.clone() }),
                    })
                })
                .collect();
            let mut receipts = Vec::new();
            for (client, message) in clients.iter().zip(&messages).rev() {
                receipts.push(client.send(message).await);
            }
            let current = clients[0].poll(room).await;
            assert_eq!(current.round, 1);
            for (client, message) in clients.iter().zip(&messages) {
                client.send(message).await;
            }
            assert_eq!(clients[0].poll(room).await.revision, current.revision);
            let prior = &submissions[0];
            for client in [&clients[1], &outsider] {
                client.rejected(Operation::Fetch { room, hash: prior.checkpoint.asset.hash, offset: 0 }, StatusCode::FORBIDDEN).await;
            }
            let allowed = if mode == Mode::ExquisiteCorpse { prior.seam.unwrap() } else { prior.image };
            let fetched = clients[1].call(Operation::Fetch { room, hash: allowed.hash, offset: 0 }).await;
            assert!(matches!(fetched, RpcResponse::AssetChunk { bytes, .. } if !bytes.is_empty()));
            outsider.rejected(Operation::Fetch { room, hash: allowed.hash, offset: 0 }, StatusCode::FORBIDDEN).await;
            if mode == Mode::ExquisiteCorpse {
                clients[1].rejected(Operation::Fetch { room, hash: prior.image.hash, offset: 0 }, StatusCode::FORBIDDEN).await;
            }
            let retry = clients[0].send(&messages[0]).await;
            let payload = serde_json::to_string(&retry).unwrap();
            assert!(!payload.contains(prior.checkpoint.asset.hash.as_str()));
            assert!(!payload.contains(&prior.note));
            assert_eq!(room_view(retry).round, 1);
            drop(receipts);
            service.shutdown().await.unwrap();
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn heartbeat_host_loss_claim_and_service_restart_restore_real_http_clients() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("service.sqlite3");
    let service = server::spawn("127.0.0.1:0".parse().unwrap(), &path, StoreOptions { heartbeat_timeout_ms: 1000, ..Default::default() }).await.unwrap();
    let (mut clients, room) = room_with(&service, 3, Mode::ExquisiteCorpse).await;
    let old_assignment = clients[1].poll(room).await.assignment;
    // Host makes no further requests; the other two independently keep their leases alive.
    for _ in 0..15 {
        clients[1].poll(room).await;
        clients[2].poll(room).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let paused = clients[1].poll(room).await;
    assert!(paused.paused.is_some());
    assert!(!paused.members.iter().find(|m| m.participant.id == clients[0].profile.id).unwrap().connected);
    let claim = request(Operation::Apply { room, expected_revision: paused.revision, action: Box::new(Action::ClaimModerator) });
    clients[1].send(&claim).await;
    clients[2].rejected(Operation::Apply { room, expected_revision: paused.revision, action: Box::new(Action::ClaimModerator) }, StatusCode::CONFLICT).await;
    service.shutdown().await.unwrap();
    let restarted = server::spawn("127.0.0.1:0".parse().unwrap(), &path, StoreOptions::default()).await.unwrap();
    for client in &mut clients {
        client.endpoint = format!("http://{}/v1/rpc", restarted.address);
    }
    for client in &clients {
        client.poll(room).await;
    }
    let restored = clients[1].poll(room).await;
    assert_eq!(restored.assignment, old_assignment);
    assert_eq!(restored.moderator, clients[1].profile.id);
    assert!(restored.paused.is_some());
    assert!(clients[1].apply(room, Action::Resume).await.paused.is_none());
    restarted.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_rejects_version_oversize_compression_and_forged_authentication() {
    let directory = tempfile::tempdir().unwrap();
    let service = server::spawn("127.0.0.1:0".parse().unwrap(), directory.path().join("service.sqlite3"), StoreOptions::default()).await.unwrap();
    let client = Client::pair(service.address, 1).await;
    let mut message = request(Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None });
    message.protocol_version += 1;
    assert_eq!(client.http.post(&client.endpoint).bearer_auth(&client.token).json(&message).send().await.unwrap().status(), StatusCode::UPGRADE_REQUIRED);
    assert_eq!(
        client.http.post(&client.endpoint).body(vec![b' '; photocraft_party::MAX_MESSAGE_BYTES + 1]).send().await.unwrap().status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let valid = request(Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None });
    assert_eq!(client.http.post(&client.endpoint).header("Content-Encoding", "gzip").json(&valid).send().await.unwrap().status(), StatusCode::BAD_REQUEST);
    assert_eq!(client.http.post(&client.endpoint).bearer_auth("b".repeat(64)).json(&valid).send().await.unwrap().status(), StatusCode::FORBIDDEN);
    assert_eq!(client.http.post(&client.endpoint).json(&valid).send().await.unwrap().status(), StatusCode::FORBIDDEN);
    let cross_site = request(Operation::Pair { participant: person(8), credential: "8".repeat(64) });
    assert_eq!(
        client.http.post(&client.endpoint).header("Origin", "https://untrusted.invalid").json(&cross_site).send().await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert!(server::spawn("0.0.0.0:0".parse().unwrap(), directory.path().join("unsafe.sqlite3"), StoreOptions::default()).await.is_err());
    service.shutdown().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn real_lobby_leave_requires_reinvitation_then_active_leave_preserves_assignment() {
    for count in [2, 3] {
        let directory = tempfile::tempdir().unwrap();
        let service = server::spawn("127.0.0.1:0".parse().unwrap(), directory.path().join("service.sqlite3"), StoreOptions::default()).await.unwrap();
        let mut clients = Vec::new();
        for n in 1..=count {
            clients.push(Client::pair(service.address, n).await);
        }
        let host = &clients[0];
        let guest = &clients[1];
        let room = room_view(host.call(Operation::Create { mode: Mode::ExquisiteCorpse, timer_ms: None }).await).id;
        host.call(Operation::Invite { room, secret: "a".repeat(64), expires_in_ms: 60_000, uses: 2 }).await;
        for client in clients.iter().skip(1) {
            client.call(Operation::Join { room, invitation: "a".repeat(64) }).await;
        }
        let before = guest.poll(room).await;
        assert_eq!(guest.call(Operation::Apply { room, expected_revision: before.revision, action: Box::new(Action::Leave) }).await, RpcResponse::Disconnected);
        guest.rejected(Operation::Poll { room, after_revision: 0 }, StatusCode::FORBIDDEN).await;
        assert_eq!(host.poll(room).await.members.len(), count as usize - 1);
        host.call(Operation::Invite { room, secret: "b".repeat(64), expires_in_ms: 60_000, uses: 1 }).await;
        let joined = room_view(guest.call(Operation::Join { room, invitation: "b".repeat(64) }).await);
        assert_eq!(joined.members.len(), count as usize);
        assert_eq!(joined.members.last().unwrap().participant, guest.profile);
        for client in &clients {
            client.apply(room, Action::Ready { ready: true }).await;
        }
        let projects = (0..3 * count).map(|n| ProjectId::new(&format!("{:032x}", n + 1000)).unwrap()).collect();
        host.apply(room, Action::Start { game: GameId::new(&format!("{:032x}", 500)).unwrap(), projects }).await;
        let active = guest.poll(room).await;
        guest.call(Operation::Apply { room, expected_revision: active.revision, action: Box::new(Action::Leave) }).await;
        let paused = host.poll(room).await;
        assert_eq!(paused.members.len(), count as usize);
        assert!(paused.paused.is_some());
        assert_eq!(guest.poll(room).await.assignment, active.assignment);
        service.shutdown().await.unwrap();
    }
}
