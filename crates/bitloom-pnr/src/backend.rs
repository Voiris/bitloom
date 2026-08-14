use std::error::Error;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use bitloom_core::{Cancellation, EffectKind, OutputFingerprint, RollbackAction, TaskContext};

use crate::{PnrCandidate, PnrRun};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PnrFailureKind {
    #[default]
    Retryable,
    Fatal,
}

#[derive(Clone)]
pub struct PnrRunContext {
    pub(crate) task: TaskContext,
    pub(crate) run_index: usize,
    pub(crate) total_runs: usize,
}

impl PnrRunContext {
    pub fn run_index(&self) -> usize {
        self.run_index
    }

    pub fn total_runs(&self) -> usize {
        self.total_runs
    }

    pub fn cancellation(&self) -> Cancellation {
        self.task.cancellation()
    }

    pub fn is_cancelled(&self) -> bool {
        self.task.is_cancelled()
    }

    pub fn defer(&self, action: impl RollbackAction) {
        self.task.defer(action);
    }
}

/// Backend for one candidate run. The task remains generic over the backend,
/// keeping its input and artifact types statically checked.
pub trait PnrBackend<Input>: Send + Sync + 'static {
    type Artifact: Send + Sync + 'static;
    type Error: Error + Send + Sync + 'static;

    fn prepare(
        &self,
        _context: TaskContext,
        _input: Arc<Input>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async { Ok(()) }
    }

    fn run(
        &self,
        context: PnrRunContext,
        input: Arc<Input>,
        seed: u64,
    ) -> impl Future<Output = Result<PnrRun<Self::Artifact>, Self::Error>> + Send;

    fn failure_kind(&self, _error: &Self::Error) -> PnrFailureKind {
        PnrFailureKind::Retryable
    }

    fn format_error(&self, error: &Self::Error) -> String {
        error.to_string()
    }

    fn effect(&self) -> EffectKind {
        EffectKind::Reversible
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        Vec::new()
    }

    fn output_fingerprint(
        &self,
        _candidate: &PnrCandidate<Self::Artifact>,
    ) -> Option<OutputFingerprint> {
        None
    }
}
