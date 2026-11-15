#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::*;
use crate::world_queue::WorldScope;
use photocraft_scene::{
    OwnerId,
    shared::{AcceptedOp, DeviceId, Edit, OpId, WorldOp, media::AssetHash},
};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};

fn info() -> CanvasSummary {
    CanvasSummary {
        canvas: CanvasId::new(&format!("{:032x}", 1)).unwrap(),
        owner: OwnerId::new(&format!("{:032x}", 2)).unwrap(),
        name: "After".into(),
        import_origin: None,
        initial_name: "Before".into(),
        epoch: 1,
        revision: 1,
    }
}
fn server(responses: Vec<(Action, Response)>) -> (ThumbnailReader, std::thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let join = std::thread::spawn(move || {
        for (action, response) in responses {
            let (mut socket, _) = listener.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0; 4096];
            let request = loop {
                let count = socket.read(&mut buffer).unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                assert!(bytes.len() < 65_536);
                if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    assert!(header.lines().any(|line| line.to_ascii_lowercase().starts_with("authorization: bearer ")));
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, value)| value.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break wire::Request::decode(&bytes[end + 4..end + 4 + length]).unwrap();
                    }
                }
            };
            assert_eq!(serde_json::to_value(request.action).unwrap(), serde_json::to_value(action).unwrap());
            let body = serde_json::to_vec(&response).unwrap();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
            socket.write_all(&body).unwrap();
        }
    });
    let access = WorldAccess {
        auth: None,
        scope: WorldScope::new(&endpoint, info().owner, DeviceId::new(&format!("{:032x}", 3)).unwrap()).unwrap(),
        credential: "a".repeat(64),
    };
    (access.thumbnail_reader(None).unwrap(), join)
}
#[test]
fn reader_authorizes_then_replays_only_the_catalogs_fixed_revision() {
    let expected = info();
    let mut latest = expected.clone();
    latest.name = "Later".into();
    latest.revision = 2;
    let event = AcceptedOp {
        revision: 1,
        op: WorldOp {
            id: OpId::new(&format!("{:032x}", 4)).unwrap(),
            canvas: expected.canvas,
            author: expected.owner,
            device: DeviceId::new(&format!("{:032x}", 3)).unwrap(),
            sequence: 1,
            authority_epoch: 1,
            action: Edit::Rename { name: expected.name.clone(), expected_revision: 0 },
        },
    };
    let (reader, join) = server(vec![
        (Action::Info { canvas: expected.canvas }, Response::Canvas { info: latest }),
        (
            Action::Events { canvas: expected.canvas, after: 0, through: Some(1), limit: wire::MAX_PAGE },
            Response::Events { events: vec![event], through: 1, next: None },
        ),
    ]);
    let scene = reader.scene(&expected, &mut || true).unwrap();
    assert_eq!(scene.revision, 1);
    assert_eq!(scene.name, "After");
    join.join().unwrap();
    let mut changed = expected.clone();
    changed.epoch = 2;
    let (reader, join) = server(vec![(Action::Info { canvas: expected.canvas }, Response::Canvas { info: changed })]);
    assert!(reader.scene(&expected, &mut || true).is_err());
    join.join().unwrap();
    assert!(reader.scene(&expected, &mut || false).is_err());
}
#[test]
fn reader_media_requires_contiguous_bounded_chunks_and_exact_checksum() {
    let bytes = b"accepted frame bytes".to_vec();
    let frame = MediaFrame {
        hash: AssetHash::new(&format!("{:x}", Sha256::digest(&bytes))).unwrap(),
        byte_len: bytes.len() as u64,
        width: 1,
        height: 1,
        duration_ms: 100,
    };
    let canvas = info().canvas;
    let (reader, join) = server(vec![
        (Action::FetchMedia { canvas, hash: frame.hash, offset: 0 }, Response::MediaChunk { bytes: bytes[..8].to_vec(), total: frame.byte_len }),
        (Action::FetchMedia { canvas, hash: frame.hash, offset: 8 }, Response::MediaChunk { bytes: bytes[8..].to_vec(), total: frame.byte_len }),
    ]);
    assert_eq!(reader.frame(canvas, &frame, &mut || true).unwrap(), bytes);
    join.join().unwrap();
    let (reader, join) =
        server(vec![(Action::FetchMedia { canvas, hash: frame.hash, offset: 0 }, Response::MediaChunk { bytes: bytes.clone(), total: frame.byte_len + 1 })]);
    assert!(reader.frame(canvas, &frame, &mut || true).is_err());
    join.join().unwrap();
    let (reader, join) =
        server(vec![(Action::FetchMedia { canvas, hash: frame.hash, offset: 0 }, Response::MediaChunk { bytes: vec![0; bytes.len()], total: frame.byte_len })]);
    assert!(reader.frame(canvas, &frame, &mut || true).unwrap_err().to_string().contains("checksum"));
    join.join().unwrap();
}
