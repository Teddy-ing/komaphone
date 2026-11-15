//! World exports use held write capabilities and private bounded staging.
use super::{
    AuthorizedWorkspace,
    gallery_files::{DirectoryKind, Output, OutputKind, copy_directory, open_input_dir},
};
use photocraft_engine::{
    EngineError, Result,
    world_assets::{PreparedWorldFiles, WorldExport, WorldFileAccess},
};
use std::sync::Arc;

impl AuthorizedWorkspace {
    pub fn world_files(&self) -> Arc<dyn WorldFileAccess> {
        Arc::new(self.clone())
    }
}
impl WorldFileAccess for AuthorizedWorkspace {
    fn prepare(&self, action: &WorldExport) -> Result<Box<dyn PreparedWorldFiles>> {
        let request = action.path().to_str().ok_or_else(|| EngineError::Other("World file path must be UTF-8".into()))?;
        if matches!(action, WorldExport::Restore { .. }) {
            let input = open_input_dir(self.read.as_ref(), request)?;
            let staging = tempfile::Builder::new().prefix("komaphone-scoped-world-input-").tempdir().map_err(|e| EngineError::Other(e.to_string()))?;
            let target = cap_std::fs::Dir::open_ambient_dir(staging.path(), cap_std::ambient_authority()).map_err(|e| EngineError::Other(e.to_string()))?;
            copy_directory(&input, &target, DirectoryKind::WorldArchive)?;
            let action = action.clone().with_path(staging.path().to_owned());
            return Ok(Box::new(Prepared { staging, action, output: None, finished: false }));
        }
        let kind = match action {
            WorldExport::View { .. } | WorldExport::TourGif { .. } => OutputKind::WorldFile,
            WorldExport::TourSequence { .. } => OutputKind::Directory(DirectoryKind::WorldSequence),
            WorldExport::Archive { .. } => OutputKind::Directory(DirectoryKind::WorldArchive),
            WorldExport::Restore { .. } => return Err(EngineError::Other("Invalid archive input route".into())),
        };
        let output = Output::new(self.write.as_ref(), request, kind)?;
        let staging = tempfile::Builder::new().prefix("komaphone-scoped-world-").tempdir().map_err(|e| EngineError::Other(e.to_string()))?;
        let action = action.clone().with_path(staging.path().join("output"));
        Ok(Box::new(Prepared { staging, action, output: Some(output), finished: false }))
    }
}
struct Prepared {
    staging: tempfile::TempDir,
    action: WorldExport,
    output: Option<Output>,
    finished: bool,
}
impl PreparedWorldFiles for Prepared {
    fn action(&self) -> WorldExport {
        self.action.clone()
    }
    fn finish(&mut self) -> Result<()> {
        if self.finished {
            return Err(EngineError::Other("This world transfer has already finished".into()));
        }
        self.finished = true;
        if let Some(output) = self.output.take() {
            output.publish(&self.staging.path().join("output"))?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
