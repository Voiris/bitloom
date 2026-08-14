use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use bitloom_artifacts::Artifact;
use bitloom_core::{OutputFingerprint, Task, TaskContext, TaskKind};
use bitloom_pack::{PackOutput, PackedBitstream};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ExistingBitstreamError {
    #[error("failed to read built bitstream {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
}

/// Imports a previously built bitstream into a flash-only graph.
pub struct ExistingBitstreamTask {
    path: PathBuf,
    label: Arc<str>,
}

impl ExistingBitstreamTask {
    pub fn new(name: impl AsRef<str>, path: impl Into<PathBuf>) -> Self {
        Self { path: path.into(), label: Arc::from(format!("Load bitstream ({})", name.as_ref())) }
    }
}

impl Task for ExistingBitstreamTask {
    type Output = PackOutput;
    type Error = ExistingBitstreamError;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("bitloom.flash.existing-bitstream");

    fn label(&self) -> Cow<'static, str> {
        Cow::Owned(self.label.to_string())
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &()
    }

    fn output_fingerprint(&self, output: &Self::Output) -> Option<OutputFingerprint> {
        Some(output.fingerprint())
    }

    async fn run(&self, _context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
        let bytes = tokio::fs::read(&self.path)
            .await
            .map_err(|source| ExistingBitstreamError::Read { path: self.path.clone(), source })?;
        let fingerprint = OutputFingerprint::new(*blake3::hash(&bytes).as_bytes());
        Ok(PackOutput {
            bitstream: Artifact::<PackedBitstream>::new(self.path.clone(), fingerprint),
        })
    }
}
