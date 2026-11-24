#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]
use clap::Parser;
use std::{
    io::Read,
    net::SocketAddr,
    path::{Path, PathBuf},
};

#[derive(Parser)]
#[command(name = "photocraft-sync", about = "Komaphone private durable room service")]
struct Args {
    #[arg(long, default_value = "127.0.0.1:43127")]
    bind: SocketAddr,
    #[arg(long)]
    data_dir: PathBuf,
    #[arg(long, default_value_t = 2048)]
    quota_mib: u64,
    /// Operational invited-member capacity per canvas, independent of party rooms.
    #[arg(long, default_value_t = 4096)]
    canvas_member_limit: u32,
    /// Operational concurrent ephemeral connections across this service.
    #[arg(long, default_value_t = 256)]
    canvas_presence_limit: u32,
    /// Certificate chain PEM for native HTTPS. Required for non-loopback binds.
    #[arg(long, requires = "tls_key", conflicts_with_all = ["backup_to", "restore_from"])]
    tls_cert: Option<PathBuf>,
    /// Private key PEM. Keep this file private; distribute only the CA certificate.
    #[arg(long, requires = "tls_cert")]
    tls_key: Option<PathBuf>,
    /// Make a consistent verified backup into a new file, without starting a listener.
    #[arg(long, conflicts_with = "restore_from")]
    backup_to: Option<PathBuf>,
    /// Restore a verified backup into a fresh data directory; never overwrite live data.
    #[arg(long)]
    restore_from: Option<PathBuf>,
    /// Enable hosted Google sign-in behind the loopback HTTPS proxy.
    #[arg(long, conflicts_with_all = ["legacy_private_network", "tls_cert", "tls_key"])]
    google_client_id: Option<String>,
    /// Explicitly allow the old private-network pairing protocol (never public hosting).
    #[arg(long)]
    legacy_private_network: bool,
    /// Administrator: mint a single-use admission without starting the service.
    #[arg(long, conflicts_with_all = ["backup_to", "restore_from", "revoke_profile", "admit_profile", "invalidate_restored_access"])]
    create_admission: bool,
    #[arg(long, default_value_t = 168)]
    admission_hours: u64,
    /// Administrator: revoke a stable profile and all its sessions/devices.
    #[arg(long, conflicts_with_all = ["backup_to", "restore_from", "admit_profile", "invalidate_restored_access"])]
    revoke_profile: Option<String>,
    /// Administrator: readmit a profile after checking the live revocation record.
    #[arg(long, conflicts_with_all = ["backup_to", "restore_from", "invalidate_restored_access"])]
    admit_profile: Option<String>,
    /// Administrator: invalidate sessions and invitations in an isolated restore.
    #[arg(long, conflicts_with_all = ["backup_to", "restore_from"])]
    invalidate_restored_access: bool,
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
    if let Some(destination) = args.backup_to {
        photocraft_sync::Store::copy_verified(args.data_dir.join("service.sqlite3"), destination)?;
        println!("{}", serde_json::json!({"backup_verified":true}));
        return Ok(());
    }
    if let Some(source) = args.restore_from {
        if args.data_dir.try_exists()? && std::fs::read_dir(&args.data_dir)?.next().transpose()?.is_some() {
            return Err(photocraft_sync::Error::Invalid("restore requires a fresh empty data directory; keep existing files and choose another directory"));
        }
        std::fs::create_dir_all(&args.data_dir)?;
        photocraft_sync::Store::copy_verified(source, args.data_dir.join("service.sqlite3"))?;
        let now = u64::try_from(chrono::Utc::now().timestamp_millis()).map_err(|_| photocraft_sync::Error::Invalid("service clock"))?;
        let quota = args.quota_mib.checked_mul(1024 * 1024).ok_or(photocraft_sync::Error::Invalid("quota"))?;
        let mut store = photocraft_sync::Store::open(
            args.data_dir.join("service.sqlite3"),
            photocraft_sync::StoreOptions { quota_bytes: quota, ..Default::default() },
            now,
        )?;
        store.identity_invalidate_restored_access()?;
        println!("{}", serde_json::json!({"restore_verified":true,"all_profiles_quarantined":true,"profile_revocation_reconciliation_required":true}));
        return Ok(());
    }
    let quota = args.quota_mib.checked_mul(1024 * 1024).ok_or(photocraft_sync::Error::Invalid("quota"))?;
    if args.create_admission || args.revoke_profile.is_some() || args.admit_profile.is_some() || args.invalidate_restored_access {
        std::fs::create_dir_all(&args.data_dir)?;
        let _authority_lock = authority_lock(&args.data_dir)?;
        let now = u64::try_from(chrono::Utc::now().timestamp_millis()).map_err(|_| photocraft_sync::Error::Invalid("service clock"))?;
        let mut store = photocraft_sync::Store::open(
            args.data_dir.join("service.sqlite3"),
            photocraft_sync::StoreOptions { quota_bytes: quota, ..Default::default() },
            now,
        )?;
        if args.create_admission {
            let lifetime = args.admission_hours.checked_mul(60 * 60 * 1000).ok_or(photocraft_sync::Error::Invalid("admission lifetime"))?;
            let admission = store.identity_create_admission(lifetime, now)?;
            // This is the explicit administrator secret output, never a service log.
            println!("{}", serde_json::json!({"admission":admission,"expires_at_ms":now.saturating_add(lifetime)}));
        } else if let Some(profile) = args.revoke_profile {
            store.identity_revoke_profile(photocraft_party::ParticipantId::new(&profile)?)?;
            println!("{}", serde_json::json!({"profile_revoked":true}));
        } else if let Some(profile) = args.admit_profile {
            store.identity_admit_profile(photocraft_party::ParticipantId::new(&profile)?)?;
            println!("{}", serde_json::json!({"profile_readmitted":true}));
        } else {
            store.identity_invalidate_restored_access()?;
            println!(
                "{}",
                serde_json::json!({"restored_sessions_and_invitations_invalidated":true,"all_profiles_quarantined":true,"profile_revocation_reconciliation_required":true})
            );
        }
        return Ok(());
    }
    if args.google_client_id.is_none() && !args.legacy_private_network && !args.bind.ip().is_loopback() {
        return Err(photocraft_sync::Error::Invalid(
            "select --google-client-id for hosted use, or explicitly --legacy-private-network for isolated LAN development",
        ));
    }
    let tls = match (args.tls_cert, args.tls_key) {
        (Some(cert), Some(key)) => Some(photocraft_sync::tls::TlsConfig::from_pem(&pem(&cert)?, &pem(&key)?)?),
        (None, None) if args.bind.ip().is_loopback() => None,
        _ => return Err(photocraft_sync::Error::Invalid("non-loopback requires --tls-cert and --tls-key")),
    };
    std::fs::create_dir_all(&args.data_dir)?;
    let _authority_lock = authority_lock(&args.data_dir)?;
    let encrypted = tls.is_some();
    let database = args.data_dir.join("service.sqlite3");
    let options = photocraft_sync::StoreOptions {
        quota_bytes: quota,
        world_limits: photocraft_sync::worlds::Limits {
            members_per_canvas: args.canvas_member_limit,
            presence_connections: args.canvas_presence_limit,
            ..Default::default()
        },
        ..Default::default()
    };
    let hosted = args.google_client_id.is_some();
    let service = if let Some(client_id) = args.google_client_id {
        let google = photocraft_sync::identity::google::GoogleVerifier::new(&client_id)?;
        photocraft_sync::server::spawn_hosted(args.bind, database, options, google).await?
    } else {
        match tls {
            Some(tls) => photocraft_sync::server::spawn_tls(args.bind, database, options, tls).await?,
            None => photocraft_sync::server::spawn(args.bind, database, options).await?,
        }
    };
    println!(
        "{}",
        serde_json::json!({"ready":true,"address":service.address.to_string(),"protocol_version":photocraft_party::protocol::PROTOCOL_VERSION,"tls":encrypted,"hosted":hosted})
    );
    tokio::signal::ctrl_c().await?;
    service.shutdown().await
}

fn pem(path: &Path) -> photocraft_sync::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.take(photocraft_sync::tls::MAX_PEM_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > photocraft_sync::tls::MAX_PEM_BYTES {
        return Err(photocraft_sync::Error::Invalid("TLS certificate/key exceeds 64 KiB"));
    }
    Ok(bytes)
}

fn authority_lock(directory: &Path) -> photocraft_sync::Result<std::fs::File> {
    let lock = std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(directory.join("service.lock"))?;
    lock.try_lock()
        .map_err(|_| photocraft_sync::Error::Invalid("service authority is already running; stop it before local administration and restart afterwards"))?;
    Ok(lock)
}
