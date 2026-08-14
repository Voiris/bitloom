use std::collections::HashMap;
use std::path::{Path, PathBuf};

use chumsky::span::SimpleSpan;

use crate::diagnostics::{DiagLabel, DiagResult, DiagSuccess, Diagnostic, Severity};
use crate::resolver::resolved::ResolvedValue;
use crate::{BuiltinValue, CompileOptions};

#[derive(Debug, Clone, Default)]
pub(super) struct Context<'src> {
    pub(super) constants: HashMap<&'src str, ResolvedValue<'src>>,
    pub(super) builtins: HashMap<String, ResolvedValue<'src>>,
    pub(super) flash: FlashConfig,
    pub(super) layout: LayoutConfig,
    pub(super) base_dir: PathBuf,
}

impl<'src> Context<'src> {
    pub(super) fn from_options(options: &CompileOptions, base_dir: &Path) -> Self {
        let builtins = options
            .builtins()
            .iter()
            .map(|(name, value)| {
                let value = match value {
                    BuiltinValue::Path(path) => ResolvedValue::Path(path.clone()),
                };
                (name.clone(), value)
            })
            .collect();

        Self { builtins, base_dir: base_dir.to_owned(), ..Self::default() }
    }

    pub(super) fn get_const(
        &self,
        name: &'src str,
        span: SimpleSpan<usize>,
    ) -> DiagResult<&ResolvedValue<'src>> {
        match self.constants.get(name) {
            Some(value) => Ok(DiagSuccess::without_warnings(value)),

            None => Err(Diagnostic::new(Severity::Error, format!("undefined constant `{name}`"))
                .add_label(DiagLabel::new_primary(
                    "this constant is not defined".to_string(),
                    span,
                ))),
        }
    }

    pub(super) fn get_builtin(
        &self,
        name: &'src str,
        span: SimpleSpan<usize>,
    ) -> DiagResult<&ResolvedValue<'src>> {
        match self.builtins.get(name) {
            Some(value) => Ok(DiagSuccess::without_warnings(value)),

            None => Err(Diagnostic::new(Severity::Error, format!("undefined builtin `${name}`"))
                .add_label(DiagLabel::new_primary(
                    "this builtin does not exist".to_string(),
                    span,
                ))),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(super) struct FlashConfig {
    pub(super) size: Option<u64>,
    pub(super) erase_size: Option<u64>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct LayoutConfig {
    pub(super) offset_align: Option<u64>,
}
