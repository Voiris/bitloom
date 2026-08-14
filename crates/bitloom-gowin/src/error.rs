use std::path::PathBuf;
use std::process::ExitStatus;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("failed to prepare {path}: {source}")]
    Prepare { path: PathBuf, source: std::io::Error },
    #[error("failed to start {tool}: {source}")]
    Start { tool: &'static str, source: std::io::Error },
    #[error("failed to wait for {tool}: {source}")]
    Wait { tool: &'static str, source: std::io::Error },
    #[error("failed to collect {tool} output: {message}")]
    Output { tool: &'static str, message: String },
    #[error("failed to terminate {tool} after cancellation: {source}")]
    Terminate { tool: &'static str, source: std::io::Error },
    #[error("{tool} was cancelled")]
    Cancelled { tool: &'static str },
    #[error("{tool} failed with {status}: {stderr}")]
    Exit { tool: &'static str, status: ExitStatus, stderr: String },
    #[error("{tool} completed without creating {path}")]
    MissingOutput { tool: &'static str, path: PathBuf },
    #[error("failed to read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("failed to parse nextpnr report {path}: {source}")]
    Report { path: PathBuf, source: serde_json::Error },
    #[error("nextpnr produced a non-finite timing score")]
    InvalidScore,
    #[error("failed to construct OSS CAD Suite environment: {message}")]
    Environment { message: String },
}
