//! Explicit isolated-test cleanup. Never point this at a profile you intend to reconnect.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
#[cfg(not(target_arch = "wasm32"))]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--discard-isolated-test-credentials") {
        return Err("Required: --discard-isolated-test-credentials ABSOLUTE_DATABASE_PATH".into());
    }
    let path = std::path::PathBuf::from(args.next().ok_or("An explicit database path is required")?);
    if args.next().is_some() || !path.is_absolute() || !path.is_file() {
        return Err("Choose one existing absolute isolated-test database path".into());
    }
    let store = photocraft_room_client::Outbox::open(path, Box::new(photocraft_room_client::WindowsCredentials))?;
    let count = store.remove_credentials_for_discarded_root()?;
    println!("Removed {count} isolated credential references; database retained");
    Ok(())
}
#[cfg(target_arch = "wasm32")]
fn main() {}
