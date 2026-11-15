use super::{Result, canceled, check_cancel, invalid};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

const MAX_CALLBACK_BYTES: usize = 8 * 1024;
const MAX_CALLBACK_REQUESTS: usize = 128;

pub(super) struct Proof {
    pub state: String,
    pub verifier: String,
}
impl Proof {
    pub fn new() -> Result<Self> {
        Ok(Self { state: random_secret()?, verifier: random_secret()? })
    }
    pub fn authorization_url(&self, client_id: &str, nonce: &str, redirect_uri: &str) -> Result<reqwest::Url> {
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(self.verifier.as_bytes()));
        reqwest::Url::parse_with_params(
            "https://accounts.google.com/o/oauth2/v2/auth",
            &[
                ("client_id", client_id),
                ("redirect_uri", redirect_uri),
                ("response_type", "code"),
                ("scope", "openid email profile"),
                ("code_challenge_method", "S256"),
                ("code_challenge", &challenge),
                ("state", &self.state),
                ("nonce", nonce),
                ("access_type", "online"),
                ("prompt", "select_account"),
            ],
        )
        .map_err(|_| invalid("Cannot prepare the Google sign-in browser request"))
    }
}

fn random_secret() -> Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|_| invalid("Secure random generation failed; sign-in did not start"))?;
    if bytes.iter().all(|byte| *byte == 0) {
        return Err(invalid("Secure random generation failed; sign-in did not start"));
    }
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

pub(super) struct Callback {
    listener: TcpListener,
    port: u16,
}
impl Callback {
    pub fn bind() -> Result<Self> {
        let listener =
            TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).map_err(|_| invalid("Cannot open the local sign-in callback; try again"))?;
        listener.set_nonblocking(true).map_err(|_| invalid("Cannot configure the local sign-in callback"))?;
        let port = listener.local_addr().map_err(|_| invalid("Cannot read the local sign-in callback address"))?.port();
        Ok(Self { listener, port })
    }
    pub fn redirect_uri(&self) -> String {
        format!("http://127.0.0.1:{}/", self.port)
    }
    pub fn receive(&self, expected_state: &str, cancel: &AtomicBool, deadline: Instant) -> Result<String> {
        let mut requests = 0usize;
        loop {
            check_cancel(cancel)?;
            if Instant::now() >= deadline {
                return Err(invalid("Sign-in timed out; choose Sign in with Google to try again"));
            }
            match self.listener.accept() {
                Ok((mut stream, address)) => {
                    requests = requests.saturating_add(1);
                    if requests > MAX_CALLBACK_REQUESTS {
                        return Err(invalid("Too many local callback requests; restart sign-in"));
                    }
                    if !address.ip().is_loopback() {
                        continue;
                    }
                    let request = read_request(&mut stream, cancel, deadline);
                    let parsed = request.and_then(|request| parse_request(&request, self.port, expected_state));
                    match parsed {
                        Ok(CallbackResult::Code(code)) => {
                            reply(&mut stream, true);
                            return Ok(code);
                        }
                        Ok(CallbackResult::Denied) => {
                            reply(&mut stream, true);
                            return Err(canceled());
                        }
                        Err(_) => reply(&mut stream, false),
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(20)),
                Err(_) => return Err(invalid("Local sign-in callback failed; try again")),
            }
        }
    }
}

enum CallbackResult {
    Code(String),
    Denied,
}

