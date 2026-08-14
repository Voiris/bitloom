use std::io::ErrorKind;
use std::path::PathBuf;

use bitloom_core::{BoxFuture, RollbackAction, RollbackContext, RollbackError};

pub(super) struct RemoveFile(PathBuf);

impl RemoveFile {
    pub(super) fn new(path: impl Into<PathBuf>) -> Self {
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
