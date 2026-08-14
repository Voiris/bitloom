use chumsky::span::SimpleSpan;

pub type DiagResult<T> = Result<DiagSuccess<T>, Diagnostic>;

#[derive(Debug, Clone)]
pub struct DiagSuccess<T> {
    pub value: T,
    pub warnings: Option<Vec<Diagnostic>>,
}

impl<T> DiagSuccess<T> {
    pub fn new(value: T, warnings: Option<Vec<Diagnostic>>) -> Self {
        Self { value, warnings }
    }

    pub fn without_warnings(value: T) -> Self {
        Self { value, warnings: None }
    }

    pub fn with_warnings(value: T, warnings: Vec<Diagnostic>) -> Self {
        Self { value, warnings: (!warnings.is_empty()).then_some(warnings) }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,

    pub labels: Vec<DiagLabel>,
    pub help: Option<String>,
    pub note: Option<String>,
}

impl Diagnostic {
    pub fn new(severity: Severity, message: String) -> Diagnostic {
        Diagnostic { severity, message, labels: vec![], help: None, note: None }
    }

    pub fn add_label(mut self, label: DiagLabel) -> Diagnostic {
        self.labels.push(label);
        self
    }

    pub fn with_help(mut self, message: String) -> Diagnostic {
        self.help = Some(message);
        self
    }

    pub fn with_note(mut self, message: String) -> Diagnostic {
        self.note = Some(message);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagLabel {
    pub message: String,
    pub span: SimpleSpan<usize>,
    pub priority: LabelStyle,
}

impl DiagLabel {
    pub fn new_primary(message: String, span: SimpleSpan<usize>) -> DiagLabel {
        DiagLabel { message, span, priority: LabelStyle::Primary }
    }

    pub fn new_secondary(message: String, span: SimpleSpan<usize>) -> DiagLabel {
        DiagLabel { message, span, priority: LabelStyle::Secondary }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelStyle {
    Primary,
    Secondary,
}