fn read_request(stream: &mut TcpStream, cancel: &AtomicBool, deadline: Instant) -> Result<Vec<u8>> {
    stream.set_nonblocking(true).map_err(|_| invalid("Cannot configure local callback"))?;
    let request_deadline = deadline.min(Instant::now() + Duration::from_secs(2));
    let mut bytes = Vec::with_capacity(1024);
    let mut buffer = [0u8; 1024];
    loop {
        check_cancel(cancel)?;
        if Instant::now() >= request_deadline {
            return Err(invalid("Incomplete local sign-in callback"));
        }
        match stream.read(&mut buffer) {
            Ok(0) => return Err(invalid("Incomplete local sign-in callback")),
            Ok(count) => {
                bytes.extend_from_slice(buffer.get(..count).ok_or_else(|| invalid("Invalid local callback length"))?);
                if bytes.len() > MAX_CALLBACK_BYTES {
                    return Err(invalid("Local sign-in callback exceeds its byte limit"));
                }
                if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
                    return Ok(bytes);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => return Err(invalid("Cannot read local sign-in callback")),
        }
    }
}

fn parse_request(bytes: &[u8], port: u16, expected_state: &str) -> Result<CallbackResult> {
    if bytes.len() > MAX_CALLBACK_BYTES || !bytes.ends_with(b"\r\n\r\n") {
        return Err(invalid("Invalid local sign-in callback"));
    }
    let request = std::str::from_utf8(bytes).map_err(|_| invalid("Invalid local sign-in callback"))?;
    let mut lines = request.split("\r\n");
    let mut first = lines.next().unwrap_or_default().split(' ');
    let method = first.next().unwrap_or_default();
    let target = first.next().unwrap_or_default();
    let version = first.next().unwrap_or_default();
    if method != "GET" || !matches!(version, "HTTP/1.1" | "HTTP/1.0") || first.next().is_some() || !target.starts_with("/?") || target.contains('#') {
        return Err(invalid("Invalid local sign-in callback method or path"));
    }
    let expected_host = format!("127.0.0.1:{port}");
    let mut found_host = false;
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line.split_once(':').ok_or_else(|| invalid("Invalid local callback header"))?;
        if name.eq_ignore_ascii_case("host") {
            if found_host || value.trim() != expected_host {
                return Err(invalid("Invalid local callback host"));
            }
            found_host = true;
        }
        if name.eq_ignore_ascii_case("transfer-encoding") || (name.eq_ignore_ascii_case("content-length") && value.trim() != "0") {
            return Err(invalid("Local sign-in callback must not contain a body"));
        }
    }
    if !found_host {
        return Err(invalid("Missing local callback host"));
    }
    let url = reqwest::Url::parse(&format!("http://{expected_host}{target}")).map_err(|_| invalid("Invalid local sign-in callback query"))?;
    let mut state = None;
    let mut code = None;
    let mut error = None;
    for (name, value) in url.query_pairs() {
        let slot = match name.as_ref() {
            "state" => &mut state,
            "code" => &mut code,
            "error" => &mut error,
            _ => continue,
        };
        if slot.is_some() || value.is_empty() || value.len() > 4096 || value.chars().any(char::is_control) {
            return Err(invalid("Invalid or duplicate local callback parameter"));
        }
        *slot = Some(value.into_owned());
    }
    if !state.as_deref().is_some_and(|state| constant_time_equal(state, expected_state)) {
        return Err(invalid("Local callback did not match this sign-in request"));
    }
    match (code, error) {
        (Some(code), None) => Ok(CallbackResult::Code(code)),
        (None, Some(_)) => Ok(CallbackResult::Denied),
        _ => Err(invalid("Local callback must contain one sign-in result")),
    }
}

pub(super) fn constant_time_equal(left: &str, right: &str) -> bool {
    left.len() == right.len() && left.bytes().zip(right.bytes()).fold(0u8, |difference, (a, b)| difference | (a ^ b)) == 0
}

