//! Managed gallery exports: basename-only validation, dedicated app-managed `Exports`
//! directory resolution, reparse-point containment, non-destructive collision numbering,
//! and atomic staging/publication.

use crate::{EngineError, Result};

/// Maximum validated export basename length (characters, excluding extension/suffix).
pub const MAX_EXPORT_STEM_CHARS: usize = 120;

const RECOGNIZED_EXTENSIONS: &[&str] = &[".komarchive", ".png", ".gif"];
const INVALID_WINDOWS_CHARS: &[char] = &['<', '>', '"', '|', '?', '*'];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Archive,
    Png,
    Gif,
    Sequence,
}

impl ExportFormat {
    pub fn from_str_name(value: &str) -> Result<Self> {
        match value {
            "archive" => Ok(Self::Archive),
            "png" => Ok(Self::Png),
            "gif" => Ok(Self::Gif),
            "sequence" => Ok(Self::Sequence),
            _ => Err(EngineError::Other("Choose archive, png, gif, or sequence".into())),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Archive => "archive",
            Self::Png => "png",
            Self::Gif => "gif",
            Self::Sequence => "sequence",
        }
    }

    pub const fn extension(self) -> Option<&'static str> {
        match self {
            Self::Archive => Some("komarchive"),
            Self::Png => Some("png"),
            Self::Gif => Some("gif"),
            Self::Sequence => None,
        }
    }

    pub const fn is_directory(self) -> bool {
        matches!(self, Self::Sequence)
    }
}

fn is_windows_reserved_device(head: &str) -> bool {
    let upper = head.to_ascii_uppercase();
    if matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$" | "CLOCK$") {
        return true;
    }
    if let Some(rest) = upper.strip_prefix("COM").or_else(|| upper.strip_prefix("LPT")) {
        let mut chars = rest.chars();
        if let (Some(ch), None) = (chars.next(), chars.next()) {
            return matches!(ch, '1'..='9' | '\u{00B9}' | '\u{00B2}' | '\u{00B3}');
        }
    }
    false
}

fn strip_recognized_extensions(mut stem: String) -> (String, bool) {
    let mut stripped = false;
    loop {
        let lower = stem.to_ascii_lowercase();
        let Some(ext_len) = RECOGNIZED_EXTENSIONS.iter().find_map(|ext| lower.ends_with(ext).then_some(ext.len())) else {
            break;
        };
        let cut = stem.len().saturating_sub(ext_len);
        if !stem.is_char_boundary(cut) {
            break;
        }
        stem.truncate(cut);
        stem = stem.trim_end_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
        stripped = true;
    }
    (stem, stripped)
}

/// Produce a safe, readable default export stem from a gallery item's title.
/// Guarantees `validate_export_basename(&sanitize_default_export_name(title)).is_ok()`.
pub fn sanitize_default_export_name(title: &str) -> String {
    let replaced: String =
        title.chars().map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { ' ' } else { c }).collect();
    let collapsed = replaced.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim().trim_end_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
    let (mut stem, _) = strip_recognized_extensions(trimmed);
    if stem.chars().count() > MAX_EXPORT_STEM_CHARS {
        stem = stem.chars().take(MAX_EXPORT_STEM_CHARS).collect();
        stem = stem.trim_end_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
    }
    if stem.is_empty() {
        return "komaphone-export".into();
    }
    let head = stem.split('.').next().unwrap_or(&stem).trim();
    if is_windows_reserved_device(head) {
        let suffix = "-export";
        if let Some((_, rest)) = stem.split_once('.') {
            let max_rest = MAX_EXPORT_STEM_CHARS.saturating_sub(head.chars().count() + suffix.len() + 1);
            let rest_trimmed: String = rest.chars().take(max_rest).collect();
            let rest_trimmed = rest_trimmed.trim_end_matches(|c: char| c == '.' || c.is_whitespace());
            stem = if rest_trimmed.is_empty() { format!("{head}{suffix}") } else { format!("{head}{suffix}.{rest_trimmed}") };
        } else {
            stem = format!("{head}{suffix}");
        }
    }
    stem
}

