use std::fmt;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use tokio::sync::OnceCell;

use crate::cache::{CacheKeyInput, OutputFingerprint};
use crate::result::ResultSlot;
use crate::rollback::{RollbackJournal, RollbackRecorder};
use crate::state::TaskStateCell;
use crate::{
    BoxFuture, CacheMode, Dependencies, DependencyCollector, EffectKind, FailureReason,
    RollbackContext, RollbackFailure, RunContext, SharedTaskResult, Task, TaskContext, TaskFailure,
    TaskInstanceId, TaskKind, TaskPhase, TaskSnapshot,
};

struct TaskNode<T: Task> {
    id: TaskInstanceId,
    label: Arc<str>,
    task: T,
    result: ResultSlot<T::Output>,
    fingerprint: OnceCell<OutputFingerprint>,
    state: TaskStateCell,
    local_rollback_failures: Mutex<Vec<RollbackFailure>>,
}

impl<T: Task> TaskNode<T> {
    fn new(task: T) -> Self {
        let id = TaskInstanceId::next();
        let label = Arc::<str>::from(task.label().into_owned());
        Self {
            id,
            label: label.clone(),
            task,
            result: ResultSlot::default(),
            fingerprint: OnceCell::new(),
            state: TaskStateCell::new(id, T::KIND, label),
            local_rollback_failures: Mutex::new(Vec::new()),
        }
    }

    fn dependencies(&self) -> Vec<ErasedNodeRef> {
        let mut collector = DependencyCollector::new();
        self.task.dependencies().collect(&mut collector);
        collector.into_nodes()
    }

    async fn execute_inner(self: &Arc<Self>, run: Arc<RunContext>) -> NodeOutcome {
        let result = self
            .result
            .initialize(|| {
                let node = self.clone();
                async move { node.run_once(run).await }
            })
            .await;

        NodeOutcome {
            succeeded: result.is_ok(),
            cache_hit: self.state.snapshot().phase == TaskPhase::Cached,
            local_rollback: std::mem::take(
                &mut *self
                    .local_rollback_failures
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            ),
        }
    }

