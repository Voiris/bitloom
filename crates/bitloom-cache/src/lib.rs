//! Persistent, typed cache adapters for Bitloom build tasks.

use std::collections::{BTreeMap, HashSet};
use std::io::ErrorKind;
use std::marker::PhantomData;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use bitloom_artifacts::{Artifact, ArtifactKind};
use bitloom_core::{
    BoxFuture, CacheError, CacheKey, CacheKeyInput, CachedOutput, OutputFingerprint, TaskCache,
};
use bitloom_pack::{PackOutput, PackedBitstream};
use bitloom_pnr::{
    ClockFrequency, PnrCandidate, PnrOutput, PnrRunFailure, PnrScore, PnrStatistics, ResourceUsage,
};
use bitloom_snapshot::{
    Snapshot, SnapshotDirectory, SnapshotFile, SnapshotFileKind, SnapshotInput,
};
use bitloom_synthesis::{SynthesisOutput, SynthesizedNetlist};
use blake3::Hasher;
use serde::{Deserialize, Serialize};

const ENTRY_FORMAT: u32 = 1;
const METADATA_FILE: &str = "metadata.json";
const ARTIFACT_NAME: &str = "artifact";
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

pub struct CacheParameters {
    hasher: Hasher,
}

impl CacheParameters {
    pub fn new(domain: &str) -> Self {
        let mut hasher = Hasher::new();
        add_field(&mut hasher, "domain", domain.as_bytes());
        Self { hasher }
    }

    pub fn add_bytes(&mut self, name: &str, value: &[u8]) -> &mut Self {
        add_field(&mut self.hasher, name, value);
        self
    }

    pub fn add_str(&mut self, name: &str, value: &str) -> &mut Self {
        self.add_bytes(name, value.as_bytes())
    }

    pub fn add_u64(&mut self, name: &str, value: u64) -> &mut Self {
        self.add_bytes(name, &value.to_le_bytes())
    }

    pub fn add_usize(&mut self, name: &str, value: usize) -> &mut Self {
        self.add_u64(name, value as u64)
    }

    pub fn add_f64(&mut self, name: &str, value: f64) -> &mut Self {
        self.add_u64(name, value.to_bits())
    }

    pub fn add_fingerprint(&mut self, name: &str, value: OutputFingerprint) -> &mut Self {
        self.add_bytes(name, value.as_bytes())
    }

    pub fn add_path(&mut self, name: &str, value: &Path) -> &mut Self {
        self.add_str(name, &normalized_path(value))
    }

    pub async fn add_file(
        &mut self,
        name: &str,
        root: &Path,
        relative_path: &Path,
    ) -> Result<&mut Self, CacheError> {
        validate_relative_path(relative_path)?;
        self.add_path(name, relative_path);
        let path = root.join(relative_path);
        let contents = tokio::fs::read(&path)
            .await
            .map_err(|error| cache_io("read cache-key input", &path, error))?;
        self.add_bytes(name, &contents);
        Ok(self)
    }

    pub fn finish(self) -> [u8; 32] {
        *self.hasher.finalize().as_bytes()
    }
}

#[derive(Clone)]
struct CacheLocation {
    root: PathBuf,
    namespace: &'static str,
    parameters: [u8; 32],
}

impl CacheLocation {
    fn new(root: impl Into<PathBuf>, namespace: &'static str, parameters: CacheParameters) -> Self {
        Self { root: root.into(), namespace, parameters: parameters.finish() }
    }

    fn key(&self, input: CacheKeyInput<'_>) -> CacheKey {
        let mut hasher = Hasher::new();
        add_field(&mut hasher, "format", &ENTRY_FORMAT.to_le_bytes());
        add_field(&mut hasher, "kind", input.kind.as_str().as_bytes());
        add_field(&mut hasher, "schema", &input.schema.to_le_bytes());
        add_field(&mut hasher, "parameters", &self.parameters);
        for fingerprint in input.dependency_fingerprints {
            add_field(&mut hasher, "dependency", fingerprint.as_bytes());
        }
        CacheKey::new(*hasher.finalize().as_bytes())
    }

    fn entry(&self, key: &CacheKey) -> PathBuf {
        self.root.join(self.namespace).join(hex(key.as_bytes()))
    }
}

pub struct SnapshotCache {
    location: CacheLocation,
    output_root: PathBuf,
}

impl SnapshotCache {
    pub async fn new(
        cache_root: impl Into<PathBuf>,
        project_root: &Path,
        output_root: impl Into<PathBuf>,
        inputs: &[SnapshotInput],
    ) -> Result<Self, CacheError> {
        let mut parameters = CacheParameters::new("snapshot");
        for input in inputs {
            parameters
                .add_str(
                    "kind",
                    match input.kind {
                        SnapshotFileKind::Source => "source",
                        SnapshotFileKind::Constraint => "constraint",
                    },
                )
                .add_file("input", project_root, &input.relative_path)
                .await?;
        }
        Ok(Self {
            location: CacheLocation::new(cache_root, "snapshot", parameters),
            output_root: output_root.into(),
        })
    }
}

