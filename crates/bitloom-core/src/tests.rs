use std::borrow::Cow;
use std::convert::Infallible;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use thiserror::Error;

use crate::*;

struct CountTask {
    executions: Arc<AtomicUsize>,
    value: usize,
}

impl Task for CountTask {
    type Output = usize;
    type Error = Infallible;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("test.count");

    fn label(&self) -> Cow<'static, str> {
        "Count".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &()
    }

    async fn run(&self, _context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(self.value)
    }
}

#[tokio::test]
async fn one_node_executes_once_and_its_result_can_be_read_repeatedly() {
    let executions = Arc::new(AtomicUsize::new(0));
    let task_ref = task(CountTask { executions: executions.clone(), value: 42 });
    let graph = GraphBuilder::new().with_root(&task_ref).build().unwrap();

    Executor::default().run(&graph).await.unwrap();

    assert_eq!(*task_ref.result().await.unwrap(), 42);
    assert_eq!(*task_ref.result().await.unwrap(), 42);
    assert_eq!(*task_ref.try_result().unwrap().unwrap(), 42);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert!(matches!(Executor::default().run(&graph).await, Err(ExecutionError::AlreadyExecuted)));
}

struct WarningTask;

impl Task for WarningTask {
    type Output = ();
    type Error = Infallible;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("test.warning");

    fn label(&self) -> Cow<'static, str> {
        "Warn".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &()
    }

    async fn run(&self, context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
        context.warning("configuration is valid but unusual");
        Ok(())
    }
}

#[tokio::test]
async fn task_warnings_are_kept_in_live_and_final_snapshots() {
    let task_ref = task(WarningTask);
    let graph = GraphBuilder::new().with_root(&task_ref).build().unwrap();
    let report = Executor::default().run(&graph).await.unwrap();

    assert_eq!(task_ref.snapshot().warnings[0].as_ref(), "configuration is valid but unusual");
    assert_eq!(report.tasks[0].warnings[0].as_ref(), "configuration is valid but unusual");
}

#[tokio::test]
async fn multiple_instances_of_one_task_type_keep_distinct_results() {
    let executions = Arc::new(AtomicUsize::new(0));
    let first = task(CountTask { executions: executions.clone(), value: 3 });
    let second = task(CountTask { executions: executions.clone(), value: 9 });
    let mut builder = GraphBuilder::new();
    builder.add_root(&first).add_root(&second);
    let graph = builder.build().unwrap();

    Executor::default().run(&graph).await.unwrap();

    assert_ne!(first.id(), second.id());
    assert_eq!(*first.result().await.unwrap(), 3);
    assert_eq!(*second.result().await.unwrap(), 9);
    assert_eq!(executions.load(Ordering::SeqCst), 2);
}

task_dependencies! {
    struct GeneratedDependencies => GeneratedInputs {
        source: TaskRef<CountTask>,
    }
}

struct GeneratedDependencyTask {
    dependencies: GeneratedDependencies,
}

impl Task for GeneratedDependencyTask {
    type Output = usize;
    type Error = Infallible;
    type Dependencies = GeneratedDependencies;

    const KIND: TaskKind = TaskKind::new("test.generated-dependencies");

    fn label(&self) -> Cow<'static, str> {
        "Generated dependencies".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependencies
    }

    async fn run(
        &self,
        _context: TaskContext,
        inputs: GeneratedInputs,
    ) -> Result<Self::Output, Self::Error> {
        Ok(*inputs.source + 1)
    }
}

#[tokio::test]
async fn dependency_macro_preserves_concrete_output_type() {
    let source = task(CountTask { executions: Arc::new(AtomicUsize::new(0)), value: 4 });
    let consumer = task(GeneratedDependencyTask { dependencies: GeneratedDependencies { source } });
    let graph = GraphBuilder::new().with_root(&consumer).build().unwrap();

    Executor::default().run(&graph).await.unwrap();

    assert_eq!(*consumer.result().await.unwrap(), 5);
}

#[derive(Debug, Error)]
#[error("intentional failure")]
struct IntentionalFailure;

struct FailingTask;

impl Task for FailingTask {
    type Output = ();
    type Error = IntentionalFailure;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("test.failure");

    fn label(&self) -> Cow<'static, str> {
        "Failure".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &()
    }

    async fn run(&self, _context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
        Err(IntentionalFailure)
    }
}

