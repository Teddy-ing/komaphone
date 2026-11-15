//! Verified, no-clobber publication of a SQLite snapshot on the destination volume.
use crate::{Error, Result};
use rusqlite::Connection;
use std::path::{Path, PathBuf};

struct Staging(PathBuf);
impl Drop for Staging {
    fn drop(&mut self) {
        // This is our cryptographically named create_new file, never a caller's path.
        let _ = std::fs::remove_file(&self.0);
    }
}

pub(crate) fn copy(db: &Connection, destination: &Path) -> Result<()> {
    if destination.try_exists()? {
        return Err(Error::Invalid("backup/restore destination already exists; choose a new path"));
    }
    crate::store::verify_database(db)?;
    let parent = destination.parent().filter(|path| !path.as_os_str().is_empty()).unwrap_or(Path::new("."));
    let mut random = [0u8; 16];
    getrandom::fill(&mut random).map_err(|_| Error::Storage)?;
    let tag: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
    let stage = parent.join(format!(".komaphone-backup-{tag}.tmp"));
    let reserved = std::fs::OpenOptions::new().write(true).create_new(true).open(&stage)?;
    let stage = Staging(stage);
    drop(reserved);
    db.backup("main", &stage.0, None::<fn(rusqlite::backup::Progress)>)?;
    let verification = Connection::open_with_flags(&stage.0, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    crate::store::verify_database(&verification)?;
    drop(verification);
    std::fs::OpenOptions::new().read(true).write(true).open(&stage.0)?.sync_all()?;
    // Same-directory hard-link creation atomically refuses an existing destination;
    // unlike rename it cannot silently replace another file on Unix. NTFS supports it.
    std::fs::hard_link(&stage.0, destination)?;
    Ok(())
}
