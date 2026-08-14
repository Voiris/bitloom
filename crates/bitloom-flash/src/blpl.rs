use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use bitloom_core::{OutputFingerprint, Task, TaskContext, TaskKind};
use thiserror::Error;

#[derive(Debug, Clone)]
pub struct BlplDocument {
    source: ::blpl::Source,
    fingerprint: OutputFingerprint,
    warnings: Arc<[::blpl::Diagnostic]>,
}

impl BlplDocument {
    pub fn source(&self) -> &::blpl::Source {
        &self.source
    }

    pub fn warnings(&self) -> &[::blpl::Diagnostic] {
        &self.warnings
    }

    pub fn fingerprint(&self) -> OutputFingerprint {
        self.fingerprint
    }
}

#[derive(Debug, Error)]
pub enum BlplError {
    #[error("failed to read {path}: {source}")]
    Read { path: PathBuf, source: std::io::Error },
    #[error("invalid partition layout: {diagnostic:?}")]
    Diagnostic { path: PathBuf, contents: Arc<str>, diagnostic: ::blpl::Diagnostic },
}

pub struct BlplParseTask {
    path: PathBuf,
}

impl BlplParseTask {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Task for BlplParseTask {
    type Output = BlplDocument;
    type Error = BlplError;
    type Dependencies = ();

    const KIND: TaskKind = TaskKind::new("bitloom.blpl.parse");
    const CACHE_SCHEMA: u32 = 1;

    fn label(&self) -> Cow<'static, str> {
        "Parse partition layout".into()
    }

    fn dependencies(&self) -> &Self::Dependencies {
        &()
    }

    fn output_fingerprint(&self, output: &Self::Output) -> Option<OutputFingerprint> {
        Some(output.fingerprint())
    }

    fn format_error(&self, error: &Self::Error) -> String {
        match error {
            BlplError::Diagnostic { path, contents, diagnostic } => {
                render_blpl_diagnostic(path, contents, diagnostic)
            }
            BlplError::Read { .. } => error.to_string(),
        }
    }

    async fn run(&self, context: TaskContext, (): ()) -> Result<Self::Output, Self::Error> {
        let contents = tokio::fs::read_to_string(&self.path)
            .await
            .map_err(|source| BlplError::Read { path: self.path.clone(), source })?;
        let source = ::blpl::Source::new(&self.path, contents);
        let parsed = ::blpl::parse(&source).map_err(|diagnostic| BlplError::Diagnostic {
            path: self.path.clone(),
            contents: Arc::from(source.content.as_str()),
            diagnostic,
        })?;
        let fingerprint =
            OutputFingerprint::new(*blake3::hash(source.content.as_bytes()).as_bytes());
        let warnings = parsed.warnings.unwrap_or_default();
        for warning in &warnings {
            context.warning(::blpl::render_diagnostic_plain(&source, warning));
        }
        Ok(BlplDocument { source, fingerprint, warnings: warnings.into() })
    }
}

pub(crate) fn emit_new_blpl_warnings(
    context: &TaskContext,
    document: &BlplDocument,
    warnings: Option<&[::blpl::Diagnostic]>,
) {
    for warning in warnings.unwrap_or_default() {
        if document.warnings().contains(warning) {
            continue;
        }
        context.warning(::blpl::render_diagnostic_plain(document.source(), warning));
    }
}

pub(crate) fn render_blpl_diagnostic(
    path: &Path,
    contents: &str,
    diagnostic: &::blpl::Diagnostic,
) -> String {
    let source = ::blpl::Source::new(path, contents);
    format!("\n{}", ::blpl::render_diagnostic(&source, diagnostic))
}
