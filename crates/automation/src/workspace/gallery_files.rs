//! Gallery transfers through held directory capabilities. Engine codecs see only a
//! private staging directory; no request-selected path reaches ambient engine I/O.

use super::{AuthorizedWorkspace, RootCapability, request_path};
use cap_std::fs::{Dir, Metadata, OpenOptions};
use photocraft_engine::gallery::{GalleryAction, GalleryFileAccess, PreparedGalleryFiles};
use photocraft_engine::{EngineError, Result};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

// A bounded automation transfer, independent of the native archive format's larger
// ceiling. Shared assets may appear in multiple exported drafts, so even stores
// within their quotas can exceed this cap; refusal preserves all original data.
const MAX_TRANSFER_BYTES: u64 = 4 * 1024 * 1024 * 1024 + 64 * 1024 * 1024;
const MAX_BACKUP_FILES: usize = 1026; // 1024 drafts, gallery archive, index.
const MAX_INDEX_BYTES: u64 = 128 * 1024;
const MAX_PATH_COMPONENTS: usize = 128;

impl AuthorizedWorkspace {
    /// A non-serializable authority for the gallery's scoped control methods.
    /// It grants exactly the roots already supplied by trusted launch configuration.
    pub fn gallery_files(&self) -> Arc<dyn GalleryFileAccess> {
        Arc::new(self.clone())
    }
}

impl GalleryFileAccess for AuthorizedWorkspace {
    fn prepare(&self, action: &GalleryAction) -> Result<Box<dyn PreparedGalleryFiles>> {
        let mut output = None;
        let mut source = None;
        let rewritten = match action {
            GalleryAction::Export { format, path, .. } => {
                let kind = match format.as_str() {
                    "archive" | "png" | "gif" => OutputKind::File,
                    "sequence" => OutputKind::Directory(DirectoryKind::Sequence),
                    _ => return Err(error("Choose archive, png, gif, or sequence")),
                };
                output = Some(Output::new(self.write.as_ref(), path, kind)?);
                action.clone()
            }
            GalleryAction::Import { path } => {
                source = Some(Input::File(open_input(self.read.as_ref(), path)?));
                action.clone()
            }
            GalleryAction::Backup { path } => {
                output = Some(Output::new(self.write.as_ref(), path, OutputKind::Directory(DirectoryKind::Backup))?);
                action.clone()
            }
            GalleryAction::Restore { path, destination } => {
                output = Some(Output::new(self.write.as_ref(), destination, OutputKind::Directory(DirectoryKind::Restored))?);
                source = Some(Input::Directory(open_input_dir(self.read.as_ref(), path)?));
                action.clone()
            }
            _ => return Err(error("Only gallery export, import, backup, and restore accept scoped files")),
        };
        let staging = tempfile::Builder::new().prefix("komaphone-scoped-gallery-").tempdir().map_err(error)?;
        let input = staging.path().join("input");
        let out = staging.path().join("output");
        if let Some(source) = source {
            match source {
                Input::File(mut file) => {
                    let mut target = std::fs::OpenOptions::new().create_new(true).write(true).open(&input).map_err(error)?;
                    let mut remaining = MAX_TRANSFER_BYTES;
                    let observed = checked_file(&file.metadata().map_err(error)?, remaining)?;
                    copy_checked(&mut file, &mut target, observed, &mut remaining)?;
                    target.sync_all().map_err(error)?;
                }
                Input::Directory(dir) => {
                    std::fs::create_dir(&input).map_err(error)?;
                    let target = Dir::open_ambient_dir(&input, cap_std::ambient_authority()).map_err(error)?;
                    copy_directory(&dir, &target, DirectoryKind::Backup)?;
                }
            }
        }
        let rewritten = match rewritten {
            GalleryAction::Export { format, chain, frame, fps, looping, .. } => GalleryAction::Export { format, path: utf8(&out)?, chain, frame, fps, looping },
            GalleryAction::Import { .. } => GalleryAction::Import { path: utf8(&input)? },
            GalleryAction::Backup { .. } => GalleryAction::Backup { path: utf8(&out)? },
            GalleryAction::Restore { .. } => GalleryAction::Restore { path: utf8(&input)?, destination: utf8(&out)? },
            _ => return Err(error("Unsupported scoped gallery action")),
        };
        Ok(Box::new(Prepared { staging, action: rewritten, output, finished: false }))
    }
}

enum Input {
    File(cap_std::fs::File),
    Directory(Dir),
}

struct Prepared {
    staging: tempfile::TempDir,
    action: GalleryAction,
    output: Option<Output>,
    finished: bool,
}

