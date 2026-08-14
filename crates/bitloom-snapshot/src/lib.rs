//! Source and constraint snapshot task.

use std::borrow::Cow;
use std::collections::HashSet;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use bitloom_artifacts::{Artifact, ArtifactKind};
use bitloom_core::{
    BoxFuture, EffectKind, OutputFingerprint, RollbackAction, RollbackContext, RollbackError, Task,
    TaskCache, TaskContext, TaskKind,
};
use blake3::Hasher;
use thiserror::Error;

#[derive(Debug)]
pub struct SnapshotDirectory;

impl ArtifactKind for SnapshotDirectory {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotFileKind {
    Source,
    Constraint,
}

#[derive(Debug, Clone)]
pub struct SnapshotFile {
    pub relative_path: PathBuf,
    pub kind: SnapshotFileKind,
    pub fingerprint: OutputFingerprint,
}

#[derive(Debug, Clone)]
pub struct Snapshot {
    pub root: Artifact<SnapshotDirectory>,
    pub files: Arc<[SnapshotFile]>,
    pub source_fingerprint: OutputFingerprint,
}

impl Snapshot {
    pub fn fingerprint(&self) -> OutputFingerprint {
        self.root.fingerprint()
    }

    pub fn source_fingerprint(&self) -> OutputFingerprint {
        self.source_fingerprint
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotInput {
    pub relative_path: PathBuf,
    pub kind: SnapshotFileKind,
}

impl SnapshotInput {
    pub fn source(path: impl Into<PathBuf>) -> Self {
        Self { relative_path: path.into(), kind: SnapshotFileKind::Source }
    }

    pub fn constraint(path: impl Into<PathBuf>) -> Self {
        Self { relative_path: path.into(), kind: SnapshotFileKind::Constraint }
    }
}

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum SnapshotConfigError {
    #[error("snapshot must contain at least one source file")]
    NoSources,
    #[error("snapshot input path must be a normalized project-relative file: {path:?}")]
    InvalidInputPath { path: PathBuf },
    #[error("snapshot input path appears more than once: {path:?}")]
    DuplicateInput { path: PathBuf },
    #[error("snapshot output path must have a final component: {path:?}")]
    InvalidOutputPath { path: PathBuf },
    #[error("snapshot input {input:?} points inside snapshot output {output:?}")]
    InputInsideOutput { input: PathBuf, output: PathBuf },
}

#[derive(Debug, Error)]
pub enum SnapshotError {
    #[error("snapshot output already exists: {path:?}")]
    OutputExists { path: PathBuf },
    #[error("snapshot input is not a regular file: {path:?}")]
    InputIsNotFile { path: PathBuf },
    #[error("failed to inspect {path:?}: {source}")]
    Inspect {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to read {path:?}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to create {path:?}: {source}")]
    Create {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to write {path:?}: {source}")]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to publish snapshot {from:?} as {to:?}: {source}")]
    Publish {
        from: PathBuf,
        to: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("failed to update snapshot progress: {message}")]
    Progress { message: String },
}

pub struct SnapshotTask {
    project_root: PathBuf,
    output_root: PathBuf,
    inputs: Arc<[SnapshotInput]>,
    cache: Option<Arc<dyn TaskCache<Snapshot>>>,
}

impl SnapshotTask {
    pub fn new(
        project_root: impl Into<PathBuf>,
        output_root: impl Into<PathBuf>,
        inputs: impl IntoIterator<Item = SnapshotInput>,
    ) -> Result<Self, SnapshotConfigError> {
        let project_root = project_root.into();
        let output_root = output_root.into();
        if output_root.file_name().is_none() {
            return Err(SnapshotConfigError::InvalidOutputPath { path: output_root });
        }

        let mut inputs = inputs.into_iter().collect::<Vec<_>>();
        let mut paths = HashSet::with_capacity(inputs.len());
        let mut has_sources = false;
        for input in &inputs {
            validate_relative_file(&input.relative_path)?;
            if !paths.insert(input.relative_path.clone()) {
                return Err(SnapshotConfigError::DuplicateInput {
                    path: input.relative_path.clone(),
                });
            }
            has_sources |= input.kind == SnapshotFileKind::Source;

            let source_path = project_root.join(&input.relative_path);
            if source_path.starts_with(&output_root) {
                return Err(SnapshotConfigError::InputInsideOutput {
                    input: source_path,
                    output: output_root,
                });
            }
        }
        if !has_sources {
            return Err(SnapshotConfigError::NoSources);
        }

        inputs.sort_by(|first, second| first.relative_path.cmp(&second.relative_path));
        Ok(Self { project_root, output_root, inputs: inputs.into(), cache: None })
    }

    pub fn with_cache(mut self, cache: Arc<dyn TaskCache<Snapshot>>) -> Self {
        self.cache = Some(cache);
        self
    }

    pub fn project_root(&self) -> &Path {
        &self.project_root
    }

    pub fn output_root(&self) -> &Path {
        &self.output_root
    }

    pub fn inputs(&self) -> &[SnapshotInput] {
        &self.inputs
    }

    fn staging_path(&self, task_id: bitloom_core::TaskInstanceId) -> PathBuf {
        let name = self
            .output_root
            .file_name()
            .expect("snapshot output name is validated during construction")
            .to_string_lossy();
        self.output_root
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(format!(".{name}.task-{}.tmp", task_id.get()))
    }
}

impl Task for SnapshotTask {
    type Output = Snapshot;
    type Error = SnapshotError;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("bitloom.snapshot");
    const CACHE_SCHEMA: u32 = 1;

    fn label(&self) -> Cow<'static, str> {
        "Load sources".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &()
    }

    fn effect(&self) -> EffectKind {
        EffectKind::Reversible
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        vec![self.output_root.clone()]
    }

    fn cache(&self) -> Option<&dyn TaskCache<Self::Output>> {
        self.cache.as_deref()
    }

    fn output_fingerprint(&self, output: &Self::Output) -> Option<OutputFingerprint> {
        Some(output.source_fingerprint())
    }

    async fn run(&self, context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
        if tokio::fs::try_exists(&self.output_root)
            .await
            .map_err(|source| SnapshotError::Inspect { path: self.output_root.clone(), source })?
        {
            return Err(SnapshotError::OutputExists { path: self.output_root.clone() });
        }

        let staging = self.staging_path(context.task_id());
        remove_path_if_exists(&staging)
            .await
            .map_err(|source| SnapshotError::Create { path: staging.clone(), source })?;
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(|source| SnapshotError::Create { path: staging.clone(), source })?;
        context.defer(RemovePath(staging.clone()));

        let total = self.inputs.len() as u64;
        context
            .set_progress(0, total)
            .map_err(|error| SnapshotError::Progress { message: error.to_string() })?;
        let mut snapshot_hasher = Hasher::new();
        let mut source_hasher = Hasher::new();
        let mut files = Vec::with_capacity(self.inputs.len());

        for (index, input) in self.inputs.iter().enumerate() {
            let source_path = self.project_root.join(&input.relative_path);
            let metadata = tokio::fs::metadata(&source_path)
                .await
                .map_err(|source| SnapshotError::Inspect { path: source_path.clone(), source })?;
            if !metadata.is_file() {
                return Err(SnapshotError::InputIsNotFile { path: source_path });
            }

            let contents = tokio::fs::read(&source_path)
                .await
                .map_err(|source| SnapshotError::Read { path: source_path.clone(), source })?;
            let destination = staging.join(&input.relative_path);
            if let Some(parent) = destination.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .map_err(|source| SnapshotError::Create { path: parent.to_owned(), source })?;
            }
            tokio::fs::write(&destination, &contents)
                .await
                .map_err(|source| SnapshotError::Write { path: destination, source })?;

            let normalized_path = input.relative_path.to_string_lossy().replace('\\', "/");
            let kind = match input.kind {
                SnapshotFileKind::Source => b's',
                SnapshotFileKind::Constraint => b'c',
            };
            let mut file_hasher = Hasher::new();
            file_hasher.update(normalized_path.as_bytes());
            file_hasher.update(&[0, kind, 0]);
            file_hasher.update(&contents);
            let file_hash = file_hasher.finalize();
            snapshot_hasher.update(file_hash.as_bytes());
            if input.kind == SnapshotFileKind::Source {
                source_hasher.update(file_hash.as_bytes());
            }
            files.push(SnapshotFile {
                relative_path: input.relative_path.clone(),
                kind: input.kind,
                fingerprint: OutputFingerprint::new(*file_hash.as_bytes()),
            });

            context
                .set_progress(index as u64 + 1, total)
                .map_err(|error| SnapshotError::Progress { message: error.to_string() })?;
        }

        context.defer(RemovePath(self.output_root.clone()));
        tokio::fs::rename(&staging, &self.output_root).await.map_err(|source| {
            SnapshotError::Publish { from: staging, to: self.output_root.clone(), source }
        })?;

        let fingerprint = OutputFingerprint::new(*snapshot_hasher.finalize().as_bytes());
        let source_fingerprint = OutputFingerprint::new(*source_hasher.finalize().as_bytes());
        Ok(Snapshot {
            root: Artifact::<SnapshotDirectory>::new(self.output_root.clone(), fingerprint),
            files: files.into(),
            source_fingerprint,
        })
    }
}

fn validate_relative_file(path: &Path) -> Result<(), SnapshotConfigError> {
    if path.as_os_str().is_empty()
        || !path.components().all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(SnapshotConfigError::InvalidInputPath { path: path.to_owned() });
    }
    Ok(())
}

