use std::collections::BTreeMap;

use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PnrScore(f64);

impl PnrScore {
    pub fn new(value: f64) -> Result<Self, InvalidPnrScore> {
        if value.is_finite() { Ok(Self(value)) } else { Err(InvalidPnrScore { value }) }
    }

    pub fn get(self) -> f64 {
        self.0
    }
}

impl Eq for PnrScore {}

impl Ord for PnrScore {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl PartialOrd for PnrScore {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug, Clone, Copy, Error, PartialEq)]
#[error("PNR score must be finite, got {value}")]
pub struct InvalidPnrScore {
    value: f64,
}

#[derive(Debug)]
pub struct PnrRun<A> {
    pub score: PnrScore,
    pub artifact: A,
    pub statistics: PnrStatistics,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct PnrStatistics {
    pub resources: BTreeMap<String, ResourceUsage>,
    pub clocks: BTreeMap<String, ClockFrequency>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceUsage {
    pub used: u64,
    pub available: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ClockFrequency {
    pub achieved_mhz: f64,
    pub constraint_mhz: f64,
}

#[derive(Debug)]
pub struct PnrCandidate<A> {
    pub run_index: usize,
    pub seed: u64,
    pub score: PnrScore,
    pub artifact: A,
    pub statistics: PnrStatistics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnrRunFailure {
    pub run_index: usize,
    pub seed: u64,
    pub message: String,
}

#[derive(Debug)]
pub struct PnrOutput<A> {
    pub selected: PnrCandidate<A>,
    pub attempts: usize,
    pub successful_runs: usize,
    pub failed_runs: Vec<PnrRunFailure>,
}
