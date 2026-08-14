use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::Arc;

use futures_util::FutureExt;
use thiserror::Error;
use tokio::task::JoinSet;

use crate::node::NodeOutcome;
use crate::{
    CacheMode, EffectKind, FailureReason, RollbackFailure, RunContext, TaskFailure, TaskGraph,
    TaskPhase, TaskSnapshot,
};

#[derive(Debug, Clone)]
pub struct ExecutorOptions {
    pub max_parallel_tasks: NonZeroUsize,
    pub cache_mode: CacheMode,
}

impl Default for ExecutorOptions {
    fn default() -> Self {
        Self {
            max_parallel_tasks: std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN),
            cache_mode: CacheMode::BestEffort,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExecutionReport {
    pub tasks: Vec<TaskSnapshot>,
    pub cache_hits: usize,
}

#[derive(Debug)]
pub struct ExecutionFailure {
    pub primary: Arc<TaskFailure>,
    pub rollback_failures: Vec<RollbackFailure>,
}

impl fmt::Display for ExecutionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}", self.primary)?;
        if !self.rollback_failures.is_empty() {
            write!(formatter, "; rollback failed for {} task(s)", self.rollback_failures.len())?;
        }
        Ok(())
    }
}

impl std::error::Error for ExecutionFailure {}

#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error("a task graph can only be executed once")]
    AlreadyExecuted,
    #[error("graph execution failed: {0}")]
    Failed(ExecutionFailure),
}

pub struct Executor {
    options: ExecutorOptions,
    run_context: Arc<RunContext>,
}

impl Default for Executor {
    fn default() -> Self {
        Self::new(ExecutorOptions::default())
    }
}

impl Executor {
    pub fn new(options: ExecutorOptions) -> Self {
        let run_context = Arc::new(RunContext::new(options.cache_mode));
        Self { options, run_context }
    }

    pub fn run_context(&self) -> Arc<RunContext> {
        self.run_context.clone()
    }

    pub async fn run(&self, graph: &TaskGraph) -> Result<ExecutionReport, ExecutionError> {
        if !graph.begin_execution() {
            return Err(ExecutionError::AlreadyExecuted);
        }

        let mut indegrees = graph.parents.iter().map(Vec::len).collect::<Vec<_>>();
        let mut ready = VecDeque::new();
        let mut irreversible_ready = VecDeque::new();
        let mut remaining_reversible =
            graph.nodes.iter().filter(|node| node.effect() != EffectKind::Irreversible).count();

        for (index, indegree) in indegrees.iter().enumerate() {
            if *indegree == 0 {
                graph.nodes[index].mark_ready();
                enqueue_ready(graph, index, &mut ready, &mut irreversible_ready);
            }
        }

        let mut running = JoinSet::new();
        let mut first_failure = None;
        let mut rollback_failures = Vec::new();
        let mut cache_hits = 0;

        loop {
            if self.run_context.is_cancelled() && first_failure.is_none() {
                first_failure = graph.nodes.iter().find_map(|node| {
                    (!node.snapshot().phase.is_terminal())
                        .then(|| Arc::new(TaskFailure::cancelled(node.id(), node.kind())))
                });
            }

            while first_failure.is_none() && running.len() < self.options.max_parallel_tasks.get() {
                let next = ready.pop_front().or_else(|| {
                    (remaining_reversible == 0 && running.is_empty())
                        .then(|| irreversible_ready.pop_front())
                        .flatten()
                });
                let Some(index) = next else {
                    break;
                };

                let node = graph.nodes[index].clone();
                let run_context = self.run_context.clone();
                running.spawn(async move {
                    let outcome = std::panic::AssertUnwindSafe(node.execute(run_context))
                        .catch_unwind()
                        .await;
                    match outcome {
                        Ok(outcome) => (index, outcome),
                        Err(_) => {
                            let failure = Arc::new(TaskFailure::new(
                                node.id(),
                                node.kind(),
                                FailureReason::Panic("executor boundary panicked".to_owned()),
                            ));
                            node.finish_without_execution(failure).await;
                            (
                                index,
                                NodeOutcome {
                                    succeeded: false,
                                    cache_hit: false,
                                    local_rollback: Vec::new(),
                                },
                            )
                        }
                    }
                });
            }

            if running.is_empty() {
                break;
            }

            let Some(joined) = running.join_next().await else {
                break;
            };
            let Ok((index, outcome)) = joined else {
                continue;
            };

            rollback_failures.extend(outcome.local_rollback);
            if graph.nodes[index].effect() != EffectKind::Irreversible {
                remaining_reversible = remaining_reversible.saturating_sub(1);
            }

            if outcome.succeeded {
                if outcome.cache_hit {
                    cache_hits += 1;
                }

                if first_failure.is_none() {
                    for &child in &graph.children[index] {
                        indegrees[child] = indegrees[child].saturating_sub(1);
                        if indegrees[child] == 0 {
                            graph.nodes[child].mark_ready();
                            enqueue_ready(graph, child, &mut ready, &mut irreversible_ready);
                        }
                    }
                }
            } else if first_failure.is_none() {
                first_failure = graph.nodes[index].snapshot().failure.or_else(|| {
                    Some(Arc::new(TaskFailure::new(
                        graph.nodes[index].id(),
                        graph.nodes[index].kind(),
                        FailureReason::Task("task failed without a recorded reason".to_owned()),
                    )))
                });
                self.run_context.cancel();
            }
        }

        for (index, node) in graph.nodes.iter().enumerate() {
            if node.snapshot().phase.is_terminal() {
                continue;
            }

            let dependency = graph.parents[index].iter().find_map(|parent| {
                matches!(
                    graph.nodes[*parent].snapshot().phase,
                    TaskPhase::Failed | TaskPhase::Cancelled | TaskPhase::Skipped
                )
                .then(|| graph.nodes[*parent].id())
            });
            let failure = Arc::new(match dependency {
                Some(dependency) => TaskFailure::dependency(node.id(), node.kind(), dependency),
                None => TaskFailure::cancelled(node.id(), node.kind()),
            });
            node.finish_without_execution(failure.clone()).await;
            if first_failure.is_none() {
                first_failure = Some(failure);
            }
        }

        if let Some(primary) = first_failure {
            return Err(ExecutionError::Failed(ExecutionFailure { primary, rollback_failures }));
        }

        Ok(ExecutionReport { tasks: graph.monitor().snapshots(), cache_hits })
    }
}

fn enqueue_ready(
    graph: &TaskGraph,
    index: usize,
    ready: &mut VecDeque<usize>,
    irreversible_ready: &mut VecDeque<usize>,
) {
    if graph.nodes[index].effect() == EffectKind::Irreversible {
        irreversible_ready.push_back(index);
    } else {
        ready.push_back(index);
    }
}
