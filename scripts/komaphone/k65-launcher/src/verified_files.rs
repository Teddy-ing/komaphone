fn metadata(path: &Path) -> Result<fs::Metadata> {
    let value = fs::symlink_metadata(path).map_err(|e| format!("Inspect {path:?}: {e}"))?;
    if value.file_type().is_symlink() {
        return Err(format!("Symbolic links are forbidden: {path:?}"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if value.file_attributes() & 0x400 != 0 {
            return Err(format!("Reparse points are forbidden: {path:?}"));
        }
    }
    Ok(value)
}

fn check_ancestors(path: &Path) -> Result<()> {
    if path.ancestors().count() > 64 {
        return Err("Path ancestor depth exceeds limit".into());
    }
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(_) => {
                metadata(parent)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Inspect ancestor {parent:?}: {e}")),
        }
    }
    Ok(())
}

fn ensure_directory(path: &Path) -> Result<()> {
    check_ancestors(path)?;
    fs::create_dir_all(path).map_err(|e| format!("Create directory {path:?}: {e}"))?;
    if !metadata(path)?.is_dir() {
        return Err(format!("Not a directory: {path:?}"));
    }
    Ok(())
}

fn digest(path: &Path, limit: u64) -> Result<String> {
    let info = metadata(path)?;
    if !info.is_file() || info.len() > limit {
        return Err(format!("Not a bounded regular file: {path:?}"));
    }
    let mut file = File::open(path).map_err(|e| format!("Open {path:?}: {e}"))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 65536];
    let mut length = 0_u64;
    loop {
        let count = file.read(&mut buffer).map_err(|e| format!("Read {path:?}: {e}"))?;
        if count == 0 {
            break;
        }
        length = length.saturating_add(count as u64);
        if length > limit {
            return Err(format!("File grew beyond limit: {path:?}"));
        }
        hash.update(buffer.get(..count).ok_or("Invalid read count")?);
    }
    Ok(format!("{:X}", hash.finalize()))
}

fn require_hash(path: &Path, expected: &str, limit: u64) -> Result<()> {
    if digest(path, limit)? != expected {
        return Err(format!("SHA256 mismatch: {path:?}"));
    }
    Ok(())
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>> {
    let info = metadata(path)?;
    if !info.is_file() || info.len() > limit as u64 {
        return Err("Manifest is not a bounded regular file".into());
    }
    let mut bytes = Vec::new();
    File::open(path).map_err(|e| e.to_string())?.take((limit as u64).saturating_add(1)).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Manifest exceeds read limit".into());
    }
    Ok(bytes)
}

fn parse_manifest(text: &str, package: Package<'_>) -> Result<BTreeMap<String, String>> {
    let mut hashes = BTreeMap::new();
    for line in text.lines() {
        let (hash, path) = line.split_once("  ").ok_or("Invalid manifest line")?;
        if hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_lowercase()) || !FILES.contains(&path) || path == "SHA256SUMS" {
            return Err("Invalid manifest path/hash".into());
        }
        if hashes.insert(path.to_owned(), hash.to_owned()).is_some() {
            return Err("Duplicate manifest path".into());
        }
    }
    if hashes.len() != FILES.len().saturating_sub(1) {
        return Err("Incomplete manifest".into());
    }
    if hashes.get("photocraft.exe").map(String::as_str) != Some(package.app_sha) {
        return Err("Unexpected candidate binaries".into());
    }
    hashes.insert("SHA256SUMS".into(), package.manifest_sha.into());
    Ok(hashes)
}

fn inventory(root: &Path, directory: &Path, depth: u8, files: &mut BTreeSet<String>) -> Result<()> {
    if depth > 16 {
        return Err("Directory nesting exceeds limit".into());
    }
    if !metadata(directory)?.is_dir() {
        return Err("Inventory root is not a directory".into());
    }
    for entry in fs::read_dir(directory).map_err(|e| format!("List {directory:?}: {e}"))? {
        let path = entry.map_err(|e| e.to_string())?.path();
        let info = metadata(&path)?;
        if info.is_dir() {
            inventory(root, &path, depth + 1, files)?;
        } else if info.is_file() {
            let relative = path.strip_prefix(root).map_err(|e| e.to_string())?.to_str().ok_or("Non-Unicode candidate path")?.replace('\\', "/");
            if !FILES.contains(&relative.as_str()) || !files.insert(relative) {
                return Err("Unexpected candidate file".into());
            }
        } else {
            return Err("Non-regular candidate object".into());
        }
    }
    Ok(())
}

