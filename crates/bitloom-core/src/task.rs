use std::borrow::Cow;
use std::error::Error;
use std::future::Future;
use std::path::PathBuf;

use crate::{Dependencies, OutputFingerprint, TaskCache, TaskContext, TaskKind};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum EffectKind {
    #[default]
    Pure,
    Reversible,
    Irreversible,
}

/// A statically typed unit of work.
pub trait Task: Send + Sync + 'static {
    type Output: Send + Sync + 'static;
    type Error: Error + Send + Sync + 'static;
    type Dependencies: Dependencies;

    const KIND: TaskKind;
    const CACHE_SCHEMA: u32 = 0;

    fn label(&self) -> Cow<'static, str>;

    fn dependencies(&self) -> &Self::Dependencies;

    fn effect(&self) -> EffectKind {
        EffectKind::Pure
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn cache(&self) -> Option<&dyn TaskCache<Self::Output>> {
        None
    }

    fn output_fingerprint(&self, _output: &Self::Output) -> Option<OutputFingerprint> {
        None
    }

    /// Formats a task error for the executor's type-erased control plane.
    ///
    /// Most tasks only need the regular [`Error::to_string`] representation.
    /// Tasks for source languages may override this to preserve rich,
    /// source-aware diagnostics.
    fn format_error(&self, error: &Self::Error) -> String {
        error.to_string()
    }

    fn run(
        &self,
        context: TaskContext,
        inputs: <Self::Dependencies as Dependencies>::Resolved,
    ) -> impl Future<Output = Result<Self::Output, Self::Error>> + Send;
}
