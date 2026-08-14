//! Typed task graph runtime used by Bitloom pipelines.
//!
//! Task inputs and outputs stay statically typed through [`TaskRef`]. Only the
//! scheduler-facing node interface is type-erased, and it never exposes task
//! outputs or uses `Any`/downcasting.

mod cache;
mod context;
mod dependencies;
mod error;
mod executor;
mod graph;
mod identity;
mod node;
mod result;
mod rollback;
mod state;
mod task;

use std::future::Future;
use std::pin::Pin;

pub use cache::{
    CacheError, CacheKey, CacheKeyInput, CacheMode, CachedOutput, OutputFingerprint, TaskCache,
};
pub use context::{Cancellation, ProgressError, RunContext, TaskContext};
pub use dependencies::{Dependencies, DependencyCollector};
pub use error::{FailureReason, SharedTaskResult, TaskFailure};
pub use executor::{ExecutionError, ExecutionFailure, ExecutionReport, Executor, ExecutorOptions};
pub use graph::{GraphBuildError, GraphBuilder, TaskGraph, TaskMonitor};
pub use identity::{TaskInstanceId, TaskKind};
pub use node::{TaskRef, task};
pub use rollback::{RollbackAction, RollbackContext, RollbackError, RollbackFailure};
pub use state::{TaskPhase, TaskProgress, TaskSnapshot};
pub use task::{EffectKind, Task};

/// A sendable future used only at object-safe control-plane boundaries.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Generates a named dependency set and its statically typed resolved inputs.
///
/// This covers the common case without a proc-macro crate or `build.rs`. More
/// complex dependency shapes can implement [`Dependencies`] directly.
///
/// ```no_run
/// # use std::convert::Infallible;
/// # use bitloom_core::{Task, TaskContext, TaskKind, TaskRef, task_dependencies};
/// # struct SynthesisTask;
/// # impl Task for SynthesisTask {
/// #     type Output = String;
/// #     type Error = Infallible;
/// #     type Dependencies = ();
/// #     const KIND: TaskKind = TaskKind::new("example.synthesis");
/// #     fn label(&self) -> std::borrow::Cow<'static, str> { "Synthesis".into() }
/// #     fn dependencies(&self) -> &() { &() }
/// #     async fn run(&self, _: TaskContext, (): ()) -> Result<String, Infallible> { Ok(String::new()) }
/// # }
/// task_dependencies! {
///     pub struct PackDependencies => PackInputs {
///         pub synthesis: TaskRef<SynthesisTask>,
///     }
/// }
/// ```
#[macro_export]
macro_rules! task_dependencies {
    (
        $(#[$attribute:meta])*
        $visibility:vis struct $dependencies:ident => $inputs:ident {
            $(
                $field_visibility:vis $field:ident : TaskRef<$task:ty>
            ),* $(,)?
        }
    ) => {
        $(#[$attribute])*
        $visibility struct $dependencies {
            $(
                $field_visibility $field: $crate::TaskRef<$task>,
            )*
        }

        $visibility struct $inputs {
            $(
                $field_visibility $field: ::std::sync::Arc<<$task as $crate::Task>::Output>,
            )*
        }

        impl $crate::Dependencies for $dependencies {
            type Resolved = $inputs;

            fn collect(&self, collector: &mut $crate::DependencyCollector) {
                $(
                    collector.add(&self.$field);
                )*
            }

            async fn resolve(
                &self,
            ) -> ::std::result::Result<
                Self::Resolved,
                ::std::sync::Arc<$crate::TaskFailure>,
            > {
                ::std::result::Result::Ok($inputs {
                    $(
                        $field: self.$field.result().await?,
                    )*
                })
            }
        }
    };
}

#[cfg(test)]
mod tests;
