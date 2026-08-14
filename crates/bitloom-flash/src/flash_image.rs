use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use bitloom_core::{
    Dependencies, DependencyCollector, EffectKind, OutputFingerprint, Task, TaskContext, TaskKind,
    TaskRef,
};
use bitloom_pack::PackOutput;
use thiserror::Error;

use crate::blpl::{BlplDocument, BlplParseTask, emit_new_blpl_warnings, render_blpl_diagnostic};
use crate::model::{ProgramImage, ProgramMode};
use crate::rollback::RemoveFile;

pub struct NamedBitstream<T: Task<Output = PackOutput>> {
    pub name: Arc<str>,
    pub task: TaskRef<T>,
}

impl<T: Task<Output = PackOutput>> Clone for NamedBitstream<T> {
    fn clone(&self) -> Self {
        Self { name: self.name.clone(), task: self.task.clone() }
    }
}

impl<T: Task<Output = PackOutput>> NamedBitstream<T> {
    pub fn new(name: impl Into<Arc<str>>, task: TaskRef<T>) -> Self {
        Self { name: name.into(), task }
    }
}

pub struct FlashImageDependencies<T: Task<Output = PackOutput>> {
    pub layout: TaskRef<BlplParseTask>,
    pub bitstreams: Vec<TaskRef<T>>,
}

pub struct FlashImageInputs {
    pub layout: Arc<BlplDocument>,
    pub bitstreams: Vec<Arc<PackOutput>>,
}

impl<T: Task<Output = PackOutput>> Dependencies for FlashImageDependencies<T> {
    type Resolved = FlashImageInputs;

    fn collect(&self, collector: &mut DependencyCollector) {
        for bitstream in &self.bitstreams {
            collector.add(bitstream);
        }
        collector.add(&self.layout);
    }

    async fn resolve(&self) -> Result<Self::Resolved, Arc<bitloom_core::TaskFailure>> {
        let layout = self.layout.result().await?;
        let mut bitstreams = Vec::with_capacity(self.bitstreams.len());
        for bitstream in &self.bitstreams {
            bitstreams.push(bitstream.result().await?);
        }
        Ok(FlashImageInputs { layout, bitstreams })
    }
}

#[derive(Debug, Error)]
pub enum FlashImageError {
    #[error("partition layout and build graph disagree about bitstream count")]
    BitstreamCount,
    #[error("invalid partition layout: {diagnostic:?}")]
    Blpl { path: PathBuf, contents: Arc<str>, diagnostic: ::blpl::Diagnostic },
    #[error("partition `{partition}` references missing bitstream `${bitstream}`")]
    MissingBitstream { partition: Arc<str>, bitstream: Arc<str> },
    #[error("failed to read bitstream {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error(
        "partition `{partition}` content ({content_size} bytes) exceeds its {partition_size}-byte partition"
    )]
    ContentTooLarge { partition: Arc<str>, content_size: u64, partition_size: u64 },
    #[error("partition `{partition}` offset or size overflows")]
    Overflow { partition: Arc<str> },
    #[error("partition `{first}` overlaps partition `{second}`")]
    Overlap { first: Arc<str>, second: Arc<str> },
    #[error("partition `{partition}` exceeds configured flash size {flash_size} bytes")]
    ExceedsFlash { partition: Arc<str>, flash_size: u64 },
    #[error("flash image is too large for this platform")]
    ImageTooLarge,
    #[error("failed to create flash image {path}: {source}")]
    Write { path: PathBuf, source: std::io::Error },
}

pub struct FlashImageTask<T: Task<Output = PackOutput>> {
    dependencies: FlashImageDependencies<T>,
    names: Arc<[Arc<str>]>,
    output: PathBuf,
}

