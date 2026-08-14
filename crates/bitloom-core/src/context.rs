use std::sync::{Arc, Mutex};

use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::rollback::RollbackRecorder;
use crate::state::TaskStateCell;
use crate::{CacheMode, RollbackAction, TaskInstanceId, TaskProgress};

#[derive(Debug, Clone)]
pub struct Cancellation {
    token: CancellationToken,
}

impl Cancellation {
    pub fn is_cancelled(&self) -> bool {
        self.token.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.token.cancelled().await;
    }
}

#[derive(Debug, Clone)]
pub struct RunContext {
    cancellation: CancellationToken,
    cache_mode: CacheMode,
}

impl Default for RunContext {
    fn default() -> Self {
        Self::new(CacheMode::BestEffort)
    }
}

impl RunContext {
    pub fn new(cache_mode: CacheMode) -> Self {
        Self { cancellation: CancellationToken::new(), cache_mode }
    }

    pub fn cache_mode(&self) -> CacheMode {
        self.cache_mode
    }

    pub fn cancellation(&self) -> Cancellation {
        Cancellation { token: self.cancellation.clone() }
    }

    pub fn cancel(&self) {
        self.cancellation.cancel();
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancellation.is_cancelled()
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum ProgressError {
    #[error("progress total must be greater than zero")]
    ZeroTotal,
    #[error("completed progress {completed} exceeds total {total}")]
    ExceedsTotal { completed: u64, total: u64 },
}

#[derive(Clone)]
pub struct TaskContext {
    task_id: TaskInstanceId,
    state: TaskStateCell,
    rollback: Arc<Mutex<RollbackRecorder>>,
    run: Arc<RunContext>,
}

impl TaskContext {
    pub(crate) fn new(
        task_id: TaskInstanceId,
        state: TaskStateCell,
        rollback: Arc<Mutex<RollbackRecorder>>,
        run: Arc<RunContext>,
    ) -> Self {
        Self { task_id, state, rollback, run }
    }

    pub fn task_id(&self) -> TaskInstanceId {
        self.task_id
    }

    pub fn cancellation(&self) -> Cancellation {
        self.run.cancellation()
    }

    pub fn is_cancelled(&self) -> bool {
        self.run.is_cancelled()
    }

    pub async fn cancelled(&self) {
        self.run.cancellation.cancelled().await;
    }

    pub fn set_progress(&self, completed: u64, total: u64) -> Result<(), ProgressError> {
        if total == 0 {
            return Err(ProgressError::ZeroTotal);
        }
        if completed > total {
            return Err(ProgressError::ExceedsTotal { completed, total });
        }

        self.state.set_progress(TaskProgress { completed, total });
        Ok(())
    }

    /// Publishes a non-fatal warning for live reporters and the final task
    /// snapshot.
    pub fn warning(&self, message: impl Into<Arc<str>>) {
        self.state.push_warning(message.into());
    }

    /// Registers cleanup for the current, unfinished task attempt.
    ///
    /// Actions run in reverse registration order if this task fails or panics.
    /// They are discarded as soon as the task completes successfully; a later
    /// node failure never rolls back this task's committed result.
    pub fn defer(&self, action: impl RollbackAction) {
        self.rollback
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(Box::new(action));
    }
}
