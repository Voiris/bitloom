use std::collections::HashMap;
use std::io::{self, IsTerminal};
use std::time::Duration;

use anyhow::{Context, Result};
use bitloom_core::{ExecutionReport, Executor, TaskGraph, TaskInstanceId, TaskPhase, TaskSnapshot};
use bitloom_ui::TerminalReporter;
use tokio::time::{MissedTickBehavior, interval};

use crate::cli::OutputArgs;
use crate::output;

const STREAM_REFRESH_INTERVAL: Duration = Duration::from_millis(100);

pub fn is_streaming(options: OutputArgs) -> bool {
    options.verbose || options.advanced || !io::stdout().is_terminal()
}

pub async fn run(graph: &TaskGraph, options: OutputArgs) -> Result<()> {
    let monitor = graph.monitor();
    let executor = Executor::default();
    if is_streaming(options) {
        observe_streaming(&monitor, run_until_interrupted(&executor, graph)).await?;
    } else {
        let mut reporter = TerminalReporter::new(graph.len())?;
        let observed = reporter.observe(&monitor, run_until_interrupted(&executor, graph)).await;
        let finish_error = reporter.finish().err();
        drop(reporter);
        if let Some(error) = observed.rendering_error.or(finish_error) {
            output::warning(format_args!("task table rendering failed: {error}"));
        }
        observed.output?;
    }
    Ok(())
}

async fn observe_streaming<F>(monitor: &bitloom_core::TaskMonitor, future: F) -> F::Output
where
    F: Future,
{
    let mut tracker = StreamTracker::default();
    tracker.emit(&monitor.snapshots());
    let mut refresh = interval(STREAM_REFRESH_INTERVAL);
    refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);
    tokio::pin!(future);

    let output = loop {
        tokio::select! {
            output = &mut future => break output,
            _ = refresh.tick() => tracker.emit(&monitor.snapshots()),
        }
    };
    tracker.emit(&monitor.snapshots());
    output
}

#[derive(Default)]
struct StreamTracker {
    tasks: HashMap<TaskInstanceId, SeenTask>,
}

#[derive(Clone, Copy)]
struct SeenTask {
    phase: TaskPhase,
    progress: Option<bitloom_core::TaskProgress>,
    warnings: usize,
}

impl StreamTracker {
    fn emit(&mut self, snapshots: &[TaskSnapshot]) {
        for snapshot in snapshots {
            let previous = self.tasks.get(&snapshot.id).copied();
            let changed = previous.is_none_or(|previous| {
                previous.phase != snapshot.phase || previous.progress != snapshot.progress
            });
            if changed {
                emit_progress(snapshot);
            }

            let warning_count = previous.map_or(0, |previous| previous.warnings);
            for warning in snapshot.warnings.iter().skip(warning_count) {
                output::warning(warning);
            }
            self.tasks.insert(
                snapshot.id,
                SeenTask {
                    phase: snapshot.phase,
                    progress: snapshot.progress,
                    warnings: snapshot.warnings.len(),
                },
            );
        }
    }
}

fn emit_progress(task: &TaskSnapshot) {
    let Some((level, message)) = progress_update(task) else {
        return;
    };
    match level {
        StreamLevel::Info => output::info(message),
        StreamLevel::Success => output::success(message),
        StreamLevel::Warning => output::warning(message),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamLevel {
    Info,
    Success,
    Warning,
}

fn progress_update(task: &TaskSnapshot) -> Option<(StreamLevel, String)> {
    let update = match task.phase {
        TaskPhase::Waiting | TaskPhase::Ready => return None,
        TaskPhase::Running => match task.progress {
            Some(progress) => (
                StreamLevel::Info,
                format!("{}: running {}/{}", task.label, progress.completed, progress.total),
            ),
            None => (StreamLevel::Info, format!("{}: running", task.label)),
        },
        TaskPhase::Cached => (StreamLevel::Success, format!("{}: cached", task.label)),
        TaskPhase::Finished => (StreamLevel::Success, format!("{}: finished", task.label)),
        TaskPhase::Failed => (StreamLevel::Warning, format!("{}: failed", task.label)),
        TaskPhase::Cancelled => (StreamLevel::Warning, format!("{}: cancelled", task.label)),
        TaskPhase::Skipped => (StreamLevel::Info, format!("{}: skipped", task.label)),
        TaskPhase::RollingBack => (StreamLevel::Warning, format!("{}: rolling back", task.label)),
    };
    Some(update)
}

async fn run_until_interrupted(executor: &Executor, graph: &TaskGraph) -> Result<ExecutionReport> {
    let run_context = executor.run_context();
    let execution = executor.run(graph);
    tokio::pin!(execution);

    tokio::select! {
        result = &mut execution => Ok(result?),
        signal = tokio::signal::ctrl_c() => {
            signal.context("failed to listen for Ctrl+C")?;
            run_context.cancel();
            Ok(execution.await?)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use bitloom_core::{Task, TaskContext, TaskKind, task};

    use super::*;

    struct ExampleTask;

    impl Task for ExampleTask {
        type Output = ();
        type Error = std::convert::Infallible;
        type Dependencies = ();

        const KIND: TaskKind = TaskKind::new("test.streaming");

        fn label(&self) -> std::borrow::Cow<'static, str> {
            "Example".into()
        }

        fn dependencies(&self) -> &Self::Dependencies {
            &()
        }

        async fn run(&self, _context: TaskContext, (): ()) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    fn snapshot(phase: TaskPhase) -> TaskSnapshot {
        let mut snapshot = task(ExampleTask).snapshot();
        snapshot.label = Arc::from("Synthesis (top)");
        snapshot.phase = phase;
        snapshot
    }

    #[test]
    fn streaming_messages_include_task_labels_and_progress() {
        let mut running = snapshot(TaskPhase::Running);
        running.progress = Some(bitloom_core::TaskProgress { completed: 3, total: 8 });

        assert_eq!(
            progress_update(&running),
            Some((StreamLevel::Info, "Synthesis (top): running 3/8".to_owned()))
        );
        assert_eq!(
            progress_update(&snapshot(TaskPhase::Finished)),
            Some((StreamLevel::Success, "Synthesis (top): finished".to_owned()))
        );
    }
}
