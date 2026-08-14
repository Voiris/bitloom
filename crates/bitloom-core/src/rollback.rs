use std::fmt;

use thiserror::Error;

use crate::{BoxFuture, TaskInstanceId};

#[derive(Debug, Clone, Default)]
pub struct RollbackContext {
    _private: (),
}

#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct RollbackError {
    message: String,
}

impl RollbackError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

pub trait RollbackAction: Send + 'static {
    fn rollback(
        self: Box<Self>,
        context: RollbackContext,
    ) -> BoxFuture<'static, Result<(), RollbackError>>;
}

pub(crate) struct RollbackJournal {
    actions: Vec<Box<dyn RollbackAction>>,
}

impl fmt::Debug for RollbackJournal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_struct("RollbackJournal").field("actions", &self.actions.len()).finish()
    }
}

impl RollbackJournal {
    pub(crate) fn new(actions: Vec<Box<dyn RollbackAction>>) -> Self {
        Self { actions }
    }

    pub const fn is_empty(&self) -> bool {
        self.actions.is_empty()
    }

    pub(crate) async fn rollback(mut self, context: RollbackContext) -> Vec<RollbackError> {
        let mut failures = Vec::new();
        while let Some(action) = self.actions.pop() {
            if let Err(error) = action.rollback(context.clone()).await {
                failures.push(error);
            }
        }
        failures
    }
}

#[derive(Debug, Clone)]
pub struct RollbackFailure {
    pub task_id: TaskInstanceId,
    pub errors: Vec<RollbackError>,
}

#[derive(Default)]
pub(crate) struct RollbackRecorder {
    actions: Vec<Box<dyn RollbackAction>>,
}

impl RollbackRecorder {
    pub(crate) fn push(&mut self, action: Box<dyn RollbackAction>) {
        self.actions.push(action);
    }

    pub(crate) fn into_journal(self) -> RollbackJournal {
        RollbackJournal::new(self.actions)
    }
}
