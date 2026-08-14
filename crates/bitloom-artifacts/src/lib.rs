//! A strongly typed reference to a generated file or directory.

use std::fmt;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};

use bitloom_core::OutputFingerprint;

pub trait ArtifactKind: Send + Sync + 'static {}

/// A file or directory whose semantic kind is checked by the compiler.
pub struct Artifact<K: ArtifactKind> {
    path: PathBuf,
    fingerprint: OutputFingerprint,
    marker: PhantomData<fn() -> K>,
}

impl<K: ArtifactKind> Artifact<K> {
    pub fn new(path: impl Into<PathBuf>, fingerprint: OutputFingerprint) -> Self {
        Self { path: path.into(), fingerprint, marker: PhantomData }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn fingerprint(&self) -> OutputFingerprint {
        self.fingerprint
    }
}

impl<K: ArtifactKind> Clone for Artifact<K> {
    fn clone(&self) -> Self {
        Self::new(self.path.clone(), self.fingerprint)
    }
}

impl<K: ArtifactKind> fmt::Debug for Artifact<K> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Artifact")
            .field("kind", &std::any::type_name::<K>())
            .field("path", &self.path)
            .field("fingerprint", &self.fingerprint)
            .finish()
    }
}