impl PreparedGalleryFiles for Prepared {
    fn action(&self) -> GalleryAction {
        self.action.clone()
    }

    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Err(error("This gallery transfer has already finished"));
        }
        self.finished = true;
        if let Some(output) = self.output.take() {
            output.publish(&self.staging.path().join("output"))?;
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
pub(super) enum DirectoryKind {
    Sequence,
    Backup,
    Restored,
    WorldSequence,
    WorldArchive,
}

#[derive(Clone, Copy)]
pub(super) enum OutputKind {
    File,
    WorldFile,
    Directory(DirectoryKind),
}

pub(super) struct Output {
    parent: Dir,
    // Windows directory handles omit FILE_SHARE_DELETE. Retain every ancestor
    // while publishing through the handle's resolved name, not only the leaf.
    _ancestors: Vec<Dir>,
    leaf: PathBuf,
    kind: OutputKind,
}

impl Output {
    pub(super) fn new(root: Option<&RootCapability>, request: &str, kind: OutputKind) -> Result<Self> {
        if matches!(kind, OutputKind::Directory(_)) && !cfg!(windows) {
            return Err(error("Scoped gallery directory transfers currently require Windows"));
        }
        let (parent, leaf, ancestors) = parent_and_leaf(root, request, "write")?;
        require_absent(&parent, &leaf)?;
        Ok(Self { parent, _ancestors: ancestors, leaf, kind })
    }

    pub(super) fn publish(self, source: &Path) -> Result<()> {
        require_absent(&self.parent, &self.leaf)?;
        match self.kind {
            OutputKind::File => self.publish_file(source, MAX_TRANSFER_BYTES),
            OutputKind::WorldFile => self.publish_file(source, 512 * 1024 * 1024),
            OutputKind::Directory(kind) => self.publish_directory(source, kind),
        }
    }

    fn publish_file(self, source: &Path, limit: u64) -> Result<()> {
        let metadata = std::fs::symlink_metadata(source).map_err(error)?;
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(error("Gallery output is not a regular file"));
        }
        let mut input = std::fs::File::open(source).map_err(error)?;
        let observed = input.metadata().map_err(error)?.len();
        let temporary = random_name()?;
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        let mut target = self.parent.open_with(&temporary, &options).map_err(error)?;
        let mut cleanup = FileCleanup { dir: &self.parent, name: &temporary, armed: true };
        let mut remaining = limit;
        copy_checked(&mut input, &mut target, observed, &mut remaining)?;
        target.sync_all().map_err(error)?;
        drop(target);
        // Hard-link publication never replaces any existing leaf, including a link.
        self.parent.hard_link(&temporary, &self.parent, &self.leaf).map_err(error)?;
        self.parent.remove_file(&temporary).map_err(|e| error(format!("Export exists, but temporary-file cleanup failed: {e}")))?;
        cleanup.armed = false;
        Ok(())
    }

    fn publish_directory(self, source: &Path, kind: DirectoryKind) -> Result<()> {
        // Use an explicit no-replace Windows move. std/cap-std rename can replace
        // an existing empty directory, so a preflight existence check is not enough.
        if !cfg!(windows) {
            return Err(error("Scoped gallery directory transfers currently require Windows"));
        }
        let input = Dir::open_ambient_dir(source, cap_std::ambient_authority()).map_err(error)?;
        let temporary = random_name()?;
        self.parent.create_dir(&temporary).map_err(error)?;
        let mut cleanup = DirectoryCleanup { parent: &self.parent, name: &temporary, armed: true };
        {
            let target = self.parent.open_dir(&temporary).map_err(error)?;
            copy_directory(&input, &target, kind)?;
        }
        require_absent(&self.parent, &self.leaf)?;
        rename_directory_noclobber(&self.parent, Path::new(&temporary), &self.leaf)?;
        cleanup.armed = false;
        Ok(())
    }
}

/// Every untrusted component is resolved relative to a held capability. The
/// root's trusted launch spelling may itself be an alias; descendants may not.
fn parent_and_leaf(root: Option<&RootCapability>, request: &str, authority: &str) -> Result<(Dir, PathBuf, Vec<Dir>)> {
    let relative = request_path(request, root).map_err(error)?;
    if relative.components().count() > MAX_PATH_COMPONENTS {
        return Err(error("Scoped gallery path exceeds its 128-component depth limit"));
    }
    let root = root.ok_or_else(|| error(format!("automation filesystem access is not granted: {authority} authority is absent")))?;
    let mut current = root.dir.try_clone().map_err(error)?;
    let mut ancestors = Vec::new();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        let Component::Normal(name) = component else { return Err(error("Invalid scoped gallery path component")) };
        if components.peek().is_none() {
            return Ok((current, PathBuf::from(name), ancestors));
        }
        let metadata = current.symlink_metadata(name).map_err(error)?;
        if !metadata.is_dir() || is_link(&metadata) {
            return Err(error("Scoped gallery paths cannot cross directory links or reparse points"));
        }
        let next = current.open_dir(name).map_err(error)?;
        ancestors.push(current);
        current = next;
    }
    Err(error("Scoped gallery path must name a file or new directory"))
}

