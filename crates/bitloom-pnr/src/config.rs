use std::collections::HashSet;
use std::num::NonZeroUsize;
use std::sync::Arc;

use thiserror::Error;

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum PnrConfigError {
    #[error("at least one PNR seed is required")]
    NoSeeds,
    #[error("PNR seed {seed} appears more than once")]
    DuplicateSeed { seed: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PnrConfig {
    seeds: Arc<[u64]>,
    parallelism: NonZeroUsize,
}

impl PnrConfig {
    pub fn new(
        seeds: impl IntoIterator<Item = u64>,
        parallelism: NonZeroUsize,
    ) -> Result<Self, PnrConfigError> {
        let seeds = seeds.into_iter().collect::<Vec<_>>();
        if seeds.is_empty() {
            return Err(PnrConfigError::NoSeeds);
        }

        let mut unique = HashSet::with_capacity(seeds.len());
        for &seed in &seeds {
            if !unique.insert(seed) {
                return Err(PnrConfigError::DuplicateSeed { seed });
            }
        }

        Ok(Self { seeds: seeds.into(), parallelism })
    }

    pub fn seeds(&self) -> &[u64] {
        &self.seeds
    }

    pub fn parallelism(&self) -> NonZeroUsize {
        self.parallelism
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_duplicate_seeds() {
        let error = PnrConfig::new([4, 7, 4], NonZeroUsize::MIN).unwrap_err();
        assert_eq!(error, PnrConfigError::DuplicateSeed { seed: 4 });
    }
}
