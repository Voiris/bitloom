//! Synthesis task and backend contract.

use std::borrow::Cow;
use std::error::Error;
use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use bitloom_artifacts::{Artifact, ArtifactKind};
use bitloom_core::{
    EffectKind, OutputFingerprint, Task, TaskCache, TaskContext, TaskKind, TaskRef,
};
use bitloom_snapshot::Snapshot;

#[derive(Debug)]
pub struct SynthesizedNetlist;

impl ArtifactKind for SynthesizedNetlist {}

#[derive(Debug, Clone)]
pub struct SynthesisOutput {
    pub netlist: Artifact<SynthesizedNetlist>,
}

impl SynthesisOutput {
    pub fn fingerprint(&self) -> OutputFingerprint {
        self.netlist.fingerprint()
    }
}

/// Concrete synthesis implementation, for example Yosys or a vendor tool.
pub trait SynthesisBackend: Send + Sync + 'static {
    type Error: Error + Send + Sync + 'static;

    fn synthesize(
        &self,
        context: TaskContext,
        snapshot: Arc<Snapshot>,
    ) -> impl Future<Output = Result<SynthesisOutput, Self::Error>> + Send;

    fn effect(&self) -> EffectKind {
        EffectKind::Reversible
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

pub struct SynthesisTask<D, B>
where
    D: Task<Output = Snapshot>,
    B: SynthesisBackend,
{
    snapshot: TaskRef<D>,
    backend: B,
    label: Arc<str>,
    cache: Option<Arc<dyn TaskCache<SynthesisOutput>>>,
}

impl<D, B> SynthesisTask<D, B>
where
    D: Task<Output = Snapshot>,
    B: SynthesisBackend,
{
    pub fn new(snapshot: TaskRef<D>, backend: B) -> Self {
        Self { snapshot, backend, label: Arc::from("Synthesis"), cache: None }
    }

    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_cache(mut self, cache: Arc<dyn TaskCache<SynthesisOutput>>) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }
}

impl<D, B> Task for SynthesisTask<D, B>
where
    D: Task<Output = Snapshot>,
    B: SynthesisBackend,
{
    type Output = SynthesisOutput;
    type Error = B::Error;
    type Dependencies = TaskRef<D>;

    const KIND: TaskKind = TaskKind::new("bitloom.synthesis");
    const CACHE_SCHEMA: u32 = 1;

    fn label(&self) -> Cow<'static, str> {
        Cow::Owned(self.label.to_string())
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.snapshot
    }

    fn effect(&self) -> EffectKind {
        self.backend.effect()
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        self.backend.output_claims()
    }

    fn cache(&self) -> Option<&dyn TaskCache<Self::Output>> {
        self.cache.as_deref()
    }

    fn output_fingerprint(&self, output: &Self::Output) -> Option<OutputFingerprint> {
        Some(output.fingerprint())
    }

    async fn run(
        &self,
        context: TaskContext,
        snapshot: Arc<Snapshot>,
    ) -> Result<Self::Output, Self::Error> {
        self.backend.synthesize(context, snapshot).await
    }
}