    async fn run_once(self: Arc<Self>, run: Arc<RunContext>) -> SharedTaskResult<T::Output> {
        self.state.mark_running();

        if run.is_cancelled() {
            let failure = Arc::new(TaskFailure::cancelled(self.id, T::KIND));
            self.state.mark_cancelled(failure.clone());
            return Err(failure);
        }

        let inputs = match self.task.dependencies().resolve().await {
            Ok(inputs) => inputs,
            Err(dependency_failure) => {
                let failure =
                    Arc::new(TaskFailure::dependency(self.id, T::KIND, dependency_failure.task_id));
                self.state.mark_skipped(failure.clone());
                return Err(failure);
            }
        };

        let dependencies = self.dependencies();
        let dependency_fingerprints =
            dependencies.iter().map(ErasedNodeRef::output_fingerprint).collect::<Option<Vec<_>>>();
        let mut cache_key = None;

        if run.cache_mode() != CacheMode::Disabled
            && let (Some(cache), Some(dependency_fingerprints)) =
                (self.task.cache(), dependency_fingerprints.as_deref())
        {
            match cache.key(CacheKeyInput {
                kind: T::KIND,
                schema: T::CACHE_SCHEMA,
                dependency_fingerprints,
            }) {
                Ok(key) => match cache.restore(&key).await {
                    Ok(Some(cached)) => {
                        let _ = self.fingerprint.set(cached.fingerprint);
                        self.state.mark_cached();
                        return Ok(Arc::new(cached.output));
                    }
                    Ok(None) => cache_key = Some(key),
                    Err(error) if run.cache_mode() == CacheMode::Strict => {
                        let failure = Arc::new(TaskFailure::new(
                            self.id,
                            T::KIND,
                            FailureReason::Cache(error.to_string()),
                        ));
                        self.state.mark_failed(failure.clone());
                        return Err(failure);
                    }
                    Err(_) => {}
                },
                Err(error) if run.cache_mode() == CacheMode::Strict => {
                    let failure = Arc::new(TaskFailure::new(
                        self.id,
                        T::KIND,
                        FailureReason::Cache(error.to_string()),
                    ));
                    self.state.mark_failed(failure.clone());
                    return Err(failure);
                }
                Err(_) => {}
            }
        }

        let recorder = Arc::new(Mutex::new(RollbackRecorder::default()));
        let context = TaskContext::new(self.id, self.state.clone(), recorder.clone(), run.clone());
        let result =
            std::panic::AssertUnwindSafe(self.task.run(context, inputs)).catch_unwind().await;

        let output = match result {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => {
                let failure = Arc::new(if run.is_cancelled() {
                    TaskFailure::cancelled(self.id, T::KIND)
                } else {
                    TaskFailure::new(
                        self.id,
                        T::KIND,
                        FailureReason::Task(self.task.format_error(&error)),
                    )
                });
                self.rollback_local(recorder).await;
                if matches!(&failure.reason, FailureReason::Cancelled) {
                    self.state.mark_cancelled(failure.clone());
                } else {
                    self.state.mark_failed(failure.clone());
                }
                return Err(failure);
            }
            Err(_) => {
                let failure = Arc::new(TaskFailure::new(
                    self.id,
                    T::KIND,
                    FailureReason::Panic("panic payload suppressed".to_owned()),
                ));
                self.rollback_local(recorder).await;
                self.state.mark_failed(failure.clone());
                return Err(failure);
            }
        };

        let mut fingerprint = self.task.output_fingerprint(&output);
        if let (Some(cache), Some(key)) = (self.task.cache(), cache_key.as_ref()) {
            match cache.store(key, &output).await {
                Ok(stored_fingerprint) => fingerprint = Some(stored_fingerprint),
                Err(error) if run.cache_mode() == CacheMode::Strict => {
                    let failure = Arc::new(TaskFailure::new(
                        self.id,
                        T::KIND,
                        FailureReason::Cache(error.to_string()),
                    ));
                    self.rollback_local(recorder).await;
                    self.state.mark_failed(failure.clone());
                    return Err(failure);
                }
                Err(_) => {}
            }
        }

        if let Some(fingerprint) = fingerprint {
            let _ = self.fingerprint.set(fingerprint);
        }

        // Successful completion is this stage's commit boundary. Compensating
        // actions protect only its in-progress attempt and never escape into
        // a later graph-level failure.
        drop(take_journal(&recorder));
        self.state.mark_finished();
        Ok(Arc::new(output))
    }

    async fn rollback_local(&self, recorder: Arc<Mutex<RollbackRecorder>>) {
        let journal = take_journal(&recorder);
        if journal.is_empty() {
            return;
        }
        self.state.mark_rolling_back();
        let errors = journal.rollback(RollbackContext::default()).await;
        if !errors.is_empty() {
            self.local_rollback_failures
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .push(RollbackFailure { task_id: self.id, errors });
        }
    }
}

fn take_journal(recorder: &Mutex<RollbackRecorder>) -> RollbackJournal {
    let recorder =
        std::mem::take(&mut *recorder.lock().unwrap_or_else(|poisoned| poisoned.into_inner()));
    recorder.into_journal()
}

pub struct TaskRef<T: Task> {
    inner: Arc<TaskNode<T>>,
}

impl<T: Task> Clone for TaskRef<T> {
    fn clone(&self) -> Self {
        Self { inner: self.inner.clone() }
    }
}

impl<T: Task> fmt::Debug for TaskRef<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TaskRef")
            .field("id", &self.id())
            .field("kind", &T::KIND)
            .field("label", &self.inner.label)
            .finish()
    }
}

impl<T: Task> TaskRef<T> {
    pub fn id(&self) -> TaskInstanceId {
        self.inner.id
    }

    pub const fn kind(&self) -> TaskKind {
        T::KIND
    }

    pub fn snapshot(&self) -> TaskSnapshot {
        self.inner.state.snapshot()
    }

    pub fn try_result(&self) -> Option<SharedTaskResult<T::Output>> {
        self.inner.result.get()
    }

    pub async fn result(&self) -> SharedTaskResult<T::Output> {
        self.inner.result.wait().await
    }

    pub(crate) fn erased(&self) -> ErasedNodeRef {
        ErasedNodeRef(self.inner.clone())
    }
}

pub fn task<T: Task>(task: T) -> TaskRef<T> {
    TaskRef { inner: Arc::new(TaskNode::new(task)) }
}

