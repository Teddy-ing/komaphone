#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
use clap::Parser;
use std::{net::SocketAddr, path::PathBuf};

#[derive(Parser)]
#[command(name = "photocraft-sync", about = "Komaphone private durable room service")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:43127")]
    bind: SocketAddr,
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long, default_value_t = 2048)]
    quota_mib: u64,
}
#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("Komaphone service: {error}");
        std::process::exit(1);
    }
}
async fn run() -> photocraft_sync::Result<()> {
    let args = Args::parse();
    let quota = args.quota_mib.checked_mul(1024 * 1024).ok_or(photocraft_sync::Error::Invalid("quota"))?;
    if !args.bind.ip().is_loopback() {
        return Err(photocraft_sync::Error::Invalid("non-loopback requires TLS; this local build fails closed"));
    }
    std::fs::create_dir_all(&args.data_dir)?;
    let service = photocraft_sync::server::spawn(
        args.bind,
        args.data_dir.join("service.sqlite3"),
        photocraft_sync::StoreOptions { quota_bytes: quota, ..Default::default() },
    )
    .await?;
    println!("{}", serde_json::json!({"ready":true,"address":service.address.to_string(),"protocol_version":1}));
    tokio::signal::ctrl_c().await?;
    service.shutdown().await
}
