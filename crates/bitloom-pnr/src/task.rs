use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use bitloom_core::{
    EffectKind, OutputFingerprint, Task, TaskCache, TaskContext, TaskKind, TaskRef,
};
use futures_util::{StreamExt, stream};

use crate::{
    PnrBackend, PnrCandidate, PnrConfig, PnrFailureKind, PnrOutput, PnrRunContext, PnrRunFailure,
};

const MAX_DISPLAYED_FAILURE_GROUPS: usize = 3;

#[derive(Debug)]
pub enum PnrTaskError {
    Cancelled,
    Prepare { message: String },
    AllRunsFailed { attempts: usize, failures: Vec<PnrRunFailure> },
    Fatal { message: String },
    Progress { message: String },
}

impl fmt::Display for PnrTaskError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => formatter.write_str("place-and-route was cancelled"),
            Self::Prepare { message } => {
                write!(formatter, "failed to prepare place-and-route runs: {message}")
            }
            Self::AllRunsFailed { attempts, failures } => {
                format_run_failures(formatter, *attempts, failures)
            }
            Self::Fatal { message } => {
                write!(formatter, "place-and-route cannot continue: {message}")
            }
            Self::Progress { message } => {
                write!(formatter, "failed to update place-and-route progress: {message}")
            }
        }
    }
}

impl std::error::Error for PnrTaskError {}

fn format_run_failures(
    formatter: &mut fmt::Formatter<'_>,
    attempts: usize,
    failures: &[PnrRunFailure],
) -> fmt::Result {
    write!(formatter, "all {attempts} place-and-route runs failed")?;
    let groups = group_failures(failures);
    if let [(message, _)] = groups.as_slice() {
        return write!(formatter, ": {message}");
    }
    for (message, seeds) in groups.iter().take(MAX_DISPLAYED_FAILURE_GROUPS) {
        write!(formatter, "\n  seeds {}: {message}", format_seeds(seeds))?;
    }
    let hidden = groups.len().saturating_sub(MAX_DISPLAYED_FAILURE_GROUPS);
    if hidden > 0 {
        write!(formatter, "\n  ... and {hidden} other distinct failure(s)")?;
    }
    Ok(())
}

fn group_failures(failures: &[PnrRunFailure]) -> Vec<(&str, Vec<u64>)> {
    let mut groups: Vec<(&str, Vec<u64>)> = Vec::new();
    for failure in failures {
        match groups.iter_mut().find(|(message, _)| *message == failure.message) {
            Some((_, seeds)) => seeds.push(failure.seed),
            None => groups.push((&failure.message, vec![failure.seed])),
        }
    }
    groups
}

fn format_seeds(seeds: &[u64]) -> String {
    let mut seeds = seeds.to_vec();
    seeds.sort_unstable();
    let mut ranges = Vec::new();
    let mut start = seeds[0];
    let mut end = start;
    for seed in seeds.into_iter().skip(1) {
        if seed == end.saturating_add(1) {
            end = seed;
        } else {
            ranges.push(format_seed_range(start, end));
            start = seed;
            end = seed;
        }
    }
    ranges.push(format_seed_range(start, end));
    ranges.join(", ")
}

fn format_seed_range(start: u64, end: u64) -> String {
    if start == end { start.to_string() } else { format!("{start}-{end}") }
}

pub struct PnrTask<D, B>
where
    D: Task,
    B: PnrBackend<D::Output>,
{
    dependency: TaskRef<D>,
    backend: B,
    config: PnrConfig,
    label: Arc<str>,
    cache: Option<Arc<dyn TaskCache<PnrOutput<B::Artifact>>>>,
}