pub(crate) trait ErasedNode: Send + Sync {
    fn id(&self) -> TaskInstanceId;
    fn kind(&self) -> TaskKind;
    fn rust_type_name(&self) -> &'static str;
    fn label(&self) -> Arc<str>;
    fn effect(&self) -> EffectKind;
    fn output_claims(&self) -> Vec<PathBuf>;
    fn dependencies(&self) -> Vec<ErasedNodeRef>;
    fn snapshot(&self) -> TaskSnapshot;
    fn mark_ready(&self);
    fn output_fingerprint(&self) -> Option<OutputFingerprint>;
    fn execute(self: Arc<Self>, run: Arc<RunContext>) -> BoxFuture<'static, NodeOutcome>;
    fn finish_without_execution(
        self: Arc<Self>,
        failure: Arc<TaskFailure>,
    ) -> BoxFuture<'static, ()>;
}

impl<T: Task> ErasedNode for TaskNode<T> {
    fn id(&self) -> TaskInstanceId {
        self.id
    }

    fn kind(&self) -> TaskKind {
        T::KIND
    }

    fn rust_type_name(&self) -> &'static str {
        std::any::type_name::<T>()
    }

    fn label(&self) -> Arc<str> {
        self.label.clone()
    }

    fn effect(&self) -> EffectKind {
        self.task.effect()
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        self.task.output_claims()
    }

    fn dependencies(&self) -> Vec<ErasedNodeRef> {
        self.dependencies()
    }

    fn snapshot(&self) -> TaskSnapshot {
        self.state.snapshot()
    }

    fn mark_ready(&self) {
        self.state.mark_ready();
    }

    fn output_fingerprint(&self) -> Option<OutputFingerprint> {
        self.fingerprint.get().copied()
    }

    fn execute(self: Arc<Self>, run: Arc<RunContext>) -> BoxFuture<'static, NodeOutcome> {
        Box::pin(async move { self.execute_inner(run).await })
    }

    fn finish_without_execution(
        self: Arc<Self>,
        failure: Arc<TaskFailure>,
    ) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            let result = self.result.finish(Err(failure.clone())).await;
            if result.is_err() {
                if matches!(&failure.reason, FailureReason::Cancelled) {
                    self.state.mark_cancelled(failure);
                } else {
                    self.state.mark_skipped(failure);
                }
            }
        })
    }
}

#[derive(Clone)]
pub(crate) struct ErasedNodeRef(Arc<dyn ErasedNode>);

impl ErasedNodeRef {
    pub(crate) fn id(&self) -> TaskInstanceId {
        self.0.id()
    }

    pub(crate) fn kind(&self) -> TaskKind {
        self.0.kind()
    }

    pub(crate) fn rust_type_name(&self) -> &'static str {
        self.0.rust_type_name()
    }

    pub(crate) fn label(&self) -> Arc<str> {
        self.0.label()
    }

    pub(crate) fn effect(&self) -> EffectKind {
        self.0.effect()
    }

    pub(crate) fn output_claims(&self) -> Vec<PathBuf> {
        self.0.output_claims()
    }

    pub(crate) fn dependencies(&self) -> Vec<Self> {
        self.0.dependencies()
    }

    pub(crate) fn snapshot(&self) -> TaskSnapshot {
        self.0.snapshot()
    }

    pub(crate) fn mark_ready(&self) {
        self.0.mark_ready();
    }

    pub(crate) fn output_fingerprint(&self) -> Option<OutputFingerprint> {
        self.0.output_fingerprint()
    }

    pub(crate) async fn execute(&self, run: Arc<RunContext>) -> NodeOutcome {
        self.0.clone().execute(run).await
    }

    pub(crate) async fn finish_without_execution(&self, failure: Arc<TaskFailure>) {
        self.0.clone().finish_without_execution(failure).await;
    }
}

impl fmt::Debug for ErasedNodeRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ErasedNodeRef")
            .field("id", &self.id())
            .field("kind", &self.kind())
            .field("label", &self.label())
            .finish()
    }
}

#[derive(Debug)]
pub(crate) struct NodeOutcome {
    pub(crate) succeeded: bool,
    pub(crate) cache_hit: bool,
    pub(crate) local_rollback: Vec<RollbackFailure>,
}
