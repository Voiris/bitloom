//! Assembly of the ordinary Bitloom build graph.
//!
//! The graph contains one shared RTL snapshot and one complete synthesis, PNR
//! and pack branch per requested top module. It contains no firmware, flash or
//! BLPL stages.

use std::collections::HashSet;
use std::sync::Arc;

use bitloom_core::{GraphBuildError, GraphBuilder, TaskCache, TaskGraph, TaskRef, task};
use bitloom_pack::{PackBackend, PackOutput, PackTask};
use bitloom_pnr::{PnrBackend, PnrConfig, PnrOutput, PnrTask};
use bitloom_snapshot::SnapshotTask;
use bitloom_synthesis::{SynthesisBackend, SynthesisOutput, SynthesisTask};
use thiserror::Error;

pub type BuildSynthesisTask<Synthesis> = SynthesisTask<SnapshotTask, Synthesis>;
pub type BuildPnrTask<Synthesis, Pnr> = PnrTask<BuildSynthesisTask<Synthesis>, Pnr>;
pub type BuildPackTask<Synthesis, Pnr, Pack> =
    PackTask<BuildPnrTask<Synthesis, Pnr>, Pack, <Pnr as PnrBackend<SynthesisOutput>>::Artifact>;

pub struct DesignSpec<Synthesis, Pnr, Pack>
where
    Synthesis: SynthesisBackend,
    Pnr: PnrBackend<SynthesisOutput>,
    Pack: PackBackend<Pnr::Artifact>,
{
    pub name: Arc<str>,
    pub synthesis_backend: Synthesis,
    pub pnr_backend: Pnr,
    pub pnr_config: PnrConfig,
    pub pack_backend: Pack,
    synthesis_cache: Option<Arc<dyn TaskCache<SynthesisOutput>>>,
    pnr_cache: Option<Arc<dyn TaskCache<PnrOutput<Pnr::Artifact>>>>,
    pack_cache: Option<Arc<dyn TaskCache<PackOutput>>>,
}

impl<Synthesis, Pnr, Pack> DesignSpec<Synthesis, Pnr, Pack>
where
    Synthesis: SynthesisBackend,
    Pnr: PnrBackend<SynthesisOutput>,
    Pack: PackBackend<Pnr::Artifact>,
{
    pub fn new(
        name: impl Into<Arc<str>>,
        synthesis_backend: Synthesis,
        pnr_backend: Pnr,
        pnr_config: PnrConfig,
        pack_backend: Pack,
    ) -> Self {
        Self {
            name: name.into(),
            synthesis_backend,
            pnr_backend,
            pnr_config,
            pack_backend,
            synthesis_cache: None,
            pnr_cache: None,
            pack_cache: None,
        }
    }

    pub fn with_caches(
        mut self,
        synthesis: Arc<dyn TaskCache<SynthesisOutput>>,
        pnr: Arc<dyn TaskCache<PnrOutput<Pnr::Artifact>>>,
        pack: Arc<dyn TaskCache<PackOutput>>,
    ) -> Self {
        self.synthesis_cache = Some(synthesis);
        self.pnr_cache = Some(pnr);
        self.pack_cache = Some(pack);
        self
    }
}

pub struct DesignPipeline<Synthesis, Pnr, Pack>
where
    Synthesis: SynthesisBackend,
    Pnr: PnrBackend<SynthesisOutput>,
    Pack: PackBackend<Pnr::Artifact>,
{
    pub name: Arc<str>,
    pub synthesis: TaskRef<BuildSynthesisTask<Synthesis>>,
    pub pnr: TaskRef<BuildPnrTask<Synthesis, Pnr>>,
    pub pack: TaskRef<BuildPackTask<Synthesis, Pnr, Pack>>,
}