struct DependentTask {
    dependency: TaskRef<FailingTask>,
    executions: Arc<AtomicUsize>,
}

impl Task for DependentTask {
    type Output = ();
    type Error = Infallible;
    type Dependencies = TaskRef<FailingTask>;

    const KIND: TaskKind = TaskKind::new("test.dependent");

    fn label(&self) -> Cow<'static, str> {
        "Dependent".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependency
    }

    async fn run(
        &self,
        _context: TaskContext,
        _input: Arc<()>,
    ) -> Result<Self::Output, Self::Error> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn failed_dependency_skips_descendant_and_completes_its_result_slot() {
    let executions = Arc::new(AtomicUsize::new(0));
    let failing = task(FailingTask);
    let dependent = task(DependentTask { dependency: failing, executions: executions.clone() });
    let graph = GraphBuilder::new().with_root(&dependent).build().unwrap();

    assert!(matches!(Executor::default().run(&graph).await, Err(ExecutionError::Failed(_))));
    let failure = dependent.result().await.unwrap_err();

    assert!(matches!(failure.reason, FailureReason::DependencyFailed { .. }));
    assert_eq!(dependent.snapshot().phase, TaskPhase::Skipped);
    assert_eq!(executions.load(Ordering::SeqCst), 0);
}

struct RecordRollback {
    name: &'static str,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl RollbackAction for RecordRollback {
    fn rollback(
        self: Box<Self>,
        _context: RollbackContext,
    ) -> BoxFuture<'static, Result<(), RollbackError>> {
        Box::pin(async move {
            self.log.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(self.name);
            Ok(())
        })
    }
}

struct RollbackRoot {
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Task for RollbackRoot {
    type Output = ();
    type Error = Infallible;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("test.rollback.root");

    fn label(&self) -> Cow<'static, str> {
        "Rollback root".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &()
    }

    fn effect(&self) -> EffectKind {
        EffectKind::Reversible
    }

    async fn run(&self, context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
        context.defer(RecordRollback { name: "root", log: self.log.clone() });
        Ok(())
    }
}

struct RollbackMiddle {
    dependency: TaskRef<RollbackRoot>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Task for RollbackMiddle {
    type Output = ();
    type Error = Infallible;
    type Dependencies = TaskRef<RollbackRoot>;

    const KIND: TaskKind = TaskKind::new("test.rollback.middle");

    fn label(&self) -> Cow<'static, str> {
        "Rollback middle".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependency
    }

    fn effect(&self) -> EffectKind {
        EffectKind::Reversible
    }

    async fn run(
        &self,
        context: TaskContext,
        _input: Arc<()>,
    ) -> Result<Self::Output, Self::Error> {
        context.defer(RecordRollback { name: "middle", log: self.log.clone() });
        Ok(())
    }
}

struct RollbackFailureTask {
    dependency: TaskRef<RollbackMiddle>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Task for RollbackFailureTask {
    type Output = ();
    type Error = IntentionalFailure;
    type Dependencies = TaskRef<RollbackMiddle>;

    const KIND: TaskKind = TaskKind::new("test.rollback.failure");

    fn label(&self) -> Cow<'static, str> {
        "Rollback failure".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependency
    }

    fn effect(&self) -> EffectKind {
        EffectKind::Reversible
    }

    async fn run(
        &self,
        context: TaskContext,
        _input: Arc<()>,
    ) -> Result<Self::Output, Self::Error> {
        context.defer(RecordRollback { name: "failed-first", log: self.log.clone() });
        context.defer(RecordRollback { name: "failed-second", log: self.log.clone() });
        Err(IntentionalFailure)
    }
}

#[tokio::test]
async fn rollback_is_local_to_the_unfinished_task() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let root = task(RollbackRoot { log: log.clone() });
    let middle = task(RollbackMiddle { dependency: root.clone(), log: log.clone() });
    let failure = task(RollbackFailureTask { dependency: middle.clone(), log: log.clone() });
    let graph = GraphBuilder::new().with_root(&failure).build().unwrap();

    assert!(Executor::default().run(&graph).await.is_err());

    assert_eq!(
        *log.lock().unwrap_or_else(|poisoned| poisoned.into_inner()),
        ["failed-second", "failed-first"]
    );
    assert_eq!(middle.snapshot().phase, TaskPhase::Finished);
    assert_eq!(root.snapshot().phase, TaskPhase::Finished);
    assert!(middle.result().await.is_ok());
    assert!(root.result().await.is_ok());
}