impl TaskCache<Snapshot> for SnapshotCache {
    fn key(&self, input: CacheKeyInput<'_>) -> Result<CacheKey, CacheError> {
        Ok(self.location.key(input))
    }

    fn restore<'a>(
        &'a self,
        key: &'a CacheKey,
    ) -> BoxFuture<'a, Result<Option<CachedOutput<Snapshot>>, CacheError>> {
        Box::pin(async move {
            let entry = self.location.entry(key);
            let Some(manifest) = read_metadata::<SnapshotManifest>(&entry).await? else {
                return Ok(None);
            };
            if manifest.format != ENTRY_FORMAT {
                invalidate_entry(&entry).await;
                return Ok(None);
            }
            let Some(snapshot) =
                validate_snapshot_entry(&entry, &self.output_root, manifest).await?
            else {
                invalidate_entry(&entry).await;
                return Ok(None);
            };
            restore_directory(&entry.join(ARTIFACT_NAME), &self.output_root).await?;
            let fingerprint = snapshot.source_fingerprint();
            Ok(Some(CachedOutput { output: snapshot, fingerprint }))
        })
    }

    fn store<'a>(
        &'a self,
        key: &'a CacheKey,
        output: &'a Snapshot,
    ) -> BoxFuture<'a, Result<OutputFingerprint, CacheError>> {
        Box::pin(async move {
            let entry = self.location.entry(key);
            let stage = prepare_entry_stage(&entry).await?;
            let result = store_snapshot(&stage, output).await;
            match result {
                Ok(()) => publish_entry(&stage, &entry).await?,
                Err(error) => {
                    let _ = remove_path(&stage).await;
                    return Err(error);
                }
            }
            Ok(output.source_fingerprint())
        })
    }
}

pub struct SynthesisCache {
    location: CacheLocation,
    output: PathBuf,
}

impl SynthesisCache {
    pub fn new(
        cache_root: impl Into<PathBuf>,
        output: impl Into<PathBuf>,
        parameters: CacheParameters,
    ) -> Self {
        Self {
            location: CacheLocation::new(cache_root, "synthesis", parameters),
            output: output.into(),
        }
    }
}

impl TaskCache<SynthesisOutput> for SynthesisCache {
    fn key(&self, input: CacheKeyInput<'_>) -> Result<CacheKey, CacheError> {
        Ok(self.location.key(input))
    }

    fn restore<'a>(
        &'a self,
        key: &'a CacheKey,
    ) -> BoxFuture<'a, Result<Option<CachedOutput<SynthesisOutput>>, CacheError>> {
        Box::pin(async move {
            let entry = self.location.entry(key);
            let Some(fingerprint) = restore_file_entry(&entry, &self.output).await? else {
                return Ok(None);
            };
            Ok(Some(CachedOutput {
                output: SynthesisOutput {
                    netlist: Artifact::<SynthesizedNetlist>::new(&self.output, fingerprint),
                },
                fingerprint,
            }))
        })
    }

    fn store<'a>(
        &'a self,
        key: &'a CacheKey,
        output: &'a SynthesisOutput,
    ) -> BoxFuture<'a, Result<OutputFingerprint, CacheError>> {
        Box::pin(async move {
            store_file_entry(&self.location.entry(key), output.netlist.path(), output.fingerprint())
                .await
        })
    }
}

pub struct PnrCache<K: ArtifactKind> {
    location: CacheLocation,
    output: PathBuf,
    marker: PhantomData<fn() -> K>,
}

impl<K: ArtifactKind> PnrCache<K> {
    pub fn new(
        cache_root: impl Into<PathBuf>,
        output: impl Into<PathBuf>,
        parameters: CacheParameters,
    ) -> Self {
        Self {
            location: CacheLocation::new(cache_root, "pnr", parameters),
            output: output.into(),
            marker: PhantomData,
        }
    }
}

impl<K: ArtifactKind> TaskCache<PnrOutput<Artifact<K>>> for PnrCache<K> {
    fn key(&self, input: CacheKeyInput<'_>) -> Result<CacheKey, CacheError> {
        Ok(self.location.key(input))
    }