fn reply(stream: &mut TcpStream, accepted: bool) {
    // Fixed text never reflects the callback URL, authorization code, state or errors.
    let body = if accepted {
        "Komaphone received the browser response. Sign-in is not complete yet. Return to Komaphone to see the result; you can close this tab."
    } else {
        "This request does not match a Komaphone sign-in. Return to the app to try again."
    };
    let status = if accepted { "200 OK" } else { "400 Bad Request" };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nPragma: no-cache\r\nContent-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\nReferrer-Policy: no-referrer\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(query: &str) -> Vec<u8> {
        format!("GET /?{query} HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n").into_bytes()
    }

    #[test]
    fn callback_requires_unique_state_code_host_and_no_body() {
        assert!(matches!(parse_request(&request("state=expected&code=abc%2F123"), 1234, "expected").unwrap(), CallbackResult::Code(code) if code == "abc/123"));
        assert!(matches!(parse_request(&request("state=expected&error=access_denied"), 1234, "expected").unwrap(), CallbackResult::Denied));
        for query in [
            "state=wrong&code=secret",
            "code=secret",
            "state=expected&state=expected&code=x",
            "state=expected&code=a&code=b",
            "state=expected&code=a&error=e",
            "state=expected&code=a%00",
        ] {
            let error = parse_request(&request(query), 1234, "expected").err().unwrap().to_string();
            assert!(!error.contains("secret"));
        }
        for bad in [
            "GET /?state=expected&code=x HTTP/1.1\r\nHost: attacker.test\r\n\r\n",
            "GET /?state=expected&code=x HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nHost: 127.0.0.1:1234\r\n\r\n",
            "POST /?state=expected&code=x HTTP/1.1\r\nHost: 127.0.0.1:1234\r\n\r\n",
            "GET /?state=expected&code=x HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nContent-Length: 1\r\n\r\nx",
            "GET /?state=expected&code=x HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nTransfer-Encoding: chunked\r\n\r\n",
        ] {
            assert!(parse_request(bad.as_bytes(), 1234, "expected").is_err());
        }
        assert!(parse_request(&vec![b'a'; MAX_CALLBACK_BYTES + 1], 1234, "expected").is_err());
    }

    #[test]
    fn pkce_matches_rfc7636_vector_and_requests_only_sign_in() {
        let proof = Proof { verifier: "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk".into(), state: "state-value".into() };
        let url = proof.authorization_url(super::super::GOOGLE_CLIENT_ID, "nonce", "http://127.0.0.1:1234/").unwrap();
        let pairs: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(pairs.get("code_challenge").unwrap(), "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM");
        assert_eq!(pairs.get("code_challenge_method").unwrap(), "S256");
        assert_eq!(pairs.get("scope").unwrap(), "openid email profile");
        assert_eq!(pairs.get("access_type").unwrap(), "online");
        assert!(!url.as_str().contains(&proof.verifier));
        let first = Proof::new().unwrap();
        let second = Proof::new().unwrap();
        assert_eq!(first.verifier.len(), 43);
        assert_ne!(first.verifier, second.verifier);
        assert_ne!(first.state, first.verifier);
    }

    #[test]
    fn callback_cancel_and_deadline_release_listener() {
        let callback = Callback::bind().unwrap();
        let port = callback.port;
        assert!(callback.receive("state", &AtomicBool::new(true), Instant::now() + Duration::from_secs(1)).is_err());
        assert!(callback.receive("state", &AtomicBool::new(false), Instant::now()).is_err());
        drop(callback);
        assert!(TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok());
    }

    #[test]
    fn callback_ignores_wrong_state_then_accepts_once_without_reflection() {
        let callback = Callback::bind().unwrap();
        let port = callback.port;
        let sender = std::thread::spawn(move || {
            for state in ["wrong", "expected"] {
                let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
                stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                write!(stream, "GET /?state={state}&code=private-code HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n\r\n").unwrap();
                let mut response = String::new();
                stream.read_to_string(&mut response).unwrap();
                assert!(!response.contains("private-code"));
                assert!(!response.contains(state));
                assert!(response.contains("Cache-Control: no-store"));
                if state == "expected" {
                    assert!(response.contains("Sign-in is not complete yet"));
                    assert!(response.contains("Return to Komaphone to see the result"));
                }
            }
        });
        assert_eq!(callback.receive("expected", &AtomicBool::new(false), Instant::now() + Duration::from_secs(5)).unwrap(), "private-code");
        sender.join().unwrap();
    }
}