/// Validate a user-entered export basename, rejecting path traversal, directory/drive syntax,
/// invalid Windows characters, trailing dots/spaces, reserved device names, and empty names.
/// Strips recognized trailing export extensions (`.komarchive`, `.png`, `.gif`) while preserving
/// meaningful internal dots.
pub fn validate_export_basename(raw: &str) -> Result<String> {
    if raw.chars().any(char::is_control) {
        return Err(EngineError::Other("Export name cannot contain control characters.".into()));
    }
    if raw.contains('/') || raw.contains('\\') || raw.contains(':') {
        return Err(EngineError::Other("Export name must be a filename only, without folders or drive letters.".into()));
    }
    if raw.chars().any(|c| INVALID_WINDOWS_CHARS.contains(&c)) {
        return Err(EngineError::Other("Export name cannot contain < > : \" / \\ | ? *.".into()));
    }
    let trimmed = raw.trim().trim_end_matches(|c: char| c == '.' || c.is_whitespace()).to_owned();
    let (stem, stripped_ext) = strip_recognized_extensions(trimmed);
    if stem.is_empty() {
        return Err(EngineError::Other(if stripped_ext { "Enter a filename before the extension.".into() } else { "Enter an export name.".into() }));
    }
    if stem.chars().count() > MAX_EXPORT_STEM_CHARS {
        return Err(EngineError::Other(format!("Export name is too long (maximum {MAX_EXPORT_STEM_CHARS} characters).")));
    }
    let head = stem.split('.').next().unwrap_or(&stem).trim();
    if is_windows_reserved_device(head) {
        return Err(EngineError::Other("That name is reserved by Windows; choose a different export name.".into()));
    }
    Ok(stem)
}

/// Construct the single-component output filename or directory name for collision index `index` (1-based).
pub fn candidate_filename(stem: &str, format: ExportFormat, index: usize) -> String {
    let numbered = if index <= 1 { stem.to_owned() } else { format!("{stem} ({index})") };
    match format.extension() {
        Some(ext) => format!("{numbered}.{ext}"),
        None => numbered,
    }
}

#[cfg(not(target_arch = "wasm32"))]
mod native_fs {
    use super::{ExportFormat, Result, candidate_filename, validate_export_basename};
    use crate::EngineError;
    use std::path::{Component, Path, PathBuf};

    const MAX_COLLISION_INDEX: usize = 10_000;
    const MAX_ANCESTORS: usize = 64;

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct PublishedExport {
        pub path: PathBuf,
        pub display_path: String,
        pub is_directory: bool,
        pub format: ExportFormat,
    }

