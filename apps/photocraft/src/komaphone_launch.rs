//! Process-scoped defaults supplied by the private LAN test launcher.

/// This convenience default does not connect, change trust, or authorize a public route.
/// Other supported endpoints remain enterable explicitly in the lobby.
pub fn private_endpoint(value: &str) -> Result<String, String> {
    let origin = value.strip_suffix('/').unwrap_or(value);
    let address = origin.strip_prefix("https://").ok_or("KOMAPHONE_SERVICE_ENDPOINT must be a private HTTPS IPv4 address with a port")?;
    let (host, port) = address.split_once(':').ok_or("KOMAPHONE_SERVICE_ENDPOINT needs an explicit port")?;
    let ip: std::net::Ipv4Addr = host.parse().map_err(|_| "KOMAPHONE_SERVICE_ENDPOINT needs a private IPv4 address")?;
    let port: u16 = port.parse().map_err(|_| "KOMAPHONE_SERVICE_ENDPOINT has an invalid port")?;
    if !ip.is_private() || port == 0 {
        return Err("KOMAPHONE_SERVICE_ENDPOINT must use a private address and a nonzero port".into());
    }
    Ok(format!("https://{ip}:{port}"))
}

/// Keep the original process alive through a pre-window GPU fallback. Launchers
/// waiting on it must keep the profile locked while the replacement edits it.
pub fn wait_for_gpu_fallback(mut child: std::process::Child) -> std::io::Result<()> {
    let status = child.wait()?;
    if status.success() { Ok(()) } else { Err(std::io::Error::other(format!("CPU compatibility process exited with {status}"))) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gpu_fallback_child_fixture() {
        if let Ok(code) = std::env::var("KOMAPHONE_TEST_GPU_FALLBACK_EXIT") {
            std::thread::sleep(std::time::Duration::from_millis(80));
            std::process::exit(code.parse().unwrap());
        }
    }

    #[test]
    fn gpu_fallback_waits_for_child_and_propagates_failure() {
        let _spawns = crate::CHILD_SPAWNS.read().unwrap_or_else(std::sync::PoisonError::into_inner);
        for code in [0, 7] {
            let start = std::time::Instant::now();
            let child = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "komaphone_launch::tests::gpu_fallback_child_fixture", "--nocapture"])
                .env("KOMAPHONE_TEST_GPU_FALLBACK_EXIT", code.to_string())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap();
            assert_eq!(wait_for_gpu_fallback(child).is_ok(), code == 0);
            assert!(start.elapsed() >= std::time::Duration::from_millis(80));
        }
    }

    #[test]
    fn private_launcher_endpoint_accepts_lan_origins_only() {
        assert_eq!(private_endpoint("https://10.0.0.40:43127/").unwrap(), "https://10.0.0.40:43127");
        for input in [
            "http://10.0.0.40:43127",
            "https://8.8.8.8:443",
            "https://127.0.0.1:43127",
            "https://example.com:443",
            "https://user@10.0.0.40:43127",
            "https://10.0.0.40:43127/path",
            "https://10.0.0.40:43127?x=1",
            "https://10.0.0.40:43127#fragment",
            "https://10.0.0.40:0",
            "https://10.0.0.40:65536",
            "https://10.0.0.40",
        ] {
            assert!(private_endpoint(input).is_err(), "{input}");
        }
    }
}