    fn restore<'a>(
        &'a self,
        key: &'a CacheKey,
    ) -> BoxFuture<'a, Result<Option<CachedOutput<PnrOutput<Artifact<K>>>>, CacheError>> {
        Box::pin(async move {
            let entry = self.location.entry(key);
            let Some(metadata) = read_metadata::<PnrMetadata>(&entry).await? else {
                return Ok(None);
            };
            if metadata.format != ENTRY_FORMAT {
                invalidate_entry(&entry).await;
                return Ok(None);
            }
            let fingerprint = OutputFingerprint::new(metadata.fingerprint);
            if !restore_verified_file(&entry, &self.output, fingerprint).await? {
                invalidate_entry(&entry).await;
                return Ok(None);
            }
            let Ok(score) = PnrScore::new(metadata.score) else {
                invalidate_entry(&entry).await;
                return Ok(None);
            };
            let statistics = PnrStatistics {
                resources: metadata
                    .resources
                    .into_iter()
                    .map(|(name, usage)| {
                        (name, ResourceUsage { used: usage.used, available: usage.available })
                    })
                    .collect(),
                clocks: metadata
                    .clocks
                    .into_iter()
                    .map(|(name, clock)| {
                        (
                            name,
                            ClockFrequency {
                                achieved_mhz: clock.achieved_mhz,
                                constraint_mhz: clock.constraint_mhz,
                            },
                        )
                    })
                    .collect(),
            };
            let failed_runs = metadata
                .failed_runs
                .into_iter()
                .map(|failure| PnrRunFailure {
                    run_index: failure.run_index,
                    seed: failure.seed,
                    message: failure.message,
                })
                .collect();
            let output = PnrOutput {
                selected: PnrCandidate {
                    run_index: metadata.run_index,
                    seed: metadata.seed,
                    score,
                    artifact: Artifact::<K>::new(&self.output, fingerprint),
                    statistics,
                },
                attempts: metadata.attempts,
                successful_runs: metadata.successful_runs,
                failed_runs,
            };
            Ok(Some(CachedOutput { output, fingerprint }))
        })
    }

    fn store<'a>(
        &'a self,
        key: &'a CacheKey,
        output: &'a PnrOutput<Artifact<K>>,
    ) -> BoxFuture<'a, Result<OutputFingerprint, CacheError>> {
        Box::pin(async move {
            let fingerprint = output.selected.artifact.fingerprint();
            let metadata = PnrMetadata {
                format: ENTRY_FORMAT,
                fingerprint: fingerprint.into_bytes(),
                run_index: output.selected.run_index,
                seed: output.selected.seed,
                score: output.selected.score.get(),
                attempts: output.attempts,
                successful_runs: output.successful_runs,
                failed_runs: output
                    .failed_runs
                    .iter()
                    .map(|failure| FailureMetadata {
                        run_index: failure.run_index,
                        seed: failure.seed,
                        message: failure.message.clone(),
                    })
                    .collect(),
                resources: output
                    .selected
                    .statistics
                    .resources
                    .iter()
                    .map(|(name, usage)| {
                        (
                            name.clone(),
                            ResourceMetadata { used: usage.used, available: usage.available },
                        )
                    })
                    .collect(),
                clocks: output
                    .selected
                    .statistics
                    .clocks
                    .iter()
                    .map(|(name, clock)| {
                        (
                            name.clone(),
                            ClockMetadata {
                                achieved_mhz: clock.achieved_mhz,
                                constraint_mhz: clock.constraint_mhz,
                            },
                        )
                    })
                    .collect(),
            };
            store_file_with_metadata(
                &self.location.entry(key),
                output.selected.artifact.path(),
                fingerprint,
                &metadata,
            )
            .await
        })
    }
}

pub struct PackCache {
    location: CacheLocation,
    output: PathBuf,
}

impl PackCache {
    pub fn new(
        cache_root: impl Into<PathBuf>,
        output: impl Into<PathBuf>,
        parameters: CacheParameters,
    ) -> Self {
        Self { location: CacheLocation::new(cache_root, "pack", parameters), output: output.into() }
    }
}

impl TaskCache<PackOutput> for PackCache {
    fn key(&self, input: CacheKeyInput<'_>) -> Result<CacheKey, CacheError> {
        Ok(self.location.key(input))
    }

    fn restore<'a>(
        &'a self,
        key: &'a CacheKey,
    ) -> BoxFuture<'a, Result<Option<CachedOutput<PackOutput>>, CacheError>> {
        Box::pin(async move {
            let entry = self.location.entry(key);
            let Some(fingerprint) = restore_file_entry(&entry, &self.output).await? else {
                return Ok(None);
            };
            Ok(Some(CachedOutput {
                output: PackOutput {
                    bitstream: Artifact::<PackedBitstream>::new(&self.output, fingerprint),
                },
                fingerprint,
            }))
        })
    }

    fn store<'a>(
        &'a self,
        key: &'a CacheKey,
        output: &'a PackOutput,
    ) -> BoxFuture<'a, Result<OutputFingerprint, CacheError>> {
        Box::pin(async move {
            store_file_entry(
                &self.location.entry(key),
                output.bitstream.path(),
                output.fingerprint(),
            )
            .await
        })
    }
}

#[derive(Serialize, Deserialize)]
struct FileMetadata {
    format: u32,
    fingerprint: [u8; 32],
}

#[derive(Serialize, Deserialize)]
struct SnapshotManifest {
    format: u32,
    fingerprint: [u8; 32],
    source_fingerprint: [u8; 32],
    files: Vec<SnapshotFileMetadata>,
}

