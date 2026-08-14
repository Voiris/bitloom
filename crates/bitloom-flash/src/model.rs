use std::path::PathBuf;

use bitloom_core::OutputFingerprint;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramMode {
    Sram,
    Flash,
}

#[derive(Debug, Clone)]
pub struct ProgramImage {
    pub path: PathBuf,
    pub mode: ProgramMode,
    pub fingerprint: OutputFingerprint,
}