struct WaitForCancellation {
    dependency: TaskRef<RollbackRoot>,
    started: Arc<tokio::sync::Notify>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl Task for WaitForCancellation {
    type Output = ();
    type Error = IntentionalFailure;
    type Dependencies = TaskRef<RollbackRoot>;

    const KIND: TaskKind = TaskKind::new("test.cancellation.wait");

    fn label(&self) -> Cow<'static, str> {
        "Wait for cancellation".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependency
    }

    async fn run(
        &self,
        context: TaskContext,
        _input: Arc<()>,
    ) -> Result<Self::Output, Self::Error> {
        context.defer(RecordRollback { name: "cancelled", log: self.log.clone() });
        self.started.notify_one();
        context.cancelled().await;
        Err(IntentionalFailure)
    }
}

#[tokio::test]
async fn cancellation_rolls_back_only_the_unfinished_task() {
    let log = Arc::new(Mutex::new(Vec::new()));
    let started = Arc::new(tokio::sync::Notify::new());
    let completed = task(RollbackRoot { log: log.clone() });
    let unfinished = task(WaitForCancellation {
        dependency: completed.clone(),
        started: started.clone(),
        log: log.clone(),
    });
    let graph = GraphBuilder::new().with_root(&unfinished).build().unwrap();
    let executor = Executor::default();
    let run_context = executor.run_context();

    let cancel = async move {
        started.notified().await;
        run_context.cancel();
    };
    let (result, ()) = tokio::join!(executor.run(&graph), cancel);
    let ExecutionError::Failed(failure) = result.unwrap_err() else {
        panic!("expected cancellation failure");
    };

    assert!(matches!(failure.primary.reason, FailureReason::Cancelled));
    assert_eq!(completed.snapshot().phase, TaskPhase::Finished);
    assert_eq!(unfinished.snapshot().phase, TaskPhase::Cancelled);
    assert_eq!(*log.lock().unwrap_or_else(|poisoned| poisoned.into_inner()), ["cancelled"]);
    assert!(completed.result().await.is_ok());
}

#[derive(Clone, Default)]
struct LateDependency {
    target: Arc<Mutex<Option<TaskRef<CycleTask>>>>,
}

impl Dependencies for LateDependency {
    type Resolved = Arc<()>;

    fn collect(&self, collector: &mut DependencyCollector) {
        if let Some(target) =
            self.target.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref()
        {
            collector.add(target);
        }
    }

    async fn resolve(&self) -> Result<Self::Resolved, Arc<TaskFailure>> {
        let target = self.target.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
        match target {
            Some(target) => target.result().await,
            None => Ok(Arc::new(())),
        }
    }
}

struct CycleTask {
    dependency: LateDependency,
    label: &'static str,
}

impl Task for CycleTask {
    type Output = ();
    type Error = Infallible;
    type Dependencies = LateDependency;

    const KIND: TaskKind = TaskKind::new("test.cycle");

    fn label(&self) -> Cow<'static, str> {
        self.label.into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependency
    }

    async fn run(
        &self,
        _context: TaskContext,
        _input: Arc<()>,
    ) -> Result<Self::Output, Self::Error> {
        Ok(())
    }
}

#[test]
fn builder_reports_dependency_cycles_with_labels() {
    let first_dependency = LateDependency::default();
    let second_dependency = LateDependency::default();
    let first = task(CycleTask { dependency: first_dependency.clone(), label: "first" });
    let second = task(CycleTask { dependency: second_dependency.clone(), label: "second" });
    *first_dependency.target.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some(second.clone());
    *second_dependency.target.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
        Some(first.clone());

    let error = GraphBuilder::new().with_root(&first).build().unwrap_err();
    assert!(matches!(error, GraphBuildError::Cycle { .. }));
    assert!(error.to_string().contains("first -> second -> first"));
}

#[derive(Default)]
struct MemoryCache {
    output: Mutex<Option<CachedOutput<usize>>>,
    stores: AtomicUsize,
    restores: AtomicUsize,
}

impl TaskCache<usize> for MemoryCache {
    fn key(&self, input: CacheKeyInput<'_>) -> Result<CacheKey, CacheError> {
        assert_eq!(input.kind, TaskKind::new("test.cache"));
        Ok(CacheKey::new([7; 32]))
    }