#[derive(Serialize, Deserialize)]
struct SnapshotFileMetadata {
    path: String,
    kind: FileKindMetadata,
    fingerprint: [u8; 32],
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum FileKindMetadata {
    Source,
    Constraint,
}

#[derive(Serialize, Deserialize)]
struct PnrMetadata {
    format: u32,
    fingerprint: [u8; 32],
    run_index: usize,
    seed: u64,
    score: f64,
    attempts: usize,
    successful_runs: usize,
    failed_runs: Vec<FailureMetadata>,
    resources: BTreeMap<String, ResourceMetadata>,
    clocks: BTreeMap<String, ClockMetadata>,
}

#[derive(Serialize, Deserialize)]
struct FailureMetadata {
    run_index: usize,
    seed: u64,
    message: String,
}

#[derive(Serialize, Deserialize)]
struct ResourceMetadata {
    used: u64,
    available: u64,
}

#[derive(Serialize, Deserialize)]
struct ClockMetadata {
    achieved_mhz: f64,
    constraint_mhz: f64,
}

async fn restore_file_entry(
    entry: &Path,
    output: &Path,
) -> Result<Option<OutputFingerprint>, CacheError> {
    let Some(metadata) = read_metadata::<FileMetadata>(entry).await? else {
        return Ok(None);
    };
    if metadata.format != ENTRY_FORMAT {
        invalidate_entry(entry).await;
        return Ok(None);
    }
    let fingerprint = OutputFingerprint::new(metadata.fingerprint);
    if !restore_verified_file(entry, output, fingerprint).await? {
        invalidate_entry(entry).await;
        return Ok(None);
    }
    Ok(Some(fingerprint))
}

async fn restore_verified_file(
    entry: &Path,
    output: &Path,
    expected: OutputFingerprint,
) -> Result<bool, CacheError> {
    let artifact = entry.join(ARTIFACT_NAME);
    let Some(actual) = hash_file_if_present(&artifact).await? else {
        return Ok(false);
    };
    if actual != expected {
        return Ok(false);
    }
    restore_file(&artifact, output).await?;
    Ok(true)
}

async fn store_file_entry(
    entry: &Path,
    source: &Path,
    fingerprint: OutputFingerprint,
) -> Result<OutputFingerprint, CacheError> {
    let metadata = FileMetadata { format: ENTRY_FORMAT, fingerprint: fingerprint.into_bytes() };
    store_file_with_metadata(entry, source, fingerprint, &metadata).await
}

async fn store_file_with_metadata<T: Serialize>(
    entry: &Path,
    source: &Path,
    fingerprint: OutputFingerprint,
    metadata: &T,
) -> Result<OutputFingerprint, CacheError> {
    let actual = hash_file(source).await?;
    if actual != fingerprint {
        return Err(CacheError::new(format!(
            "artifact fingerprint changed before caching: {}",
            source.display()
        )));
    }
    let stage = prepare_entry_stage(entry).await?;
    let result = async {
        tokio::fs::copy(source, stage.join(ARTIFACT_NAME))
            .await
            .map_err(|error| cache_io("copy artifact into cache", source, error))?;
        write_metadata(&stage, metadata).await
    }
    .await;
    match result {
        Ok(()) => publish_entry(&stage, entry).await?,
        Err(error) => {
            let _ = remove_path(&stage).await;
            return Err(error);
        }
    }
    Ok(fingerprint)
}

async fn store_snapshot(stage: &Path, snapshot: &Snapshot) -> Result<(), CacheError> {
    let artifact = stage.join(ARTIFACT_NAME);
    tokio::fs::create_dir_all(&artifact)
        .await
        .map_err(|error| cache_io("create snapshot cache", &artifact, error))?;
    let mut all_hasher = Hasher::new();
    let mut source_hasher = Hasher::new();
    let mut files = Vec::with_capacity(snapshot.files.len());
    let mut seen = HashSet::with_capacity(snapshot.files.len());
    for file in snapshot.files.iter() {
        validate_relative_path(&file.relative_path)?;
        if !seen.insert(file.relative_path.clone()) {
            return Err(CacheError::new(format!(
                "duplicate snapshot path: {}",
                file.relative_path.display()
            )));
        }
        let source = snapshot.root.path().join(&file.relative_path);
        let contents = tokio::fs::read(&source)
            .await
            .map_err(|error| cache_io("read snapshot artifact", &source, error))?;
        let actual = snapshot_file_fingerprint(&file.relative_path, file.kind, &contents);
        if actual != file.fingerprint {
            return Err(CacheError::new(format!(
                "snapshot file fingerprint changed before caching: {}",
                source.display()
            )));
        }
        all_hasher.update(actual.as_bytes());
        if file.kind == SnapshotFileKind::Source {
            source_hasher.update(actual.as_bytes());
        }
        let destination = artifact.join(&file.relative_path);
        create_parent(&destination).await?;
        tokio::fs::write(&destination, contents)
            .await
            .map_err(|error| cache_io("write snapshot cache", &destination, error))?;
        files.push(SnapshotFileMetadata {
            path: normalized_path(&file.relative_path),
            kind: match file.kind {
                SnapshotFileKind::Source => FileKindMetadata::Source,
                SnapshotFileKind::Constraint => FileKindMetadata::Constraint,
            },
            fingerprint: actual.into_bytes(),
        });
    }
    let fingerprint = OutputFingerprint::new(*all_hasher.finalize().as_bytes());
    let source_fingerprint = OutputFingerprint::new(*source_hasher.finalize().as_bytes());
    if fingerprint != snapshot.fingerprint() || source_fingerprint != snapshot.source_fingerprint()
    {
        return Err(CacheError::new("snapshot aggregate fingerprint changed before caching"));
    }
    write_metadata(
        stage,
        &SnapshotManifest {
            format: ENTRY_FORMAT,
            fingerprint: fingerprint.into_bytes(),
            source_fingerprint: source_fingerprint.into_bytes(),
            files,
        },
    )
    .await
}

async fn validate_snapshot_entry(
    entry: &Path,
    output_root: &Path,
    manifest: SnapshotManifest,
) -> Result<Option<Snapshot>, CacheError> {
    let artifact = entry.join(ARTIFACT_NAME);
    let mut all_hasher = Hasher::new();
    let mut source_hasher = Hasher::new();
    let mut files = Vec::with_capacity(manifest.files.len());
    let mut seen = HashSet::with_capacity(manifest.files.len());
    for cached in manifest.files {
        let relative_path = PathBuf::from(cached.path);
        if validate_relative_path(&relative_path).is_err() || !seen.insert(relative_path.clone()) {
            return Ok(None);
        }
        let kind = match cached.kind {
            FileKindMetadata::Source => SnapshotFileKind::Source,
            FileKindMetadata::Constraint => SnapshotFileKind::Constraint,
        };
        let path = artifact.join(&relative_path);
        let contents = match tokio::fs::read(&path).await {
            Ok(contents) => contents,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(cache_io("read cached snapshot", &path, error)),
        };
        let actual = snapshot_file_fingerprint(&relative_path, kind, &contents);
        if actual.as_bytes() != &cached.fingerprint {
            return Ok(None);
        }
        all_hasher.update(actual.as_bytes());
        if kind == SnapshotFileKind::Source {
            source_hasher.update(actual.as_bytes());
        }
        files.push(SnapshotFile { relative_path, kind, fingerprint: actual });
    }
    let fingerprint = OutputFingerprint::new(*all_hasher.finalize().as_bytes());
    let source_fingerprint = OutputFingerprint::new(*source_hasher.finalize().as_bytes());
    if fingerprint.as_bytes() != &manifest.fingerprint
        || source_fingerprint.as_bytes() != &manifest.source_fingerprint
    {
        return Ok(None);
    }
    Ok(Some(Snapshot {
        root: Artifact::<SnapshotDirectory>::new(output_root, fingerprint),
        files: files.into(),
        source_fingerprint,
    }))
}

fn snapshot_file_fingerprint(
    relative_path: &Path,
    kind: SnapshotFileKind,
    contents: &[u8],
) -> OutputFingerprint {
    let mut hasher = Hasher::new();
    hasher.update(normalized_path(relative_path).as_bytes());
    hasher.update(&[
        0,
        match kind {
            SnapshotFileKind::Source => b's',
            SnapshotFileKind::Constraint => b'c',
        },
        0,
    ]);
    hasher.update(contents);
    OutputFingerprint::new(*hasher.finalize().as_bytes())
}

async fn read_metadata<T: for<'de> Deserialize<'de>>(
    entry: &Path,
) -> Result<Option<T>, CacheError> {
    let path = entry.join(METADATA_FILE);
    let source = match tokio::fs::read(&path).await {
        Ok(source) => source,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(cache_io("read cache metadata", &path, error)),
    };
    let metadata = match serde_json::from_slice::<T>(&source) {
        Ok(metadata) => metadata,
        Err(_) => {
            invalidate_entry(entry).await;
            return Ok(None);
        }
    };
    Ok(Some(metadata))
}

async fn write_metadata<T: Serialize>(entry: &Path, metadata: &T) -> Result<(), CacheError> {
    let path = entry.join(METADATA_FILE);
    let source = serde_json::to_vec(metadata)
        .map_err(|error| CacheError::new(format!("failed to encode cache metadata: {error}")))?;
    tokio::fs::write(&path, source)
        .await
        .map_err(|error| cache_io("write cache metadata", &path, error))
}

async fn prepare_entry_stage(entry: &Path) -> Result<PathBuf, CacheError> {
    let parent = entry.parent().ok_or_else(|| CacheError::new("cache entry has no parent"))?;
    tokio::fs::create_dir_all(parent)
        .await
        .map_err(|error| cache_io("create cache directory", parent, error))?;
    let stage = temporary_sibling(entry, "store")?;
    remove_path(&stage)
        .await
        .map_err(|error| cache_io("remove stale cache staging path", &stage, error))?;
    tokio::fs::create_dir_all(&stage)
        .await
        .map_err(|error| cache_io("create cache staging path", &stage, error))?;
    Ok(stage)
}

async fn publish_entry(stage: &Path, entry: &Path) -> Result<(), CacheError> {
    if tokio::fs::try_exists(entry)
        .await
        .map_err(|error| cache_io("inspect cache entry", entry, error))?
    {
        remove_path(stage)
            .await
            .map_err(|error| cache_io("remove redundant cache staging path", stage, error))?;
        return Ok(());
    }
    match tokio::fs::rename(stage, entry).await {
        Ok(()) => Ok(()),
        Err(_error)
            if tokio::fs::try_exists(entry).await.map_err(|inspect| {
                cache_io("inspect concurrent cache entry", entry, inspect)
            })? =>
        {
            remove_path(stage)
                .await
                .map_err(|cleanup| cache_io("remove redundant cache staging path", stage, cleanup))
        }
        Err(error) => Err(cache_io("publish cache entry", entry, error)),
    }
}

async fn restore_file(source: &Path, destination: &Path) -> Result<(), CacheError> {
    create_parent(destination).await?;
    let stage = temporary_sibling(destination, "restore")?;
    let _ = remove_path(&stage).await;
    tokio::fs::copy(source, &stage)
        .await
        .map_err(|error| cache_io("restore cached artifact", destination, error))?;
    replace_path(&stage, destination).await
}

async fn restore_directory(source: &Path, destination: &Path) -> Result<(), CacheError> {
    if let Some(parent) = destination.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| cache_io("create snapshot restore parent", parent, error))?;
    }
    let stage = temporary_sibling(destination, "restore")?;
    let _ = remove_path(&stage).await;
    tokio::fs::create_dir_all(&stage)
        .await
        .map_err(|error| cache_io("create snapshot restore staging path", &stage, error))?;
    copy_directory(source, &stage).await?;
    replace_path(&stage, destination).await
}