fn open_input(root: Option<&RootCapability>, request: &str) -> Result<cap_std::fs::File> {
    let (parent, leaf, _ancestors) = parent_and_leaf(root, request, "read")?;
    checked_file(&parent.symlink_metadata(&leaf).map_err(error)?, MAX_TRANSFER_BYTES)?;
    let file = parent.open(&leaf).map_err(error)?;
    checked_file(&file.metadata().map_err(error)?, MAX_TRANSFER_BYTES)?;
    Ok(file)
}

pub(super) fn open_input_dir(root: Option<&RootCapability>, request: &str) -> Result<Dir> {
    let (parent, leaf, _ancestors) = parent_and_leaf(root, request, "read")?;
    let metadata = parent.symlink_metadata(&leaf).map_err(error)?;
    if !metadata.is_dir() || is_link(&metadata) {
        return Err(error("Gallery backup input must be a regular directory, not a link"));
    }
    parent.open_dir(&leaf).map_err(error)
}

fn checked_file(metadata: &Metadata, maximum: u64) -> Result<u64> {
    if !metadata.is_file() || is_link(metadata) {
        return Err(error("Scoped gallery transfer requires regular files, not links or devices"));
    }
    if metadata.len() > maximum {
        return Err(error("Scoped gallery transfer exceeds its 4 GiB plus 64 MiB limit"));
    }
    Ok(metadata.len())
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use cap_std::fs::MetadataExt;
        // FILE_ATTRIBUTE_REPARSE_POINT also covers junctions, not only symlinks.
        metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn require_absent(dir: &Dir, leaf: &Path) -> Result<()> {
    match dir.symlink_metadata(leaf) {
        Ok(_) => Err(error("Destination already exists; choose a new file or directory")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(error(e)),
    }
}

/// Atomic publication under the held parent capability. The caller keeps all
/// ancestor capabilities alive, pinning the name obtained from that handle.
/// Explicitly omit REPLACE_EXISTING and COPY_ALLOWED: neither a final collision
/// nor a cross-volume copy may turn this into a replacing or partial operation.
fn rename_directory_noclobber(parent: &Dir, source: &Path, destination: &Path) -> Result<()> {
    for path in [source, destination] {
        let text = path.to_str().ok_or_else(|| error("Gallery publication name is not UTF-8"))?;
        if text.contains('\0') {
            return Err(error("Gallery publication names cannot contain NUL"));
        }
        let relative = super::relative_path(text).map_err(error)?;
        if relative.components().count() != 1 {
            return Err(error("Gallery publication names must be single components"));
        }
    }
    let metadata = parent.symlink_metadata(source).map_err(error)?;
    if !metadata.is_dir() || is_link(&metadata) {
        return Err(error("Gallery publication source must be an owned regular directory"));
    }
    #[cfg(windows)]
    {
        let held = parent.try_clone().map_err(error)?.into_std_file();
        let path = winx::file::get_file_path(&held).map_err(error)?;
        if !path.is_absolute() {
            return Err(error("Gallery publication parent handle has no absolute path"));
        }
        let from = utf8(&path.join(source))?;
        let to = utf8(&path.join(destination))?;
        winsafe::MoveFileEx(&from, Some(&to), winsafe::co::MOVEFILE::WRITE_THROUGH).map_err(error)
    }
    #[cfg(not(windows))]
    {
        Err(error("Scoped gallery directory transfers currently require Windows"))
    }
}

pub(super) fn copy_directory(source: &Dir, destination: &Dir, kind: DirectoryKind) -> Result<()> {
    let mut names = Vec::new();
    let maximum = match kind {
        DirectoryKind::Sequence => 36,
        DirectoryKind::Backup => MAX_BACKUP_FILES,
        DirectoryKind::Restored => 2,
        DirectoryKind::WorldSequence => 1801,
        DirectoryKind::WorldArchive => 4097,
    };
    for entry in source.entries().map_err(error)? {
        let entry = entry.map_err(error)?;
        let name = entry.file_name().into_string().map_err(|_| error("Gallery transfer filename is not UTF-8"))?;
        if names.len() >= maximum || !allowed_name(&name, kind) {
            return Err(error("Gallery directory contains an unsupported file or too many files"));
        }
        names.push(name);
    }
    names.sort();
    if names.is_empty()
        || matches!(kind, DirectoryKind::Backup) && (!names.iter().any(|n| n == "backup.json") || !names.iter().any(|n| n == "gallery.komabackup"))
        || matches!(kind, DirectoryKind::Restored) && names != ["drafts.sqlite3", "gallery.sqlite3"]
        || matches!(kind, DirectoryKind::WorldSequence) && !names.iter().any(|name| name == "tour.json")
        || matches!(kind, DirectoryKind::WorldArchive) && !names.iter().any(|name| name == "world.json")
    {
        return Err(error("Gallery transfer is missing required files"));
    }
    let mut remaining = if matches!(kind, DirectoryKind::WorldSequence | DirectoryKind::WorldArchive) { 512 * 1024 * 1024 } else { MAX_TRANSFER_BYTES };
    for name in names {
        let limit = if name == "backup.json" {
            MAX_INDEX_BYTES.min(remaining)
        } else if matches!(name.as_str(), "world.json" | "tour.json") {
            (32 * 1024 * 1024).min(remaining)
        } else {
            remaining
        };
        checked_file(&source.symlink_metadata(&name).map_err(error)?, limit)?;
        let mut input = source.open(&name).map_err(error)?;
        let observed = checked_file(&input.metadata().map_err(error)?, limit)?;
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        let mut output = destination.open_with(&name, &options).map_err(error)?;
        copy_checked(&mut input, &mut output, observed, &mut remaining)?;
        output.sync_all().map_err(error)?;
    }
    Ok(())
}

fn allowed_name(name: &str, kind: DirectoryKind) -> bool {
    match kind {
        DirectoryKind::Backup => {
            matches!(name, "backup.json" | "gallery.komabackup")
                || name.strip_suffix(".komarchive").is_some_and(|id| id.len() == 32 && id.bytes().all(|b| b.is_ascii_hexdigit()))
        }
        DirectoryKind::Sequence => {
            name.strip_prefix("frame-").and_then(|n| n.strip_suffix(".png")).is_some_and(|n| n.len() == 4 && n.bytes().all(|b| b.is_ascii_digit()))
        }
        DirectoryKind::Restored => matches!(name, "drafts.sqlite3" | "gallery.sqlite3"),
        DirectoryKind::WorldSequence => name == "tour.json" || allowed_name(name, DirectoryKind::Sequence),
        DirectoryKind::WorldArchive => {
            name == "world.json"
                || name
                    .strip_suffix(".png")
                    .is_some_and(|hash| hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        }
    }
}

fn copy_checked(input: &mut impl Read, output: &mut impl Write, observed: u64, remaining: &mut u64) -> Result<()> {
    if observed > *remaining {
        return Err(error("Scoped gallery transfer exceeds its 4 GiB plus 64 MiB limit"));
    }
    let mut copied = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let ask = usize::try_from(observed.saturating_sub(copied).saturating_add(1)).unwrap_or(buffer.len()).min(buffer.len());
        let read = match input.read(&mut buffer[..ask]) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            result => result.map_err(error)?,
        };
        if read == 0 {
            break;
        }
        copied = copied.checked_add(read as u64).ok_or_else(|| error("Gallery transfer length overflow"))?;
        if copied > observed {
            return Err(error("Gallery input grew during transfer; original preserved"));
        }
        output.write_all(&buffer[..read]).map_err(error)?;
    }
    if copied != observed {
        return Err(error("Gallery input changed length during transfer; original preserved"));
    }
    *remaining = remaining.saturating_sub(copied);
    Ok(())
}

fn random_name() -> Result<String> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).map_err(error)?;
    let mut name = String::from(".komaphone-scoped-");
    for byte in bytes {
        use std::fmt::Write as _;
        write!(name, "{byte:02x}").map_err(error)?;
    }
    Ok(name)
}

struct FileCleanup<'a> {
    dir: &'a Dir,
    name: &'a str,
    armed: bool,
}
impl Drop for FileCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.dir.remove_file(self.name);
        }
    }
}

struct DirectoryCleanup<'a> {
    parent: &'a Dir,
    name: &'a str,
    armed: bool,
}
impl Drop for DirectoryCleanup<'_> {
    fn drop(&mut self) {
        if self.armed {
            // Capability-scoped recursive removal touches only our random staging
            // name. cap-std never follows a directory link outside its authority.
            let _ = self.parent.remove_dir_all(self.name);
        }
    }
}

fn utf8(path: &Path) -> Result<String> {
    path.to_str().map(str::to_owned).ok_or_else(|| error("Temporary gallery path is not UTF-8"))
}

fn error(message: impl std::fmt::Display) -> EngineError {
    EngineError::Other(message.to_string())
}

#[cfg(test)]
mod tests;