impl<T: Task<Output = PackOutput>> FlashImageTask<T> {
    pub fn new(
        layout: TaskRef<BlplParseTask>,
        bitstreams: impl IntoIterator<Item = NamedBitstream<T>>,
        output: impl Into<PathBuf>,
    ) -> Self {
        let bitstreams = bitstreams.into_iter().collect::<Vec<_>>();
        let names = bitstreams.iter().map(|item| item.name.clone()).collect();
        let dependencies = FlashImageDependencies {
            layout,
            bitstreams: bitstreams.into_iter().map(|item| item.task).collect(),
        };
        Self { dependencies, names, output: output.into() }
    }
}

impl<T: Task<Output = PackOutput>> Task for FlashImageTask<T> {
    type Output = ProgramImage;
    type Error = FlashImageError;
    type Dependencies = FlashImageDependencies<T>;

    const KIND: TaskKind = TaskKind::new("bitloom.flash.image");
    const CACHE_SCHEMA: u32 = 1;

    fn label(&self) -> Cow<'static, str> {
        "Assemble flash image".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependencies
    }

    fn effect(&self) -> EffectKind {
        EffectKind::Reversible
    }

    fn output_claims(&self) -> Vec<PathBuf> {
        vec![self.output.clone()]
    }

    fn output_fingerprint(&self, output: &Self::Output) -> Option<OutputFingerprint> {
        Some(output.fingerprint)
    }

    fn format_error(&self, error: &Self::Error) -> String {
        match error {
            FlashImageError::Blpl { path, contents, diagnostic } => {
                render_blpl_diagnostic(path, contents, diagnostic)
            }
            _ => error.to_string(),
        }
    }

    async fn run(
        &self,
        context: TaskContext,
        inputs: FlashImageInputs,
    ) -> Result<Self::Output, Self::Error> {
        if self.names.len() != inputs.bitstreams.len() {
            return Err(FlashImageError::BitstreamCount);
        }
        let mut options = ::blpl::CompileOptions::new();
        for (name, output) in self.names.iter().zip(inputs.bitstreams.iter()) {
            options.set_builtin_path(name.as_ref(), output.bitstream.path());
        }
        let compiled = ::blpl::compile(inputs.layout.source(), &options).map_err(|diagnostic| {
            FlashImageError::Blpl {
                path: inputs.layout.source().path.clone(),
                contents: Arc::from(inputs.layout.source().content.as_str()),
                diagnostic,
            }
        })?;
        emit_new_blpl_warnings(&context, &inputs.layout, compiled.warnings.as_deref());

        let image_size = compiled
            .value
            .partitions
            .iter()
            .filter_map(|partition| partition.offset.checked_add(partition.size))
            .max()
            .unwrap_or(0);
        let image_len = usize::try_from(image_size).map_err(|_| FlashImageError::ImageTooLarge)?;
        let mut image = Vec::new();
        image.try_reserve_exact(image_len).map_err(|_| FlashImageError::ImageTooLarge)?;
        image.resize(image_len, 0xff);
        for partition in &compiled.value.partitions {
            let ::blpl::PartitionSource::File(path) = &partition.source;
            let bytes = tokio::fs::read(path)
                .await
                .map_err(|source| FlashImageError::Read { path: path.clone(), source })?;
            let start =
                usize::try_from(partition.offset).map_err(|_| FlashImageError::ImageTooLarge)?;
            let content_end =
                start.checked_add(bytes.len()).ok_or(FlashImageError::ImageTooLarge)?;
            image[start..content_end].copy_from_slice(&bytes);
            let magic_end =
                start.checked_add(partition.magic.len()).ok_or(FlashImageError::ImageTooLarge)?;
            image[start..magic_end].copy_from_slice(&partition.magic);
        }

        if let Some(parent) = self.output.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|source| FlashImageError::Write { path: parent.to_owned(), source })?;
        }
        context.defer(RemoveFile::new(self.output.clone()));
        tokio::fs::write(&self.output, &image)
            .await
            .map_err(|source| FlashImageError::Write { path: self.output.clone(), source })?;
        let fingerprint = OutputFingerprint::new(*blake3::hash(&image).as_bytes());
        Ok(ProgramImage { path: self.output.clone(), mode: ProgramMode::Flash, fingerprint })
    }
}