impl<D, B> PnrTask<D, B>
where
    D: Task,
    B: PnrBackend<D::Output>,
{
    pub fn new(dependency: TaskRef<D>, backend: B, config: PnrConfig) -> Self {
        Self { dependency, backend, config, label: Arc::from("Place & Route"), cache: None }
    }

    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_cache(mut self, cache: Arc<dyn TaskCache<PnrOutput<B::Artifact>>>) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn config(&self) -> &PnrConfig {
        &self.config
    }

    async fn run_candidates(
        &self,
        context: TaskContext,
        input: Arc<D::Output>,
    ) -> Result<PnrOutput<B::Artifact>, PnrTaskError> {
        let total = self.config.seeds().len();
        context
            .set_progress(0, total as u64)
            .map_err(|error| PnrTaskError::Progress { message: error.to_string() })?;
        self.backend
            .prepare(context.clone(), input.clone())
            .await
            .map_err(|error| PnrTaskError::Prepare { message: error.to_string() })?;

        let runs = stream::iter(self.config.seeds().iter().copied().enumerate())
            .map(|(run_index, seed)| {
                let input = input.clone();
                let run_context =
                    PnrRunContext { task: context.clone(), run_index, total_runs: total };
                async move {
                    if run_context.is_cancelled() {
                        return CandidateResult::Cancelled;
                    }
                    match self.backend.run(run_context, input, seed).await {
                        Ok(run) => CandidateResult::Succeeded(PnrCandidate {
                            run_index,
                            seed,
                            score: run.score,
                            artifact: run.artifact,
                            statistics: run.statistics,
                        }),
                        Err(error) => CandidateResult::Failed {
                            failure: PnrRunFailure {
                                run_index,
                                seed,
                                message: self.backend.format_error(&error),
                            },
                            kind: self.backend.failure_kind(&error),
                        },
                    }
                }
            })
            .buffer_unordered(self.config.parallelism().get());
        futures_util::pin_mut!(runs);

        let mut completed = 0;
        let mut cancelled = false;
        let mut candidates = Vec::new();
        let mut failures = Vec::new();
        while let Some(result) = runs.next().await {
            completed += 1;
            context
                .set_progress(completed as u64, total as u64)
                .map_err(|error| PnrTaskError::Progress { message: error.to_string() })?;
            match result {
                CandidateResult::Succeeded(candidate) => candidates.push(candidate),
                CandidateResult::Failed { failure, kind: PnrFailureKind::Retryable } => {
                    failures.push(failure);
                }
                CandidateResult::Failed { failure, kind: PnrFailureKind::Fatal } => {
                    return Err(PnrTaskError::Fatal { message: failure.message });
                }
                CandidateResult::Cancelled => cancelled = true,
            }
        }

        if cancelled || context.is_cancelled() {
            return Err(PnrTaskError::Cancelled);
        }

        let Some(selected_index) = candidates
            .iter()
            .enumerate()
            .max_by(|(_, first), (_, second)| {
                first.score.cmp(&second.score).then_with(|| second.run_index.cmp(&first.run_index))
            })
            .map(|(index, _)| index)
        else {
            return Err(PnrTaskError::AllRunsFailed { attempts: total, failures });
        };
        let selected = candidates.swap_remove(selected_index);

        Ok(PnrOutput {
            selected,
            attempts: total,
            successful_runs: candidates.len() + 1,
            failed_runs: failures,
        })
    }
}

impl<D, B> Task for PnrTask<D, B>
where
    D: Task,
    B: PnrBackend<D::Output>,
{
    type Output = PnrOutput<B::Artifact>;
    type Error = PnrTaskError;
    type Dependencies = TaskRef<D>;

    const KIND: TaskKind = TaskKind::new("bitloom.pnr");
    const CACHE_SCHEMA: u32 = 1;

    fn label(&self) -> std::borrow::Cow<'static, str> {
        std::borrow::Cow::Owned(self.label.to_string())
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependency
    }

    fn effect(&self) -> EffectKind {
        self.backend.effect()
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        self.backend.output_claims()
    }

    fn cache(&self) -> Option<&dyn TaskCache<Self::Output>> {
        self.cache.as_deref()
    }

    fn output_fingerprint(&self, output: &Self::Output) -> Option<OutputFingerprint> {
        self.backend.output_fingerprint(&output.selected)
    }

    async fn run(
        &self,
        context: TaskContext,
        input: Arc<D::Output>,
    ) -> Result<Self::Output, Self::Error> {
        self.run_candidates(context, input).await
    }
}

enum CandidateResult<A> {
    Succeeded(PnrCandidate<A>),
    Failed { failure: PnrRunFailure, kind: PnrFailureKind },
    Cancelled,
}

