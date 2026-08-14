use std::fmt;

use thiserror::Error;

use crate::{BoxFuture, TaskKind};

/// Content-addressed identity of a cache entry.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct CacheKey([u8; 32]);

/// Content fingerprint of one task output.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct OutputFingerprint([u8; 32]);

macro_rules! impl_digest {
    ($type:ty) => {
        impl $type {
            pub const fn new(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }

            pub const fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }

            pub const fn into_bytes(self) -> [u8; 32] {
                self.0
            }
        }

        impl fmt::Debug for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for byte in &self.0[..8] {
                    write!(formatter, "{byte:02x}")?;
                }
                formatter.write_str("…")
            }
        }
    };
}

impl_digest!(CacheKey);
impl_digest!(OutputFingerprint);

/// Executor behavior when a cache implementation returns an error.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CacheMode {
    Disabled,
    #[default]
    BestEffort,
    Strict,
}

/// Inputs supplied to the task-specific cache-key implementation.
#[derive(Debug, Clone, Copy)]
pub struct CacheKeyInput<'a> {
    pub kind: TaskKind,
    pub schema: u32,
    pub dependency_fingerprints: &'a [OutputFingerprint],
}

/// A typed value restored from persistent cache storage.
#[derive(Debug)]
pub struct CachedOutput<T> {
    pub output: T,
    pub fingerprint: OutputFingerprint,
}

#[derive(Debug, Clone, Error)]
#[error("{message}")]
pub struct CacheError {
    message: String,
}

impl CacheError {
    pub fn new(message: impl Into<String>) -> Self {
        Self { message: message.into() }
    }
}

/// Typed cache adapter owned by a concrete task.
///
/// Implementations decide how task parameters, toolchain identity and relevant
/// environment values are incorporated into the key. The core always supplies
/// the kind, schema version and ordered dependency fingerprints.
pub trait TaskCache<T>: Send + Sync + 'static {
    fn key(&self, input: CacheKeyInput<'_>) -> Result<CacheKey, CacheError>;

    fn restore<'a>(
        &'a self,
        key: &'a CacheKey,
    ) -> BoxFuture<'a, Result<Option<CachedOutput<T>>, CacheError>>;

    fn store<'a>(
        &'a self,
        key: &'a CacheKey,
        output: &'a T,
    ) -> BoxFuture<'a, Result<OutputFingerprint, CacheError>>;
}