#[derive(Debug, Error)]
pub enum BuildPipelineError {
    #[error("ordinary build requires at least one top-module design")]
    NoDesigns,
    #[error("build design name cannot be empty")]
    EmptyDesignName,
    #[error("build design `{name}` appears more than once")]
    DuplicateDesignName { name: Arc<str> },
    #[error(transparent)]
    Graph(#[from] GraphBuildError),
}

pub struct BuildPipeline<Synthesis, Pnr, Pack>
where
    Synthesis: SynthesisBackend,
    Pnr: PnrBackend<SynthesisOutput>,
    Pack: PackBackend<Pnr::Artifact>,
{
    pub graph: TaskGraph,
    pub snapshot: TaskRef<SnapshotTask>,
    pub designs: Vec<DesignPipeline<Synthesis, Pnr, Pack>>,
}

/// Typed build nodes before a command chooses the final graph roots.
///
/// `build` makes every pack node a root. `run` can instead attach programming
/// tasks to the same nodes and build one larger graph.
pub struct BuildTasks<Synthesis, Pnr, Pack>
where
    Synthesis: SynthesisBackend,
    Pnr: PnrBackend<SynthesisOutput>,
    Pack: PackBackend<Pnr::Artifact>,
{
    pub snapshot: TaskRef<SnapshotTask>,
    pub designs: Vec<DesignPipeline<Synthesis, Pnr, Pack>>,
}

impl<Synthesis, Pnr, Pack> BuildTasks<Synthesis, Pnr, Pack>
where
    Synthesis: SynthesisBackend,
    Pnr: PnrBackend<SynthesisOutput>,
    Pack: PackBackend<Pnr::Artifact>,
{
    pub fn new(
        snapshot_task: SnapshotTask,
        designs: impl IntoIterator<Item = DesignSpec<Synthesis, Pnr, Pack>>,
    ) -> Result<Self, BuildPipelineError> {
        let designs = designs.into_iter().collect::<Vec<_>>();
        validate_designs(&designs)?;

        let snapshot = task(snapshot_task);
        let designs = designs
            .into_iter()
            .map(|design| {
                let name = design.name;
                let mut synthesis_task =
                    SynthesisTask::new(snapshot.clone(), design.synthesis_backend)
                        .with_label(format!("Synthesis ({name})"));
                if let Some(cache) = design.synthesis_cache {
                    synthesis_task = synthesis_task.with_cache(cache);
                }
                let synthesis = task(synthesis_task);
                let mut pnr_task =
                    PnrTask::new(synthesis.clone(), design.pnr_backend, design.pnr_config)
                        .with_label(format!("Place & Route ({name})"));
                if let Some(cache) = design.pnr_cache {
                    pnr_task = pnr_task.with_cache(cache);
                }
                let pnr = task(pnr_task);
                let mut pack_task = PackTask::new(pnr.clone(), design.pack_backend)
                    .with_label(format!("Generate bitstream ({name})"));
                if let Some(cache) = design.pack_cache {
                    pack_task = pack_task.with_cache(cache);
                }
                let pack = task(pack_task);
                DesignPipeline { name, synthesis, pnr, pack }
            })
            .collect();

        Ok(Self { snapshot, designs })
    }

    pub fn add_pack_roots(&self, builder: &mut GraphBuilder) {
        for design in &self.designs {
            builder.add_root(&design.pack);
        }
    }

    pub fn build(self) -> Result<BuildPipeline<Synthesis, Pnr, Pack>, BuildPipelineError> {
        let mut builder = GraphBuilder::new();
        self.add_pack_roots(&mut builder);
        let graph = builder.build()?;
        Ok(BuildPipeline { graph, snapshot: self.snapshot, designs: self.designs })
    }
}

impl<Synthesis, Pnr, Pack> BuildPipeline<Synthesis, Pnr, Pack>
where
    Synthesis: SynthesisBackend,
    Pnr: PnrBackend<SynthesisOutput>,
    Pack: PackBackend<Pnr::Artifact>,
{
    pub fn new(
        snapshot_task: SnapshotTask,
        designs: impl IntoIterator<Item = DesignSpec<Synthesis, Pnr, Pack>>,
    ) -> Result<Self, BuildPipelineError> {
        BuildTasks::new(snapshot_task, designs)?.build()
    }
}

fn validate_designs<Synthesis, Pnr, Pack>(
    designs: &[DesignSpec<Synthesis, Pnr, Pack>],
) -> Result<(), BuildPipelineError>
where
    Synthesis: SynthesisBackend,
    Pnr: PnrBackend<SynthesisOutput>,
    Pack: PackBackend<Pnr::Artifact>,
{
    if designs.is_empty() {
        return Err(BuildPipelineError::NoDesigns);
    }

    let mut names = HashSet::with_capacity(designs.len());
    for design in designs {
        if design.name.trim().is_empty() {
            return Err(BuildPipelineError::EmptyDesignName);
        }
        if !names.insert(design.name.clone()) {
            return Err(BuildPipelineError::DuplicateDesignName { name: design.name.clone() });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::convert::Infallible;
    use std::num::NonZeroUsize;
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use bitloom_artifacts::{Artifact, ArtifactKind};
    use bitloom_cache::{CacheParameters, PackCache, PnrCache, SnapshotCache, SynthesisCache};
    use bitloom_core::{CacheMode, Executor, ExecutorOptions, OutputFingerprint, TaskContext};
    use bitloom_pack::{PackBackend, PackOutput, PackedBitstream};
    use bitloom_pnr::{PnrRun, PnrRunContext, PnrScore, PnrStatistics};
    use bitloom_snapshot::{Snapshot, SnapshotInput};
    use bitloom_synthesis::SynthesizedNetlist;

    use super::*;

    #[derive(Debug)]
    struct MockRoutedNetlist;

    impl ArtifactKind for MockRoutedNetlist {}

    struct MockSynthesis {
        executions: Arc<AtomicUsize>,
        output: PathBuf,
    }

    impl SynthesisBackend for MockSynthesis {
        type Error = Infallible;

        async fn synthesize(
            &self,
            _context: TaskContext,
            _snapshot: Arc<Snapshot>,
        ) -> Result<SynthesisOutput, Self::Error> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            if let Some(parent) = self.output.parent() {
                tokio::fs::create_dir_all(parent).await.unwrap();
            }
            tokio::fs::write(&self.output, b"synthesized").await.unwrap();
            Ok(SynthesisOutput {
                netlist: Artifact::<SynthesizedNetlist>::new(
                    self.output.clone(),
                    OutputFingerprint::new(*blake3::hash(b"synthesized").as_bytes()),
                ),
            })
        }

        fn output_claims(&self) -> Vec<PathBuf> {
            vec![self.output.clone()]
        }
    }

    struct MockPnr {
        top: Arc<str>,
        executions: Arc<AtomicUsize>,
        output_dir: PathBuf,
    }

    impl PnrBackend<SynthesisOutput> for MockPnr {
        type Artifact = Artifact<MockRoutedNetlist>;
        type Error = Infallible;

        async fn run(
            &self,
            _context: PnrRunContext,
            _input: Arc<SynthesisOutput>,
            seed: u64,
        ) -> Result<PnrRun<Self::Artifact>, Self::Error> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            tokio::fs::create_dir_all(&self.output_dir).await.unwrap();
            let output = self.output_dir.join(format!("candidate-{seed}.json"));
            tokio::fs::write(&output, format!("routed-{}-{seed}", self.top)).await.unwrap();
            let fingerprint = OutputFingerprint::new(
                *blake3::hash(&tokio::fs::read(&output).await.unwrap()).as_bytes(),
            );
            Ok(PnrRun {
                score: PnrScore::new(seed as f64).expect("test seeds produce finite scores"),
                artifact: Artifact::<MockRoutedNetlist>::new(output, fingerprint),
                statistics: PnrStatistics::default(),
            })
        }

        fn output_fingerprint(
            &self,
            candidate: &bitloom_pnr::PnrCandidate<Self::Artifact>,
        ) -> Option<OutputFingerprint> {
            Some(candidate.artifact.fingerprint())
        }
    }

    struct MockPack {
        output: PathBuf,
        executions: Arc<AtomicUsize>,
    }

    impl PackBackend<Artifact<MockRoutedNetlist>> for MockPack {
        type Error = Infallible;

        async fn pack(
            &self,
            _context: TaskContext,
            _routed: Arc<bitloom_pnr::PnrOutput<Artifact<MockRoutedNetlist>>>,
        ) -> Result<PackOutput, Self::Error> {
            self.executions.fetch_add(1, Ordering::SeqCst);
            if let Some(parent) = self.output.parent() {
                tokio::fs::create_dir_all(parent).await.unwrap();
            }
            tokio::fs::write(&self.output, b"packed").await.unwrap();
            Ok(PackOutput {
                bitstream: Artifact::<PackedBitstream>::new(
                    self.output.clone(),
                    OutputFingerprint::new(*blake3::hash(b"packed").as_bytes()),
                ),
            })
        }

        fn output_claims(&self) -> Vec<PathBuf> {
            vec![self.output.clone()]
        }
    }

    fn test_root() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("bitloom-build-{nonce}"))
    }

    fn strict_executor() -> Executor {
        Executor::new(ExecutorOptions {
            max_parallel_tasks: NonZeroUsize::new(4).unwrap(),
            cache_mode: CacheMode::Strict,
        })
    }

    async fn cached_pipeline(
        root: &Path,
        synthesis_executions: Arc<AtomicUsize>,
        pnr_executions: Arc<AtomicUsize>,
        pack_executions: Arc<AtomicUsize>,
    ) -> BuildPipeline<MockSynthesis, MockPnr, MockPack> {
        let cache_root = root.join("build/cache");
        let snapshot_root = root.join("build/snapshot");
        let snapshot = SnapshotTask::new(
            root,
            &snapshot_root,
            [SnapshotInput::source("src/top.sv"), SnapshotInput::constraint("constraints/top.cst")],
        )
        .unwrap();
        let snapshot_cache = SnapshotCache::new(
            &cache_root,
            snapshot.project_root(),
            snapshot.output_root(),
            snapshot.inputs(),
        )
        .await
        .unwrap();
        let snapshot = snapshot.with_cache(Arc::new(snapshot_cache));
        let synthesis_output = root.join("build/design/synthesis.json");
        let pnr_root = root.join("build/design/pnr");
        let pack_output = root.join("build/bitstreams/top.fs");
        let synthesis_parameters = CacheParameters::new("mock-synthesis");
        let mut pnr_parameters = CacheParameters::new("mock-pnr");
        pnr_parameters
            .add_file("constraint", root, PathBuf::from("constraints/top.cst").as_path())
            .await
            .unwrap();
        let pack_parameters = CacheParameters::new("mock-pack");
        let design = DesignSpec::new(
            "top",
            MockSynthesis { executions: synthesis_executions, output: synthesis_output.clone() },
            MockPnr {
                top: Arc::from("top"),
                executions: pnr_executions,
                output_dir: pnr_root.clone(),
            },
            PnrConfig::new([1, 2], NonZeroUsize::new(2).unwrap()).unwrap(),
            MockPack { output: pack_output.clone(), executions: pack_executions },
        )
        .with_caches(
            Arc::new(SynthesisCache::new(&cache_root, synthesis_output, synthesis_parameters)),
            Arc::new(PnrCache::<MockRoutedNetlist>::new(
                &cache_root,
                pnr_root.join("cached-selected.json"),
                pnr_parameters,
            )),
            Arc::new(PackCache::new(&cache_root, pack_output, pack_parameters)),
        );
        BuildPipeline::new(snapshot, [design]).unwrap()
    }

    async fn clear_cached_build_outputs(root: &Path) {
        for path in
            [root.join("build/snapshot"), root.join("build/design"), root.join("build/bitstreams")]
        {
            match tokio::fs::remove_dir_all(&path).await {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("failed to remove {}: {error}", path.display()),
            }
        }
    }

    #[tokio::test]
    async fn reuses_cached_pipeline_and_invalidates_only_constraint_consumers() {
        let root = test_root();
        tokio::fs::create_dir_all(root.join("src")).await.unwrap();
        tokio::fs::create_dir_all(root.join("constraints")).await.unwrap();
        tokio::fs::write(root.join("src/top.sv"), b"module top; endmodule").await.unwrap();
        tokio::fs::write(root.join("constraints/top.cst"), b"IO_LOC \"clk\" 1;").await.unwrap();
        let synthesis_executions = Arc::new(AtomicUsize::new(0));
        let pnr_executions = Arc::new(AtomicUsize::new(0));
        let pack_executions = Arc::new(AtomicUsize::new(0));

        let first = cached_pipeline(
            &root,
            synthesis_executions.clone(),
            pnr_executions.clone(),
            pack_executions.clone(),
        )
        .await;
        let report = strict_executor().run(&first.graph).await.unwrap();
        assert_eq!(report.cache_hits, 0);
        assert_eq!(synthesis_executions.load(Ordering::SeqCst), 1);
        assert_eq!(pnr_executions.load(Ordering::SeqCst), 2);
        assert_eq!(pack_executions.load(Ordering::SeqCst), 1);

        clear_cached_build_outputs(&root).await;
        let second = cached_pipeline(
            &root,
            synthesis_executions.clone(),
            pnr_executions.clone(),
            pack_executions.clone(),
        )
        .await;
        let report = strict_executor().run(&second.graph).await.unwrap();
        assert_eq!(report.cache_hits, 4);
        assert_eq!(synthesis_executions.load(Ordering::SeqCst), 1);
        assert_eq!(pnr_executions.load(Ordering::SeqCst), 2);
        assert_eq!(pack_executions.load(Ordering::SeqCst), 1);

        clear_cached_build_outputs(&root).await;
        tokio::fs::write(root.join("constraints/top.cst"), b"IO_LOC \"clk\" 2;").await.unwrap();
        let third = cached_pipeline(
            &root,
            synthesis_executions.clone(),
            pnr_executions.clone(),
            pack_executions.clone(),
        )
        .await;
        let report = strict_executor().run(&third.graph).await.unwrap();
        assert_eq!(report.cache_hits, 2);
        assert_eq!(synthesis_executions.load(Ordering::SeqCst), 1);
        assert_eq!(pnr_executions.load(Ordering::SeqCst), 4);
        assert_eq!(pack_executions.load(Ordering::SeqCst), 1);

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn builds_each_top_module_as_an_independent_branch() {
        let root = test_root();
        tokio::fs::create_dir_all(root.join("src")).await.unwrap();
        tokio::fs::write(
            root.join("src/tops.sv"),
            b"module control_top; endmodule\nmodule recovery_top; endmodule",
        )
        .await
        .unwrap();
        let synthesis_executions = Arc::new(AtomicUsize::new(0));
        let pnr_executions = Arc::new(AtomicUsize::new(0));
        let pack_executions = Arc::new(AtomicUsize::new(0));
        let snapshot = SnapshotTask::new(
            &root,
            root.join("build/snapshot"),
            [SnapshotInput::source("src/tops.sv")],
        )
        .unwrap();
        let pipeline = BuildPipeline::new(
            snapshot,
            [
                DesignSpec::new(
                    "control_top",
                    MockSynthesis {
                        executions: synthesis_executions.clone(),
                        output: root.join("build/control/netlist.json"),
                    },
                    MockPnr {
                        top: Arc::from("control_top"),
                        executions: pnr_executions.clone(),
                        output_dir: root.join("build/control/pnr"),
                    },
                    PnrConfig::new([10, 30, 20], NonZeroUsize::new(2).unwrap()).unwrap(),
                    MockPack {
                        output: root.join("build/control/bitstream.fs"),
                        executions: pack_executions.clone(),
                    },
                ),
                DesignSpec::new(
                    "recovery_top",
                    MockSynthesis {
                        executions: synthesis_executions.clone(),
                        output: root.join("build/recovery/netlist.json"),
                    },
                    MockPnr {
                        top: Arc::from("recovery_top"),
                        executions: pnr_executions.clone(),
                        output_dir: root.join("build/recovery/pnr"),
                    },
                    PnrConfig::new([7, 5], NonZeroUsize::new(2).unwrap()).unwrap(),
                    MockPack {
                        output: root.join("build/recovery/bitstream.fs"),
                        executions: pack_executions.clone(),
                    },
                ),
            ],
        )
        .unwrap();

        assert_eq!(pipeline.graph.len(), 7);
        assert_eq!(pipeline.designs.len(), 2);
        assert_ne!(pipeline.designs[0].synthesis.id(), pipeline.designs[1].synthesis.id());
        assert_ne!(pipeline.designs[0].pnr.id(), pipeline.designs[1].pnr.id());
        assert_ne!(pipeline.designs[0].pack.id(), pipeline.designs[1].pack.id());
        Executor::default().run(&pipeline.graph).await.unwrap();

        assert_eq!(pipeline.designs[0].pnr.result().await.unwrap().selected.seed, 30);
        assert_eq!(pipeline.designs[1].pnr.result().await.unwrap().selected.seed, 7);
        assert_eq!(
            pipeline.designs[0].pack.result().await.unwrap().bitstream.path(),
            root.join("build/control/bitstream.fs")
        );
        assert_eq!(
            pipeline.designs[1].pack.result().await.unwrap().bitstream.path(),
            root.join("build/recovery/bitstream.fs")
        );
        assert_eq!(
            pipeline.designs[0].synthesis.snapshot().label.as_ref(),
            "Synthesis (control_top)"
        );
        assert_eq!(
            pipeline.designs[1].pack.snapshot().label.as_ref(),
            "Generate bitstream (recovery_top)"
        );
        assert_eq!(synthesis_executions.load(Ordering::SeqCst), 2);
        assert_eq!(pnr_executions.load(Ordering::SeqCst), 5);
        assert_eq!(pack_executions.load(Ordering::SeqCst), 2);

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn rejects_build_without_top_modules() {
        let root = PathBuf::from("project");
        let snapshot = SnapshotTask::new(
            &root,
            root.join("build/snapshot"),
            [SnapshotInput::source("src/top.sv")],
        )
        .unwrap();

        let result = BuildPipeline::<MockSynthesis, MockPnr, MockPack>::new(snapshot, []);

        assert!(matches!(result, Err(BuildPipelineError::NoDesigns)));
    }
}
