use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use bitloom_core::{BoxFuture, OutputFingerprint, RollbackAction, RollbackContext, RollbackError};

use crate::ToolError;

pub(crate) async fn prepare_parent(path: &Path) -> Result<(), ToolError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|source| ToolError::Prepare { path: parent.to_owned(), source })?;
    }
    Ok(())
}

pub(crate) async fn ensure_output(tool: &'static str, path: &Path) -> Result<(), ToolError> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(|source| ToolError::Read { path: path.to_owned(), source })?
    {
        Ok(())
    } else {
        Err(ToolError::MissingOutput { tool, path: path.to_owned() })
    }
}

pub(crate) async fn fingerprint_file(path: &Path) -> Result<OutputFingerprint, ToolError> {
    let bytes = tokio::fs::read(path)
        .await
        .map_err(|source| ToolError::Read { path: path.to_owned(), source })?;
    Ok(OutputFingerprint::new(*blake3::hash(&bytes).as_bytes()))
}

pub(crate) struct RemoveFile(PathBuf);

impl RemoveFile {
    pub(crate) fn new(path: impl Into<PathBuf>) -> Self {
        Self(path.into())
    }
}

impl RollbackAction for RemoveFile {
    fn rollback(
        self: Box<Self>,
        _context: RollbackContext,
    ) -> BoxFuture<'static, Result<(), RollbackError>> {
        Box::pin(async move {
            match tokio::fs::remove_file(&self.0).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
                Err(error) => Err(RollbackError::new(format!(
                    "failed to remove {}: {error}",
                    self.0.display()
                ))),
            }
        })
    }
}