struct RemovePath(PathBuf);

impl RollbackAction for RemovePath {
    fn rollback(
        self: Box<Self>,
        _context: RollbackContext,
    ) -> BoxFuture<'static, Result<(), RollbackError>> {
        Box::pin(async move {
            remove_path_if_exists(&self.0).await.map_err(|error| {
                RollbackError::new(format!("failed to remove {}: {error}", self.0.display()))
            })
        })
    }
}

async fn remove_path_if_exists(path: &Path) -> std::io::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use bitloom_core::{Executor, GraphBuilder, TaskPhase, task};

    use super::*;

    fn test_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("bitloom-snapshot-{name}-{nonce}"))
    }

    #[tokio::test]
    async fn copies_inputs_in_stable_order_and_fingerprints_them() {
        let root = test_root("copy");
        tokio::fs::create_dir_all(root.join("src")).await.unwrap();
        tokio::fs::create_dir_all(root.join("constraints")).await.unwrap();
        tokio::fs::write(root.join("src/top.sv"), b"module top; endmodule").await.unwrap();
        tokio::fs::write(root.join("constraints/target.cst"), b"IO_LOC \"clk\" 1;").await.unwrap();
        let output = root.join("build/snapshot");
        let snapshot = task(
            SnapshotTask::new(
                &root,
                &output,
                [
                    SnapshotInput::constraint("constraints/target.cst"),
                    SnapshotInput::source("src/top.sv"),
                ],
            )
            .unwrap(),
        );
        let graph = GraphBuilder::new().with_root(&snapshot).build().unwrap();

        Executor::default().run(&graph).await.unwrap();
        let result = snapshot.result().await.unwrap();

        assert!(result.root.path().join("src/top.sv").is_file());
        assert_eq!(result.files[0].relative_path, PathBuf::from("constraints/target.cst"));
        assert_eq!(result.files[1].relative_path, PathBuf::from("src/top.sv"));
        assert_eq!(snapshot.snapshot().phase, TaskPhase::Finished);

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn rejects_parent_paths_and_duplicate_destinations() {
        let root = PathBuf::from("project");
        assert!(matches!(
            SnapshotTask::new(
                &root,
                root.join("build/snapshot"),
                [SnapshotInput::source("../top.sv")]
            ),
            Err(SnapshotConfigError::InvalidInputPath { .. })
        ));
        assert!(matches!(
            SnapshotTask::new(
                &root,
                root.join("build/snapshot"),
                [SnapshotInput::source("src/top.sv"), SnapshotInput::source("src/top.sv")],
            ),
            Err(SnapshotConfigError::DuplicateInput { .. })
        ));
    }
}
