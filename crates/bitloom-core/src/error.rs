use std::fmt;
use std::sync::Arc;

use thiserror::Error;

use crate::{TaskInstanceId, TaskKind};

pub type SharedTaskResult<T> = Result<Arc<T>, Arc<TaskFailure>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FailureReason {
    Task(String),
    DependencyFailed { dependency: TaskInstanceId },
    Cancelled,
    Panic(String),
    Cache(String),
    Skipped(String),
}

impl fmt::Display for FailureReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Task(message) => formatter.write_str(message),
            Self::DependencyFailed { dependency } => {
                write!(formatter, "dependency task {dependency} failed")
            }
            Self::Cancelled => formatter.write_str("task was cancelled"),
            Self::Panic(message) => write!(formatter, "task panicked: {message}"),
            Self::Cache(message) => write!(formatter, "cache operation failed: {message}"),
            Self::Skipped(message) => write!(formatter, "task was skipped: {message}"),
        }
    }
}

#[derive(Debug, Clone, Error)]
#[error("task {task_id} ({kind}) failed: {reason}")]
pub struct TaskFailure {
    pub task_id: TaskInstanceId,
    pub kind: TaskKind,
    pub reason: FailureReason,
}

impl TaskFailure {
    pub(crate) fn new(task_id: TaskInstanceId, kind: TaskKind, reason: FailureReason) -> Self {
        Self { task_id, kind, reason }
    }

    pub(crate) fn dependency(
        task_id: TaskInstanceId,
        kind: TaskKind,
        dependency: TaskInstanceId,
    ) -> Self {
        Self::new(task_id, kind, FailureReason::DependencyFailed { dependency })
    }

    pub(crate) fn cancelled(task_id: TaskInstanceId, kind: TaskKind) -> Self {
        Self::new(task_id, kind, FailureReason::Cancelled)
    }
}
