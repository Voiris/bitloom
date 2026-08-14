//! Bitstream packing task.

use std::borrow::Cow;
use std::error::Error;
use std::future::Future;
use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::Arc;

use bitloom_artifacts::{Artifact, ArtifactKind};
use bitloom_core::{
    EffectKind, OutputFingerprint, Task, TaskCache, TaskContext, TaskKind, TaskRef,
};
use bitloom_pnr::PnrOutput;

#[derive(Debug)]
pub struct PackedBitstream;

impl ArtifactKind for PackedBitstream {}

#[derive(Debug, Clone)]
pub struct PackOutput {
    pub bitstream: Artifact<PackedBitstream>,
}

impl PackOutput {
    pub fn fingerprint(&self) -> OutputFingerprint {
        self.bitstream.fingerprint()
    }
}

/// Tool-specific conversion of a routed design into one bitstream variant.
pub trait PackBackend<Routed>: Send + Sync + 'static
where
    Routed: Send + Sync + 'static,
{
    type Error: Error + Send + Sync + 'static;

    fn pack(
        &self,
        context: TaskContext,
        routed: Arc<PnrOutput<Routed>>,
    ) -> impl Future<Output = Result<PackOutput, Self::Error>> + Send;

    fn effect(&self) -> EffectKind {
        EffectKind::Reversible
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        Vec::new()
    }
}

pub struct PackTask<D, B, Routed>
where
    D: Task<Output = PnrOutput<Routed>>,
    B: PackBackend<Routed>,
    Routed: Send + Sync + 'static,
{
    routed: TaskRef<D>,
    backend: B,
    label: Arc<str>,
    cache: Option<Arc<dyn TaskCache<PackOutput>>>,
    routed_type: PhantomData<fn() -> Routed>,
}

impl<D, B, Routed> PackTask<D, B, Routed>
where
    D: Task<Output = PnrOutput<Routed>>,
    B: PackBackend<Routed>,
    Routed: Send + Sync + 'static,
{
    pub fn new(routed: TaskRef<D>, backend: B) -> Self {
        Self {
            routed,
            backend,
            label: Arc::from("Generate bitstream"),
            cache: None,
            routed_type: PhantomData,
        }
    }

    pub fn with_label(mut self, label: impl Into<Arc<str>>) -> Self {
        self.label = label.into();
        self
    }

    pub fn with_cache(mut self, cache: Arc<dyn TaskCache<PackOutput>>) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn backend(&self) -> &B {
        &self.backend
    }
}

impl<D, B, Routed> Task for PackTask<D, B, Routed>
where
    D: Task<Output = PnrOutput<Routed>>,
    B: PackBackend<Routed>,
    Routed: Send + Sync + 'static,
{
    type Output = PackOutput;
    type Error = B::Error;
    type Dependencies = TaskRef<D>;

    const KIND: TaskKind = TaskKind::new("bitloom.pack");
    const CACHE_SCHEMA: u32 = 1;

    fn label(&self) -> Cow<'static, str> {
        Cow::Owned(self.label.to_string())
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.routed
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
        routed: Arc<PnrOutput<Routed>>,
    ) -> Result<Self::Output, Self::Error> {
        self.backend.pack(context, routed).await
    }
}