    fn restore<'a>(
        &'a self,
        _key: &'a CacheKey,
    ) -> BoxFuture<'a, Result<Option<CachedOutput<usize>>, CacheError>> {
        Box::pin(async move {
            self.restores.fetch_add(1, Ordering::SeqCst);
            Ok(self.output.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).as_ref().map(
                |cached| CachedOutput { output: cached.output, fingerprint: cached.fingerprint },
            ))
        })
    }

    fn store<'a>(
        &'a self,
        _key: &'a CacheKey,
        output: &'a usize,
    ) -> BoxFuture<'a, Result<OutputFingerprint, CacheError>> {
        Box::pin(async move {
            self.stores.fetch_add(1, Ordering::SeqCst);
            let fingerprint = OutputFingerprint::new([9; 32]);
            *self.output.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) =
                Some(CachedOutput { output: *output, fingerprint });
            Ok(fingerprint)
        })
    }
}

struct CachedTask {
    cache: Arc<MemoryCache>,
    executions: Arc<AtomicUsize>,
}

impl Task for CachedTask {
    type Output = usize;
    type Error = Infallible;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("test.cache");
    const CACHE_SCHEMA: u32 = 1;

    fn label(&self) -> Cow<'static, str> {
        "Cached".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &()
    }

    fn cache(&self) -> Option<&dyn TaskCache<Self::Output>> {
        Some(self.cache.as_ref())
    }

    async fn run(&self, _context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
        self.executions.fetch_add(1, Ordering::SeqCst);
        Ok(77)
    }
}

struct FailingCachedConsumer {
    dependency: TaskRef<CachedTask>,
}

impl Task for FailingCachedConsumer {
    type Output = ();
    type Error = IntentionalFailure;
    type Dependencies = TaskRef<CachedTask>;

    const KIND: TaskKind = TaskKind::new("test.cache.consumer.failure");

    fn label(&self) -> Cow<'static, str> {
        "Failing cached consumer".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependency
    }

    async fn run(
        &self,
        _context: TaskContext,
        _input: Arc<usize>,
    ) -> Result<Self::Output, Self::Error> {
        Err(IntentionalFailure)
    }
}

#[tokio::test]
async fn downstream_failure_keeps_completed_stage_and_its_cache() {
    let cache = Arc::new(MemoryCache::default());
    let executions = Arc::new(AtomicUsize::new(0));
    let completed = task(CachedTask { cache: cache.clone(), executions: executions.clone() });
    let failure = task(FailingCachedConsumer { dependency: completed.clone() });
    let graph = GraphBuilder::new().with_root(&failure).build().unwrap();

    assert!(Executor::default().run(&graph).await.is_err());
    assert_eq!(completed.snapshot().phase, TaskPhase::Finished);
    assert_eq!(*completed.result().await.unwrap(), 77);
    assert_eq!(cache.stores.load(Ordering::SeqCst), 1);

    let restored = task(CachedTask { cache: cache.clone(), executions: executions.clone() });
    let restored_graph = GraphBuilder::new().with_root(&restored).build().unwrap();
    let report = Executor::default().run(&restored_graph).await.unwrap();

    assert_eq!(restored.snapshot().phase, TaskPhase::Cached);
    assert_eq!(report.cache_hits, 1);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn typed_cache_restores_output_without_running_handler_again() {
    let cache = Arc::new(MemoryCache::default());
    let executions = Arc::new(AtomicUsize::new(0));
    let first = task(CachedTask { cache: cache.clone(), executions: executions.clone() });
    let first_graph = GraphBuilder::new().with_root(&first).build().unwrap();
    Executor::default().run(&first_graph).await.unwrap();

    let second = task(CachedTask { cache: cache.clone(), executions: executions.clone() });
    let second_graph = GraphBuilder::new().with_root(&second).build().unwrap();
    let report = Executor::new(ExecutorOptions {
        max_parallel_tasks: NonZeroUsize::MIN,
        cache_mode: CacheMode::Strict,
    })
    .run(&second_graph)
    .await
    .unwrap();

    assert_eq!(*second.result().await.unwrap(), 77);
    assert_eq!(second.snapshot().phase, TaskPhase::Cached);
    assert_eq!(report.cache_hits, 1);
    assert_eq!(executions.load(Ordering::SeqCst), 1);
    assert_eq!(cache.stores.load(Ordering::SeqCst), 1);
    assert_eq!(cache.restores.load(Ordering::SeqCst), 2);
}