    /// Format a filesystem path cleanly for user-facing display by removing Windows verbatim prefixes.
    pub fn strip_windows_verbatim(path: &Path) -> String {
        let text = path.to_string_lossy();
        if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
            format!(r"\\{unc}")
        } else if let Some(rest) = text.strip_prefix(r"\\?\") {
            rest.to_owned()
        } else {
            text.into_owned()
        }
    }

    pub fn is_reparse_or_symlink(meta: &std::fs::Metadata) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            meta.file_type().is_symlink() || (meta.file_attributes() & 0x400) != 0
        }
        #[cfg(not(windows))]
        {
            meta.file_type().is_symlink()
        }
    }

    /// Resolve the dedicated per-user Exports folder from `data_dir` and optional override
    /// (`KOMAPHONE_EXPORTS_DIR`), completely independent of the current working directory.
    pub fn resolve_exports_dir(data_dir: &Path, explicit_exports_env: Option<&Path>) -> Result<PathBuf> {
        if let Some(explicit) = explicit_exports_env.filter(|p| !p.as_os_str().is_empty()) {
            let text = explicit.to_str().ok_or_else(|| EngineError::Other("Exports directory path is not valid UTF-8.".into()))?;
            if text.contains('\0') {
                return Err(EngineError::Other("Exports directory path cannot contain NUL.".into()));
            }
            if !explicit.is_absolute() {
                return Err(EngineError::Other("Exports directory must be an absolute path.".into()));
            }
            return Ok(explicit.to_path_buf());
        }
        let text = data_dir.to_str().ok_or_else(|| EngineError::Other("Application data directory path is not valid UTF-8.".into()))?;
        if text.contains('\0') {
            return Err(EngineError::Other("Application data directory path cannot contain NUL.".into()));
        }
        if !data_dir.is_absolute() {
            return Err(EngineError::Other("Application data directory must be an absolute path.".into()));
        }
        if data_dir.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.eq_ignore_ascii_case("data"))
            && let Some(parent) = data_dir.parent()
        {
            let lower = parent.join("exports");
            if std::fs::symlink_metadata(&lower).is_ok() {
                return Ok(lower);
            }
            return Ok(parent.join("Exports"));
        }
        Ok(data_dir.join("Exports"))
    }

    fn reject_if_unsafe_dir_entry(path: &Path) -> Result<()> {
        match std::fs::symlink_metadata(path) {
            Ok(meta) => {
                if is_reparse_or_symlink(&meta) {
                    return Err(EngineError::Other("Exports folder cannot be or cross a symbolic link or junction.".into()));
                }
                if !meta.is_dir() {
                    return Err(EngineError::Other("Exports location exists as a file instead of a folder.".into()));
                }
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(EngineError::Other(format!("Cannot inspect Exports folder: {e}"))),
        }
    }

    /// Ensure `exports_root` exists as a real non-reparse directory and return its canonical path.
    pub fn ensure_safe_exports_dir(exports_root: &Path) -> Result<PathBuf> {
        let text = exports_root.to_str().ok_or_else(|| EngineError::Other("Exports folder path is not valid UTF-8.".into()))?;
        if text.contains('\0') {
            return Err(EngineError::Other("Exports folder path cannot contain NUL.".into()));
        }
        if !exports_root.is_absolute() {
            return Err(EngineError::Other("Exports folder must be an absolute path.".into()));
        }
        if exports_root.ancestors().count() > MAX_ANCESTORS {
            return Err(EngineError::Other("Exports folder path exceeds depth limit.".into()));
        }

        #[cfg(windows)]
        {
            for ancestor in exports_root.ancestors() {
                if ancestor.parent().is_some() {
                    reject_if_unsafe_dir_entry(ancestor)?;
                }
            }
        }
        #[cfg(not(windows))]
        {
            reject_if_unsafe_dir_entry(exports_root)?;
            if let Some(parent) = exports_root.parent().filter(|p| p.parent().is_some()) {
                reject_if_unsafe_dir_entry(parent)?;
            }
        }

        std::fs::create_dir_all(exports_root).map_err(|e| EngineError::Other(format!("Cannot create Exports folder: {e}")))?;

        let meta = std::fs::symlink_metadata(exports_root).map_err(|e| EngineError::Other(format!("Cannot inspect Exports folder: {e}")))?;
        if is_reparse_or_symlink(&meta) || !meta.is_dir() {
            return Err(EngineError::Other("Exports folder cannot be a symbolic link, junction, or file.".into()));
        }
        std::fs::canonicalize(exports_root).map_err(|e| EngineError::Other(format!("Cannot resolve Exports folder: {e}")))
    }

    fn verify_single_normal_component(filename: &str) -> Result<&str> {
        let mut components = Path::new(filename).components();
        match (components.next(), components.next()) {
            (Some(Component::Normal(_)), None) => Ok(filename),
            _ => Err(EngineError::Other("Export filename must be a single path component.".into())),
        }
    }

    /// Owns an isolated temporary staging folder inside the verified `Exports` directory.
    /// Dropping without calling [`Self::publish`] cleans only this staging folder.
    pub struct ManagedExportStage {
        exports_root: PathBuf,
        canonical_root: PathBuf,
        stage_dir: tempfile::TempDir,
        stem: String,
        format: ExportFormat,
    }

    impl ManagedExportStage {
        pub fn prepare(exports_root: &Path, raw_name: &str, format: ExportFormat) -> Result<Self> {
            let stem = validate_export_basename(raw_name)?;
            let canonical_root = ensure_safe_exports_dir(exports_root)?;
            let stage_dir = tempfile::Builder::new()
                .prefix(".komaphone-export-")
                .tempdir_in(&canonical_root)
                .map_err(|e| EngineError::Other(format!("Cannot create export staging folder: {e}")))?;
            Ok(Self { exports_root: exports_root.to_path_buf(), canonical_root, stage_dir, stem, format })
        }

        pub fn staged_output_path(&self) -> PathBuf {
            if self.format.is_directory() { self.stage_dir.path().join("frames") } else { self.stage_dir.path().join("payload") }
        }

        pub fn publish(self) -> Result<PublishedExport> {
            let current_root = ensure_safe_exports_dir(&self.exports_root)?;
            if current_root != self.canonical_root {
                return Err(EngineError::Other("Exports folder changed during export; original data preserved.".into()));
            }
            let staged = self.staged_output_path();
            for index in 1..=MAX_COLLISION_INDEX {
                let filename = candidate_filename(&self.stem, self.format, index);
                verify_single_normal_component(&filename)?;
                let dest = self.canonical_root.join(&filename);
                match std::fs::symlink_metadata(&dest) {
                    Ok(_) => continue,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(e) => return Err(EngineError::Other(format!("Cannot check export destination: {e}"))),
                }
                if self.format.is_directory() {
                    match publish_directory_noclobber(&staged, &dest) {
                        PublishOutcome::Published => {
                            let meta =
                                std::fs::symlink_metadata(&dest).map_err(|e| EngineError::Other(format!("Cannot verify published export folder: {e}")))?;
                            if !meta.is_dir() || is_reparse_or_symlink(&meta) {
                                return Err(EngineError::Other("Published export folder failed containment check.".into()));
                            }
                            let display_path = strip_windows_verbatim(&dest);
                            return Ok(PublishedExport { path: dest, display_path, is_directory: true, format: self.format });
                        }
                        PublishOutcome::Collision => continue,
                        PublishOutcome::Failed(err) => return Err(err),
                    }
                } else {
                    match publish_file_noclobber(&staged, &dest) {
                        PublishOutcome::Published => {
                            let meta = std::fs::symlink_metadata(&dest).map_err(|e| EngineError::Other(format!("Cannot verify published export file: {e}")))?;
                            if !meta.is_file() || is_reparse_or_symlink(&meta) {
                                return Err(EngineError::Other("Published export file failed containment check.".into()));
                            }
                            let display_path = strip_windows_verbatim(&dest);
                            return Ok(PublishedExport { path: dest, display_path, is_directory: false, format: self.format });
                        }
                        PublishOutcome::Collision => continue,
                        PublishOutcome::Failed(err) => return Err(err),
                    }
                }
            }
            Err(EngineError::Other("Too many existing exports share this name; choose a different export name.".into()))
        }
    }

    enum PublishOutcome {
        Published,
        Collision,
        Failed(EngineError),
    }

    fn is_already_exists_os_error(err: &std::io::Error) -> bool {
        err.kind() == std::io::ErrorKind::AlreadyExists || matches!(err.raw_os_error(), Some(80 | 183))
    }

    fn publish_file_noclobber(staged: &Path, dest: &Path) -> PublishOutcome {
        match std::fs::hard_link(staged, dest) {
            Ok(()) => PublishOutcome::Published,
            Err(e) if is_already_exists_os_error(&e) => PublishOutcome::Collision,
            Err(e) => {
                #[cfg(windows)]
                {
                    if e.kind() == std::io::ErrorKind::Unsupported || matches!(e.raw_os_error(), Some(1 | 50)) {
                        return move_noclobber_windows(staged, dest);
                    }
                }
                PublishOutcome::Failed(EngineError::Other(format!("Could not save export to Exports folder: {e}")))
            }
        }
    }

    fn publish_directory_noclobber(staged: &Path, dest: &Path) -> PublishOutcome {
        #[cfg(windows)]
        {
            move_noclobber_windows(staged, dest)
        }
        #[cfg(not(windows))]
        {
            match std::fs::rename(staged, dest) {
                Ok(()) => PublishOutcome::Published,
                Err(e) if matches!(e.kind(), std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty) => PublishOutcome::Collision,
                Err(e) => PublishOutcome::Failed(EngineError::Other(format!("Could not save export folder: {e}"))),
            }
        }
    }

    #[cfg(windows)]
    fn move_noclobber_windows(staged: &Path, dest: &Path) -> PublishOutcome {
        let Some(src) = staged.to_str() else {
            return PublishOutcome::Failed(EngineError::Other("Staged export path is not valid text.".into()));
        };
        let Some(dst) = dest.to_str() else {
            return PublishOutcome::Failed(EngineError::Other("Destination export path is not valid text.".into()));
        };
        if src.contains('\0') || dst.contains('\0') {
            return PublishOutcome::Failed(EngineError::Other("Export path cannot contain NUL.".into()));
        }
        match winsafe::MoveFileEx(src, Some(dst), winsafe::co::MOVEFILE::WRITE_THROUGH) {
            Ok(()) => PublishOutcome::Published,
            Err(winsafe::co::ERROR::ALREADY_EXISTS | winsafe::co::ERROR::FILE_EXISTS) => PublishOutcome::Collision,
            Err(err) => PublishOutcome::Failed(EngineError::Other(format!("Could not publish export: {err}"))),
        }
    }

    /// Verify that `target` is a real non-reparse entry directly inside `exports_root` before
    /// launching Explorer / Finder / file manager to reveal or open it.
    pub fn validate_reveal_target(exports_root: &Path, target: &Path, is_directory: bool) -> Result<PathBuf> {
        let canonical_root = ensure_safe_exports_dir(exports_root)?;
        let text = target.to_str().ok_or_else(|| EngineError::Other("Export target path is not valid UTF-8.".into()))?;
        if text.contains('\0') {
            return Err(EngineError::Other("Export target path cannot contain NUL.".into()));
        }
        let meta = std::fs::symlink_metadata(target).map_err(|_| EngineError::Other("Exported file or folder no longer exists.".into()))?;
        if is_reparse_or_symlink(&meta) {
            return Err(EngineError::Other("Cannot open a symbolic link or junction in Exports.".into()));
        }
        if is_directory && !meta.is_dir() {
            return Err(EngineError::Other("Exported target is not a folder.".into()));
        }
        if !is_directory && !meta.is_file() {
            return Err(EngineError::Other("Exported target is not a regular file.".into()));
        }
        let canonical_target = std::fs::canonicalize(target).map_err(|e| EngineError::Other(format!("Cannot resolve exported path: {e}")))?;
        let Some(parent) = canonical_target.parent() else {
            return Err(EngineError::Other("Export target has no parent folder.".into()));
        };
        if parent != canonical_root {
            return Err(EngineError::Other("Refused to open a path outside the Exports folder.".into()));
        }
        Ok(canonical_target)
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub use native_fs::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn basename_validation_and_extension_normalization() {
        assert_eq!(validate_export_basename("Sunset").unwrap(), "Sunset");
        assert_eq!(validate_export_basename("  Sunset.png  ").unwrap(), "Sunset");
        assert_eq!(validate_export_basename("Sunset.PNG").unwrap(), "Sunset");
        assert_eq!(validate_export_basename("Sunset.gif").unwrap(), "Sunset");
        assert_eq!(validate_export_basename("Sunset.komarchive").unwrap(), "Sunset");
        assert_eq!(validate_export_basename("v1.2.final.png").unwrap(), "v1.2.final");
        assert_eq!(validate_export_basename("v1.2.final.png.gif").unwrap(), "v1.2.final");
        assert_eq!(validate_export_basename("my.png.study").unwrap(), "my.png.study");
        assert_eq!(validate_export_basename("sketch...  ").unwrap(), "sketch");
        assert_eq!(validate_export_basename("sketch.png. ").unwrap(), "sketch");

        for bad in [
            "",
            "   ",
            ".",
            "..",
            "...",
            ".png",
            ".GIF",
            ".komarchive",
            "a/b",
            "a\\b",
            "../escape",
            "sub/file.png",
            r"C:\temp\a",
            "C:a",
            r"\\server\share\a",
            r"\\?\C:\a",
            "foo:ads",
            "a<b",
            "a>b",
            "a\"b",
            "a|b",
            "a?b",
            "a*b",
            "CON",
            "con.png",
            "PRN",
            "AUX",
            "NUL.gif",
            "COM1",
            "com9.komarchive",
            "LPT1",
            "COM\u{00B9}",
            "LPT\u{00B3}",
            "bad\0name",
            "bad\nname",
        ] {
            assert!(validate_export_basename(bad).is_err(), "expected rejection for {bad:?}");
        }

        let too_long = "a".repeat(MAX_EXPORT_STEM_CHARS + 1);
        assert!(validate_export_basename(&too_long).is_err());
        let exact_max = "a".repeat(MAX_EXPORT_STEM_CHARS);
        assert_eq!(validate_export_basename(&exact_max).unwrap().chars().count(), MAX_EXPORT_STEM_CHARS);
    }

    #[test]
    fn sanitize_default_name_always_produces_valid_basename() {
        for raw in ["", "   ", "...", "CON", "con.png", "Cat / Dog?", "a:b\\c*d|e", "v1.2.final.png", &"x".repeat(200), "LPT\u{00B2}.txt"] {
            let safe = sanitize_default_export_name(raw);
            assert!(validate_export_basename(&safe).is_ok(), "sanitized {raw:?} -> {safe:?} must validate");
        }
        assert_eq!(sanitize_default_export_name("Cat / Dog?"), "Cat Dog");
        assert_eq!(sanitize_default_export_name("Bouncing Lantern.gif"), "Bouncing Lantern");
        assert_eq!(sanitize_default_export_name("CON"), "CON-export");
    }

    #[test]
    fn candidate_filename_formats_collisions_predictably() {
        assert_eq!(candidate_filename("art", ExportFormat::Png, 1), "art.png");
        assert_eq!(candidate_filename("art", ExportFormat::Png, 2), "art (2).png");
        assert_eq!(candidate_filename("art", ExportFormat::Archive, 3), "art (3).komarchive");
        assert_eq!(candidate_filename("anim", ExportFormat::Gif, 1), "anim.gif");
        assert_eq!(candidate_filename("frames", ExportFormat::Sequence, 1), "frames");
        assert_eq!(candidate_filename("frames", ExportFormat::Sequence, 2), "frames (2)");
    }

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn resolve_exports_dir_isolates_sibling_exports_and_rejects_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        std::fs::create_dir(&data).unwrap();
        assert_eq!(resolve_exports_dir(&data, None).unwrap(), dir.path().join("Exports"));
        std::fs::create_dir(dir.path().join("exports")).unwrap();
        assert_eq!(resolve_exports_dir(&data, None).unwrap(), dir.path().join("exports"));
        assert_eq!(resolve_exports_dir(dir.path(), None).unwrap(), dir.path().join("Exports"));
        let explicit = dir.path().join("CustomExports");
        assert_eq!(resolve_exports_dir(&data, Some(&explicit)).unwrap(), explicit);
        assert!(resolve_exports_dir(std::path::Path::new("relative/data"), None).is_err());
        assert!(resolve_exports_dir(&data, Some(std::path::Path::new("relative/exports"))).is_err());
    }
}