fn copy_directory<'a>(
    source: &'a Path,
    destination: &'a Path,
) -> BoxFuture<'a, Result<(), CacheError>> {
    Box::pin(async move {
        let mut entries = tokio::fs::read_dir(source)
            .await
            .map_err(|error| cache_io("read cached directory", source, error))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|error| cache_io("read cached directory entry", source, error))?
        {
            let file_type = entry.file_type().await.map_err(|error| {
                cache_io("inspect cached directory entry", &entry.path(), error)
            })?;
            let target = destination.join(entry.file_name());
            if file_type.is_dir() {
                tokio::fs::create_dir_all(&target)
                    .await
                    .map_err(|error| cache_io("create restored directory", &target, error))?;
                copy_directory(&entry.path(), &target).await?;
            } else if file_type.is_file() {
                tokio::fs::copy(entry.path(), &target)
                    .await
                    .map_err(|error| cache_io("copy restored file", &target, error))?;
            } else {
                return Err(CacheError::new(format!(
                    "cache entry contains unsupported filesystem object: {}",
                    entry.path().display()
                )));
            }
        }
        Ok(())
    })
}

async fn replace_path(stage: &Path, destination: &Path) -> Result<(), CacheError> {
    remove_path(destination)
        .await
        .map_err(|error| cache_io("remove previous restored artifact", destination, error))?;
    tokio::fs::rename(stage, destination)
        .await
        .map_err(|error| cache_io("publish restored artifact", destination, error))
}