fn verify_candidate(root: &Path, package: Package<'_>) -> Result<BTreeMap<String, String>> {
    check_ancestors(root)?;
    let manifest_path = root.join("SHA256SUMS");
    let bytes = read_bounded(&manifest_path, 16384)?;
    if format!("{:X}", Sha256::digest(&bytes)) != package.manifest_sha {
        return Err("Candidate manifest SHA256 mismatch".into());
    }
    let text = std::str::from_utf8(&bytes).map_err(|e| e.to_string())?;
    let hashes = parse_manifest(text, package)?;
    let mut files = BTreeSet::new();
    inventory(root, root, 0, &mut files)?;
    if files.len() != FILES.len() {
        return Err("Candidate file set is incomplete".into());
    }
    for (path, hash) in &hashes {
        require_hash(&root.join(path), hash, MAX_FILE_BYTES)?;
    }
    Ok(hashes)
}

fn copy_verified(source: &Path, destination: &Path, staging_root: &Path, hash: &str, limit: u64) -> Result<()> {
    require_hash(source, hash, limit)?;
    let parent = destination.parent().ok_or("Missing destination parent")?;
    ensure_directory(parent)?;
    match fs::symlink_metadata(destination) {
        Ok(_) => return require_hash(destination, hash, limit),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(format!("Inspect copy destination: {e}")),
    }
    ensure_directory(staging_root)?;
    let (staging, mut output) = create_staging(staging_root)?;
    let input = File::open(source).map_err(|e| e.to_string())?;
    let copied = std::io::copy(&mut input.take(limit.saturating_add(1)), &mut output).map_err(|e| format!("Copy to staging {staging:?}: {e}"))?;
    if copied > limit {
        return Err(format!("Copy exceeded size limit; staging retained: {staging:?}"));
    }
    output.sync_all().map_err(|e| e.to_string())?;
    drop(output);
    require_hash(&staging, hash, limit)?;
    // An NTFS hard link publishes verified bytes atomically and never replaces a file.
    // Staging lives outside the exact candidate inventory so interrupted copies remain retryable.
    match fs::hard_link(&staging, destination) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => require_hash(destination, hash, limit)?,
        Err(e) => return Err(format!("Publish copy on local NTFS destination: {e}; staging retained: {staging:?}")),
    }
    fs::remove_file(&staging).map_err(|e| format!("Remove owned copy staging: {e}"))?;
    require_hash(destination, hash, limit)?;
    Ok(())
}

fn create_staging(parent: &Path) -> Result<(PathBuf, File)> {
    for _ in 0..32 {
        let sequence = COPY_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".komaphone-copy-{}-{sequence}.tmp", std::process::id()));
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("Create owned staging: {e}")),
        }
    }
    Err("Could not reserve a fresh copy staging file".into())
}

fn append_event(destination: &Path, value: serde_json::Value) -> Result<()> {
    ensure_directory(destination)?;
    let path = destination.join("launcher-log.txt");
    check_ancestors(&path)?;
    if path.exists() && metadata(&path)?.len() > 1024 * 1024 {
        return Err("Launcher log exceeds limit".into());
    }
    let mut file = OpenOptions::new().append(true).create(true).open(&path).map_err(|e| e.to_string())?;
    writeln!(file, "{value}").map_err(|e| e.to_string())?;
    file.sync_all().map_err(|e| e.to_string())
}

/// Keep one launcher/app pair per profile. The OS releases this exclusive handle
/// on a crash; an existing empty file is not a stale PID lock and can be reused.
fn lock_profile(destination: &Path) -> Result<File> {
    ensure_directory(destination)?;
    let path = destination.join("launcher.lock");
    check_ancestors(&path)?;
    if path.exists() && (!metadata(&path)?.is_file() || metadata(&path)?.len() != 0) {
        return Err("Unexpected profile lock file; no file was replaced".into());
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
    }
    options.open(&path).map_err(|e| format!("Cannot exclusively open this profile; close its existing Komaphone window before relaunching: {e}"))
}

