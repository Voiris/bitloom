extern crate core;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::{fs, io};

use ariadne::{CharSet, Color, Config, IndexType, Label, Report, ReportKind};

mod cache;
mod diagnostics;
mod parser;
mod resolver;

pub use cache::{CacheError, PartitionCache, PartitionFingerprint};
pub use diagnostics::{DiagLabel, DiagResult, DiagSuccess, Diagnostic, LabelStyle, Severity};
pub use resolver::resolved::{Layout, Partition, Source as PartitionSource};

const ERROR_SEVERITY_COLOR: Color = Color::Red;
const WARNING_SEVERITY_COLOR: Color = Color::Yellow;
const PRIMARY_LABEL_COLOR: Color = Color::Cyan;
const SECONDARY_LABEL_COLOR: Color = Color::Blue;

#[derive(Debug, Clone)]
pub struct Source {
    pub path: PathBuf,
    pub content: String,
}

impl Source {
    pub fn new(path: impl Into<PathBuf>, content: impl Into<String>) -> Self {
        Self { path: path.into(), content: content.into() }
    }

    pub fn read(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        Ok(Self { path: path.to_owned(), content: fs::read_to_string(path)? })
    }
}

#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum BuiltinValue {
    Path(PathBuf),
}

#[derive(Debug, Clone, Default)]
pub struct CompileOptions {
    builtins: HashMap<String, BuiltinValue>,
}

impl CompileOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_builtin_path(mut self, name: impl Into<String>, path: impl Into<PathBuf>) -> Self {
        self.set_builtin_path(name, path);
        self
    }

    pub fn set_builtin_path(
        &mut self,
        name: impl Into<String>,
        path: impl Into<PathBuf>,
    ) -> Option<BuiltinValue> {
        self.builtins.insert(name.into(), BuiltinValue::Path(path.into()))
    }

    pub fn builtins(&self) -> &HashMap<String, BuiltinValue> {
        &self.builtins
    }
}

pub fn compile(source: &Source, options: &CompileOptions) -> DiagResult<Layout> {
    let parsed = parser::parse_source(source)?;
    let mut warnings = parsed.warnings.unwrap_or_default();

    let resolved = resolver::resolve_layout(
        parsed.value,
        options,
        source.path.parent().unwrap_or_else(|| Path::new("")),
    )?;
    if let Some(mut resolver_warnings) = resolved.warnings {
        warnings.append(&mut resolver_warnings);
    }

    Ok(DiagSuccess::with_warnings(resolved.value, warnings))
}

/// Parses BLPL without resolving files or laying out partitions.
///
/// This keeps syntax checking independent from build artifacts, so a graph can
/// parse `partition.blpl` in parallel with synthesis and place-and-route.
pub fn parse(source: &Source) -> DiagResult<()> {
    let parsed = parser::parse_source(source)?;
    Ok(DiagSuccess::new((), parsed.warnings))
}

/// Renders a BLPL diagnostic with Ariadne using the parser's original spans.
pub fn render_diagnostic(source: &Source, diagnostic: &Diagnostic) -> String {
    render_diagnostic_with_color(source, diagnostic, true)
}

/// Renders a BLPL diagnostic without terminal escape sequences.
pub fn render_diagnostic_plain(source: &Source, diagnostic: &Diagnostic) -> String {
    render_diagnostic_with_color(source, diagnostic, false)
}

fn render_diagnostic_with_color(source: &Source, diagnostic: &Diagnostic, color: bool) -> String {
    let source_id = source.path.display().to_string();
    let primary_span = diagnostic
        .labels
        .iter()
        .find(|label| matches!(label.priority, LabelStyle::Primary))
        .or_else(|| diagnostic.labels.first())
        .map_or(0..0, |label| label.span.into_range());
    let report_kind = match diagnostic.severity {
        Severity::Error => ReportKind::Custom("Error", ERROR_SEVERITY_COLOR),
        Severity::Warning => ReportKind::Custom("Warning", WARNING_SEVERITY_COLOR),
    };
    let mut report = Report::build(report_kind, (source_id.clone(), primary_span))
        .with_config(
            Config::default()
                .with_index_type(IndexType::Byte)
                .with_char_set(CharSet::Ascii)
                .with_underlines(true)
                .with_color(color),
        )
        .with_message(&diagnostic.message);

    for label in &diagnostic.labels {
        let primary = matches!(label.priority, LabelStyle::Primary);
        report = report.with_label(
            Label::new((source_id.clone(), label.span.into_range()))
                .with_color(label_color(label.priority))
                .with_priority(i32::from(primary))
                .with_message(&label.message),
        );
    }
    if let Some(help) = &diagnostic.help {
        report = report.with_help(help);
    }
    if let Some(note) = &diagnostic.note {
        report = report.with_note(note);
    }

    let mut rendered = Vec::new();
    if report
        .finish()
        .write((source_id, ariadne::Source::from(source.content.as_str())), &mut rendered)
        .is_err()
    {
        return diagnostic.message.clone();
    }
    String::from_utf8_lossy(&rendered).trim_end().to_owned()
}

fn label_color(priority: LabelStyle) -> Color {
    match priority {
        LabelStyle::Primary => PRIMARY_LABEL_COLOR,
        LabelStyle::Secondary => SECONDARY_LABEL_COLOR,
    }
}