async fn create_parent(path: &Path) -> Result<(), CacheError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|error| cache_io("create artifact parent", parent, error))?;
    }
    Ok(())
}

async fn hash_file(path: &Path) -> Result<OutputFingerprint, CacheError> {
    let contents =
        tokio::fs::read(path).await.map_err(|error| cache_io("hash artifact", path, error))?;
    Ok(OutputFingerprint::new(*blake3::hash(&contents).as_bytes()))
}

async fn hash_file_if_present(path: &Path) -> Result<Option<OutputFingerprint>, CacheError> {
    match tokio::fs::read(path).await {
        Ok(contents) => Ok(Some(OutputFingerprint::new(*blake3::hash(&contents).as_bytes()))),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(cache_io("hash cached artifact", path, error)),
    }
}

async fn invalidate_entry(entry: &Path) {
    let _ = remove_path(entry).await;
}

async fn remove_path(path: &Path) -> std::io::Result<()> {
    match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) if metadata.is_dir() => tokio::fs::remove_dir_all(path).await,
        Ok(_) => tokio::fs::remove_file(path).await,
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn temporary_sibling(path: &Path, operation: &str) -> Result<PathBuf, CacheError> {
    let file_name = path
        .file_name()
        .ok_or_else(|| CacheError::new(format!("path has no final component: {}", path.display())))?
        .to_string_lossy();
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    Ok(path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join(format!(".{file_name}.{operation}-{}-{id}.tmp", std::process::id())))
}

fn validate_relative_path(path: &Path) -> Result<(), CacheError> {
    if path.as_os_str().is_empty()
        || !path.components().all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(CacheError::new(format!("invalid project-relative cache path: {path:?}")));
    }
    Ok(())
}

fn normalized_path(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn add_field(hasher: &mut Hasher, name: &str, value: &[u8]) {
    hasher.update(&(name.len() as u64).to_le_bytes());
    hasher.update(name.as_bytes());
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value);
}