impl<A> fmt::Debug for CandidateResult<A> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Succeeded(_) => formatter.write_str("Succeeded(..)"),
            Self::Failed { failure, kind } => formatter
                .debug_struct("Failed")
                .field("failure", failure)
                .field("kind", kind)
                .finish(),
            Self::Cancelled => formatter.write_str("Cancelled"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::convert::Infallible;
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use bitloom_core::{Executor, GraphBuilder, TaskProgress, task};
    use thiserror::Error;

    use super::*;
    use crate::{ClockFrequency, PnrRun, PnrScore, PnrStatistics};

    struct InputTask;

    impl Task for InputTask {
        type Output = usize;
        type Error = Infallible;
        type Dependencies = ();

        const KIND: TaskKind = TaskKind::new("test.input");

        fn label(&self) -> std::borrow::Cow<'static, str> {
            "Input".into()
        }

        fn dependencies(&self) -> &Self::Dependencies {
            &()
        }

        async fn run(&self, _context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
            Ok(1)
        }
    }

    #[derive(Debug, Error)]
    #[error("candidate failed")]
    struct MockError;

    struct MockBackend {
        active: Arc<AtomicUsize>,
        maximum: Arc<AtomicUsize>,
        preparations: Arc<AtomicUsize>,
    }

    impl PnrBackend<usize> for MockBackend {
        type Artifact = u64;
        type Error = MockError;

        async fn prepare(
            &self,
            _context: TaskContext,
            _input: Arc<usize>,
        ) -> Result<(), Self::Error> {
            self.preparations.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        async fn run(
            &self,
            _context: PnrRunContext,
            _input: Arc<usize>,
            seed: u64,
        ) -> Result<PnrRun<Self::Artifact>, Self::Error> {
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum.fetch_max(active, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(2)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);

            let statistics = PnrStatistics {
                resources: BTreeMap::new(),
                clocks: BTreeMap::from([(
                    "clk".to_owned(),
                    ClockFrequency { achieved_mhz: seed as f64, constraint_mhz: 1.0 },
                )]),
            };

            Ok(PnrRun {
                score: PnrScore::new(seed as f64).map_err(|_| MockError)?,
                artifact: seed,
                statistics,
            })
        }
    }

    #[tokio::test]
    async fn runs_many_candidates_with_bounded_parallelism_and_selects_best() {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let preparations = Arc::new(AtomicUsize::new(0));
        let input = task(InputTask);
        let pnr = task(PnrTask::new(
            input,
            MockBackend { active, maximum: maximum.clone(), preparations: preparations.clone() },
            PnrConfig::new(0..64, NonZeroUsize::new(8).unwrap()).unwrap(),
        ));
        let graph = GraphBuilder::new().with_root(&pnr).build().unwrap();

        Executor::default().run(&graph).await.unwrap();
        let output = pnr.result().await.unwrap();

        assert_eq!(output.selected.seed, 63);
        assert_eq!(output.selected.statistics.clocks["clk"].achieved_mhz, 63.0);
        assert_eq!(output.attempts, 64);
        assert_eq!(output.successful_runs, 64);
        assert_eq!(preparations.load(Ordering::SeqCst), 1);
        assert!((2..=8).contains(&maximum.load(Ordering::SeqCst)));
        assert_eq!(pnr.snapshot().progress, Some(TaskProgress { completed: 64, total: 64 }));
    }

    #[test]
    fn groups_repeated_failures_and_limits_distinct_diagnostics() {
        let failures = (0..8)
            .map(|seed| PnrRunFailure {
                run_index: seed as usize,
                seed,
                message: if seed < 4 {
                    "routing failed".to_owned()
                } else {
                    format!("failure {seed}")
                },
            })
            .collect();
        let message = PnrTaskError::AllRunsFailed { attempts: 8, failures }.to_string();

        assert_eq!(message.matches("routing failed").count(), 1);
        assert!(message.contains("seeds 0-3: routing failed"));
        assert!(message.contains("and 2 other distinct failure(s)"));
    }

    #[derive(Debug, Error)]
    enum ClassifiedError {
        #[error("invalid constraints")]
        Fatal,
        #[error("candidate routing failed")]
        Retryable,
    }

    struct FailingBackend {
        started: Arc<AtomicUsize>,
        barrier: Arc<tokio::sync::Barrier>,
    }

    impl PnrBackend<usize> for FailingBackend {
        type Artifact = u64;
        type Error = ClassifiedError;

        async fn run(
            &self,
            _context: PnrRunContext,
            _input: Arc<usize>,
            seed: u64,
        ) -> Result<PnrRun<Self::Artifact>, Self::Error> {
            self.started.fetch_add(1, Ordering::SeqCst);
            self.barrier.wait().await;
            if seed == 0 {
                Err(ClassifiedError::Fatal)
            } else {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Err(ClassifiedError::Retryable)
            }
        }

        fn failure_kind(&self, error: &Self::Error) -> PnrFailureKind {
            match error {
                ClassifiedError::Fatal => PnrFailureKind::Fatal,
                ClassifiedError::Retryable => PnrFailureKind::Retryable,
            }
        }
    }

    #[tokio::test]
    async fn fatal_failure_stops_unstarted_candidates() {
        let parallelism = NonZeroUsize::new(4).unwrap();
        let started = Arc::new(AtomicUsize::new(0));
        let input = task(InputTask);
        let pnr = task(PnrTask::new(
            input,
            FailingBackend {
                started: started.clone(),
                barrier: Arc::new(tokio::sync::Barrier::new(parallelism.get())),
            },
            PnrConfig::new(0..64, parallelism).unwrap(),
        ));
        let graph = GraphBuilder::new().with_root(&pnr).build().unwrap();

        let result = tokio::time::timeout(Duration::from_secs(1), Executor::default().run(&graph))
            .await
            .expect("fatal PNR failure should stop active candidates");

        assert!(result.is_err());
        assert_eq!(started.load(Ordering::SeqCst), parallelism.get());
        assert!(pnr.result().await.unwrap_err().to_string().contains("invalid constraints"));
    }
}
