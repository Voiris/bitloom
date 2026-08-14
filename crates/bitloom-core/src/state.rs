use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use tokio::sync::watch;

use crate::{TaskFailure, TaskInstanceId, TaskKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPhase {
    Waiting,
    Ready,
    Running,
    Cached,
    Finished,
    Failed,
    Cancelled,
    Skipped,
    RollingBack,
}

impl TaskPhase {
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Cached | Self::Finished | Self::Failed | Self::Cancelled | Self::Skipped
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskProgress {
    pub completed: u64,
    pub total: u64,
}

#[derive(Debug, Clone)]
pub struct TaskSnapshot {
    pub id: TaskInstanceId,
    pub kind: TaskKind,
    pub label: Arc<str>,
    pub phase: TaskPhase,
    pub progress: Option<TaskProgress>,
    pub elapsed: Option<Duration>,
    pub warnings: Vec<Arc<str>>,
    pub failure: Option<Arc<TaskFailure>>,
}

#[derive(Debug)]
struct StateInner {
    phase: TaskPhase,
    progress: Option<TaskProgress>,
    started_at: Option<Instant>,
    finished_at: Option<Instant>,
    warnings: Vec<Arc<str>>,
    failure: Option<Arc<TaskFailure>>,
}

#[derive(Debug, Clone)]
pub(crate) struct TaskStateCell {
    id: TaskInstanceId,
    kind: TaskKind,
    label: Arc<str>,
    inner: Arc<RwLock<StateInner>>,
    revision: watch::Sender<u64>,
}

impl TaskStateCell {
    pub(crate) fn new(id: TaskInstanceId, kind: TaskKind, label: Arc<str>) -> Self {
        let (revision, _) = watch::channel(0);
        Self {
            id,
            kind,
            label,
            inner: Arc::new(RwLock::new(StateInner {
                phase: TaskPhase::Waiting,
                progress: None,
                started_at: None,
                finished_at: None,
                warnings: Vec::new(),
                failure: None,
            })),
            revision,
        }
    }

    pub(crate) fn snapshot(&self) -> TaskSnapshot {
        let inner = self.inner.read().unwrap_or_else(|poisoned| poisoned.into_inner());
        let elapsed = inner.started_at.map(|started_at| {
            inner.finished_at.unwrap_or_else(Instant::now).saturating_duration_since(started_at)
        });

        TaskSnapshot {
            id: self.id,
            kind: self.kind,
            label: self.label.clone(),
            phase: inner.phase,
            progress: inner.progress,
            elapsed,
            warnings: inner.warnings.clone(),
            failure: inner.failure.clone(),
        }
    }

    pub(crate) fn mark_ready(&self) {
        self.update(|inner| {
            if inner.phase == TaskPhase::Waiting {
                inner.phase = TaskPhase::Ready;
            }
        });
    }

    pub(crate) fn mark_running(&self) {
        self.update(|inner| {
            inner.phase = TaskPhase::Running;
            inner.started_at.get_or_insert_with(Instant::now);
            inner.finished_at = None;
            inner.failure = None;
        });
    }

    pub(crate) fn set_progress(&self, progress: TaskProgress) {
        self.update(|inner| {
            if inner.phase == TaskPhase::Running {
                inner.progress = Some(progress);
            }
        });
    }

    pub(crate) fn push_warning(&self, warning: Arc<str>) {
        self.update(|inner| inner.warnings.push(warning));
    }

    pub(crate) fn mark_cached(&self) {
        self.mark_terminal(TaskPhase::Cached, None);
    }

    pub(crate) fn mark_finished(&self) {
        self.mark_terminal(TaskPhase::Finished, None);
    }

    pub(crate) fn mark_failed(&self, failure: Arc<TaskFailure>) {
        self.mark_terminal(TaskPhase::Failed, Some(failure));
    }

    pub(crate) fn mark_cancelled(&self, failure: Arc<TaskFailure>) {
        self.mark_terminal(TaskPhase::Cancelled, Some(failure));
    }

    pub(crate) fn mark_skipped(&self, failure: Arc<TaskFailure>) {
        self.mark_terminal(TaskPhase::Skipped, Some(failure));
    }

    pub(crate) fn mark_rolling_back(&self) {
        self.update(|inner| inner.phase = TaskPhase::RollingBack);
    }

    fn mark_terminal(&self, phase: TaskPhase, failure: Option<Arc<TaskFailure>>) {
        self.update(|inner| {
            inner.phase = phase;
            inner.finished_at.get_or_insert_with(Instant::now);
            inner.failure = failure;
        });
    }

    fn update(&self, update: impl FnOnce(&mut StateInner)) {
        {
            let mut inner = self.inner.write().unwrap_or_else(|poisoned| poisoned.into_inner());
            update(&mut inner);
        }
        self.revision.send_modify(|revision| *revision = revision.wrapping_add(1));
    }
}
