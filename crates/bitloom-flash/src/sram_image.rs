use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::Arc;

use bitloom_core::{
    Dependencies, DependencyCollector, OutputFingerprint, Task, TaskContext, TaskKind, TaskRef,
};
use bitloom_pack::PackOutput;
use thiserror::Error;

use crate::blpl::{BlplDocument, BlplParseTask, emit_new_blpl_warnings, render_blpl_diagnostic};
use crate::model::{ProgramImage, ProgramMode};

pub struct SramImageDependencies<T: Task<Output = PackOutput>> {
    pub layout: TaskRef<BlplParseTask>,
    pub bitstream: TaskRef<T>,
}

impl<T: Task<Output = PackOutput>> Dependencies for SramImageDependencies<T> {
    type Resolved = (Arc<BlplDocument>, Arc<PackOutput>);

    fn collect(&self, collector: &mut DependencyCollector) {
        collector.add(&self.bitstream);
        collector.add(&self.layout);
    }

    async fn resolve(&self) -> Result<Self::Resolved, Arc<bitloom_core::TaskFailure>> {
        Ok((self.layout.result().await?, self.bitstream.result().await?))
    }
}

#[derive(Debug, Error)]
pub enum SramImageError {
    #[error("SRAM programming requires a partition layout containing exactly one bitstream")]
    InvalidLayout,
    #[error("invalid partition layout: {diagnostic:?}")]
    Blpl { path: PathBuf, contents: Arc<str>, diagnostic: ::blpl::Diagnostic },
}

pub struct SramImageTask<T: Task<Output = PackOutput>> {
    dependencies: SramImageDependencies<T>,
    bitstream_name: Arc<str>,
}

impl<T: Task<Output = PackOutput>> SramImageTask<T> {
    pub fn new(
        layout: TaskRef<BlplParseTask>,
        bitstream_name: impl Into<Arc<str>>,
        bitstream: TaskRef<T>,
    ) -> Self {
        Self {
            dependencies: SramImageDependencies { layout, bitstream },
            bitstream_name: bitstream_name.into(),
        }
    }
}

impl<T: Task<Output = PackOutput>> Task for SramImageTask<T> {
    type Output = ProgramImage;
    type Error = SramImageError;
    type Dependencies = SramImageDependencies<T>;

    const KIND: TaskKind = TaskKind::new("bitloom.flash.sram-image");

    fn label(&self) -> Cow<'static, str> {
        "Prepare SRAM image".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &self.dependencies
    }

    fn output_fingerprint(&self, output: &Self::Output) -> Option<OutputFingerprint> {
        Some(output.fingerprint)
    }

    fn format_error(&self, error: &Self::Error) -> String {
        match error {
            SramImageError::Blpl { path, contents, diagnostic } => {
                render_blpl_diagnostic(path, contents, diagnostic)
            }
            SramImageError::InvalidLayout => error.to_string(),
        }
    }

    async fn run(
        &self,
        context: TaskContext,
        (document, bitstream): (Arc<BlplDocument>, Arc<PackOutput>),
    ) -> Result<Self::Output, Self::Error> {
        let options = ::blpl::CompileOptions::new()
            .with_builtin_path(self.bitstream_name.as_ref(), bitstream.bitstream.path());
        let layout = ::blpl::compile(document.source(), &options).map_err(|diagnostic| {
            SramImageError::Blpl {
                path: document.source().path.clone(),
                contents: Arc::from(document.source().content.as_str()),
                diagnostic,
            }
        })?;
        emit_new_blpl_warnings(&context, &document, layout.warnings.as_deref());
        if layout.value.partitions.len() != 1
            || !matches!(
                &layout.value.partitions[0].source,
                ::blpl::PartitionSource::File(path) if path == bitstream.bitstream.path()
            )
        {
            return Err(SramImageError::InvalidLayout);
        }
        Ok(ProgramImage {
            path: bitstream.bitstream.path().to_owned(),
            mode: ProgramMode::Sram,
            fingerprint: bitstream.fingerprint(),
        })
    }
}
