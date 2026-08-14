//! Place-and-route tasks for Bitloom.
//!
//! One [`PnrTask`] represents the complete multi-seed search. Candidate runs
//! are an implementation detail of that task, so the graph and the UI expose a
//! single row whose progress advances from `0/N` to `N/N`.

mod backend;
mod config;
mod model;
mod task;

pub use backend::{PnrBackend, PnrFailureKind, PnrRunContext};
pub use config::{PnrConfig, PnrConfigError};
pub use model::{
    ClockFrequency, InvalidPnrScore, PnrCandidate, PnrOutput, PnrRun, PnrRunFailure, PnrScore,
    PnrStatistics, ResourceUsage,
};
pub use task::{PnrTask, PnrTaskError};