fn hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn cache_io(operation: &str, path: &Path, error: std::io::Error) -> CacheError {
    CacheError::new(format!("failed to {operation} {}: {error}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::Arc;
    use std::time::{SystemTime, UNIX_EPOCH};

    use bitloom_core::{
        CacheMode, Executor, ExecutorOptions, GraphBuilder, TaskKind, TaskPhase, task,
    };

    use super::*;

    #[derive(Debug)]
    struct TestRoutedNetlist;

    impl ArtifactKind for TestRoutedNetlist {}

    fn test_root(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock should follow the Unix epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("bitloom-cache-{name}-{nonce}"))
    }

    fn strict_executor() -> Executor {
        Executor::new(ExecutorOptions {
            max_parallel_tasks: NonZeroUsize::MIN,
            cache_mode: CacheMode::Strict,
        })
    }

    fn cache_key<T>(cache: &impl TaskCache<T>, kind: TaskKind, schema: u32) -> CacheKey {
        cache
            .key(CacheKeyInput {
                kind,
                schema,
                dependency_fingerprints: &[OutputFingerprint::new([7; 32])],
            })
            .unwrap()
    }

    #[tokio::test]
    async fn restores_snapshot_and_keeps_constraints_out_of_source_fingerprint() {
        let root = test_root("snapshot");
        tokio::fs::create_dir_all(root.join("rtl")).await.unwrap();
        tokio::fs::create_dir_all(root.join("constraints")).await.unwrap();
        tokio::fs::write(root.join("rtl/top.sv"), "module top; endmodule").await.unwrap();
        tokio::fs::write(root.join("constraints/top.cst"), "IO_LOC \"clk\" 1;").await.unwrap();
        let cache_root = root.join("build/cache");
        let output_root = root.join("build/snapshot");
        let inputs =
            [SnapshotInput::source("rtl/top.sv"), SnapshotInput::constraint("constraints/top.cst")];

        let first_task =
            bitloom_snapshot::SnapshotTask::new(&root, &output_root, inputs.clone()).unwrap();
        let first_cache = SnapshotCache::new(
            &cache_root,
            first_task.project_root(),
            first_task.output_root(),
            first_task.inputs(),
        )
        .await
        .unwrap();
        let first = task(first_task.with_cache(Arc::new(first_cache)));
        let graph = GraphBuilder::new().with_root(&first).build().unwrap();
        strict_executor().run(&graph).await.unwrap();
        let first_output = first.result().await.unwrap();
        let first_source_fingerprint = first_output.source_fingerprint();
        let first_full_fingerprint = first_output.fingerprint();
        assert_eq!(first.snapshot().phase, TaskPhase::Finished);

        tokio::fs::remove_dir_all(&output_root).await.unwrap();
        let second_task =
            bitloom_snapshot::SnapshotTask::new(&root, &output_root, inputs.clone()).unwrap();
        let second_cache = SnapshotCache::new(
            &cache_root,
            second_task.project_root(),
            second_task.output_root(),
            second_task.inputs(),
        )
        .await
        .unwrap();
        let second = task(second_task.with_cache(Arc::new(second_cache)));
        let graph = GraphBuilder::new().with_root(&second).build().unwrap();
        let report = strict_executor().run(&graph).await.unwrap();
        assert_eq!(second.snapshot().phase, TaskPhase::Cached);
        assert_eq!(report.cache_hits, 1);
        assert!(output_root.join("rtl/top.sv").is_file());

        tokio::fs::remove_dir_all(&output_root).await.unwrap();
        tokio::fs::write(root.join("constraints/top.cst"), "IO_LOC \"clk\" 2;").await.unwrap();
        let third_task = bitloom_snapshot::SnapshotTask::new(&root, &output_root, inputs).unwrap();
        let third_cache = SnapshotCache::new(
            &cache_root,
            third_task.project_root(),
            third_task.output_root(),
            third_task.inputs(),
        )
        .await
        .unwrap();
        let third = task(third_task.with_cache(Arc::new(third_cache)));
        let graph = GraphBuilder::new().with_root(&third).build().unwrap();
        strict_executor().run(&graph).await.unwrap();
        let third_output = third.result().await.unwrap();
        assert_eq!(third.snapshot().phase, TaskPhase::Finished);
        assert_eq!(third_output.source_fingerprint(), first_source_fingerprint);
        assert_ne!(third_output.fingerprint(), first_full_fingerprint);

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn restores_synthesis_and_pack_files_and_rejects_corruption() {
        let root = test_root("files");
        let cache_root = root.join("cache");
        let synthesis_path = root.join("work/synthesis.json");
        tokio::fs::create_dir_all(synthesis_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&synthesis_path, b"netlist").await.unwrap();
        let synthesis_fingerprint = OutputFingerprint::new(*blake3::hash(b"netlist").as_bytes());
        let synthesis = SynthesisOutput {
            netlist: Artifact::<SynthesizedNetlist>::new(&synthesis_path, synthesis_fingerprint),
        };
        let synthesis_cache = SynthesisCache::new(
            &cache_root,
            &synthesis_path,
            CacheParameters::new("synthesis-test"),
        );
        let synthesis_key = cache_key(&synthesis_cache, TaskKind::new("bitloom.synthesis"), 2);
        synthesis_cache.store(&synthesis_key, &synthesis).await.unwrap();
        tokio::fs::remove_file(&synthesis_path).await.unwrap();
        let restored = synthesis_cache.restore(&synthesis_key).await.unwrap().unwrap();
        assert_eq!(restored.fingerprint, synthesis_fingerprint);
        assert_eq!(tokio::fs::read(&synthesis_path).await.unwrap(), b"netlist");

        let cache_artifact = synthesis_cache.location.entry(&synthesis_key).join(ARTIFACT_NAME);
        tokio::fs::write(&cache_artifact, b"corrupt").await.unwrap();
        tokio::fs::remove_file(&synthesis_path).await.unwrap();
        assert!(synthesis_cache.restore(&synthesis_key).await.unwrap().is_none());

        let bitstream_path = root.join("work/top.fs");
        tokio::fs::write(&bitstream_path, b"bitstream").await.unwrap();
        let bitstream_fingerprint = OutputFingerprint::new(*blake3::hash(b"bitstream").as_bytes());
        let pack = PackOutput {
            bitstream: Artifact::<PackedBitstream>::new(&bitstream_path, bitstream_fingerprint),
        };
        let pack_cache =
            PackCache::new(&cache_root, &bitstream_path, CacheParameters::new("pack-test"));
        let pack_key = cache_key(&pack_cache, TaskKind::new("bitloom.pack"), 1);
        pack_cache.store(&pack_key, &pack).await.unwrap();
        tokio::fs::remove_file(&bitstream_path).await.unwrap();
        let restored = pack_cache.restore(&pack_key).await.unwrap().unwrap();
        assert_eq!(restored.output.fingerprint(), bitstream_fingerprint);
        assert_eq!(tokio::fs::read(&bitstream_path).await.unwrap(), b"bitstream");

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn round_trips_selected_pnr_candidate_and_statistics() {
        let root = test_root("pnr");
        let cache_root = root.join("cache");
        let routed_path = root.join("work/candidate.json");
        tokio::fs::create_dir_all(routed_path.parent().unwrap()).await.unwrap();
        tokio::fs::write(&routed_path, b"routed").await.unwrap();
        let fingerprint = OutputFingerprint::new(*blake3::hash(b"routed").as_bytes());
        let output = PnrOutput {
            selected: PnrCandidate {
                run_index: 3,
                seed: 42,
                score: PnrScore::new(12.5).unwrap(),
                artifact: Artifact::<TestRoutedNetlist>::new(&routed_path, fingerprint),
                statistics: PnrStatistics {
                    resources: BTreeMap::from([(
                        "LUT4".to_owned(),
                        ResourceUsage { used: 10, available: 100 },
                    )]),
                    clocks: BTreeMap::from([(
                        "clk".to_owned(),
                        ClockFrequency { achieved_mhz: 81.5, constraint_mhz: 27.0 },
                    )]),
                },
            },
            attempts: 8,
            successful_runs: 7,
            failed_runs: vec![PnrRunFailure {
                run_index: 1,
                seed: 40,
                message: "timing failed".to_owned(),
            }],
        };
        let restored_path = root.join("work/cached-selected.json");
        let cache = PnrCache::<TestRoutedNetlist>::new(
            &cache_root,
            &restored_path,
            CacheParameters::new("pnr-test"),
        );
        let key = cache_key(&cache, TaskKind::new("bitloom.pnr"), 2);

        cache.store(&key, &output).await.unwrap();
        let restored = cache.restore(&key).await.unwrap().unwrap().output;

        assert_eq!(restored.selected.seed, 42);
        assert_eq!(restored.selected.score.get(), 12.5);
        assert_eq!(restored.selected.statistics.resources["LUT4"].used, 10);
        assert_eq!(restored.selected.statistics.clocks["clk"].achieved_mhz, 81.5);
        assert_eq!(restored.failed_runs[0].message, "timing failed");
        assert_eq!(tokio::fs::read(&restored_path).await.unwrap(), b"routed");

        tokio::fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn cache_key_changes_with_parameters_and_dependencies() {
        let root = PathBuf::from("cache");
        let mut first_parameters = CacheParameters::new("test");
        first_parameters.add_str("top", "first");
        let first = PackCache::new(&root, "first.fs", first_parameters);
        let mut second_parameters = CacheParameters::new("test");
        second_parameters.add_str("top", "second");
        let second = PackCache::new(&root, "second.fs", second_parameters);
        let input = CacheKeyInput {
            kind: TaskKind::new("bitloom.pack"),
            schema: 1,
            dependency_fingerprints: &[OutputFingerprint::new([1; 32])],
        };
        let other_dependency = [OutputFingerprint::new([2; 32])];

        assert_ne!(first.key(input).unwrap(), second.key(input).unwrap());
        assert_ne!(
            first.key(input).unwrap(),
            first
                .key(CacheKeyInput { dependency_fingerprints: &other_dependency, ..input })
                .unwrap()
        );
    }
}
