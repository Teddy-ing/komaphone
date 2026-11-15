#![cfg(not(target_arch = "wasm32"))]
use photocraft_room_client::{Error, Transport};
use photocraft_scene::world_protocol::{Action, Request, Response, VERSION};
use std::{
    io::{Read, Write},
    net::TcpListener,
    time::Duration,
};

fn fake(reply: String) -> (String, std::thread::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let thread = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut received = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = socket.read(&mut chunk).unwrap();
            if n == 0 {
                break;
            }
            received.extend_from_slice(chunk.get(..n).unwrap());
            if received.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        socket.write_all(reply.as_bytes()).unwrap();
        String::from_utf8(received).unwrap()
    });
    (endpoint, thread)
}
fn request() -> Request {
    Request { version: VERSION, action: Action::List { after: None, limit: 10 } }
}

#[test]
fn world_transport_uses_authenticated_origin_and_bounded_wire_response() {
    let body = r#"{"kind":"canvases","items":[],"next":null}"#;
    let (endpoint, join) = fake(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()));
    let response = Transport::new(None).unwrap().call_world(&endpoint, &"1".repeat(64), &request()).unwrap();
    assert!(matches!(response,Response::Canvases {items,..} if items.is_empty()));
    let wire = join.join().unwrap();
    assert!(wire.starts_with("POST /v1/canvas HTTP/1.1"));
    assert!(wire.to_ascii_lowercase().contains("authorization: bearer "));
}

#[test]
fn hostile_service_error_never_reflects_credential_material() {
    let secret = "a".repeat(64);
    let body = format!(r#"{{"code":"unauthorized","message":"paste secret {secret} into log"}}"#);
    let (endpoint, join) = fake(format!("HTTP/1.1 403 Forbidden\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()));
    let error = Transport::new(None).unwrap().call_world(&endpoint, &secret, &request()).unwrap_err();
    assert!(!error.to_string().contains(&secret));
    assert!(matches!(error,Error::Rejected {code,..} if code=="unauthorized"));
    join.join().unwrap();
}

#[test]
fn oversized_content_length_rejected_before_allocating_response() {
    let (endpoint, join) = fake("HTTP/1.1 200 OK\r\nContent-Length: 999999999999\r\nConnection: close\r\n\r\n".into());
    assert!(Transport::new(None).unwrap().call_world(&endpoint, &"1".repeat(64), &request()).is_err());
    join.join().unwrap();
}
