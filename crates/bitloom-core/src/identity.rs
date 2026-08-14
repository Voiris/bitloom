use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_TASK_ID: AtomicU64 = AtomicU64::new(1);

/// Stable, user-defined name of a task implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskKind(&'static str);

impl TaskKind {
    pub const fn new(value: &'static str) -> Self {
        Self(value)
    }

    pub const fn as_str(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for TaskKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

/// Identity of one task node in one in-memory graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TaskInstanceId(u64);

impl TaskInstanceId {
    pub(crate) fn next() -> Self {
        Self(NEXT_TASK_ID.fetch_add(1, Ordering::Relaxed))
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for TaskInstanceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}
