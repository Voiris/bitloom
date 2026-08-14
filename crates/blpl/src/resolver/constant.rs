use std::collections::HashMap;

use chumsky::span::SimpleSpanned;

use super::context::Context;
use crate::diagnostics::{DiagLabel, DiagResult, DiagSuccess, Diagnostic, Severity};
use crate::parser::unresolved::ConstDecl;
use crate::resolver::value::resolve_value;

pub(super) fn resolve_constants<'src>(
    declarations: &[SimpleSpanned<ConstDecl<'src>>],
    context: &mut Context<'src>,
) -> DiagResult<()> {
    let mut map = HashMap::new();
    let mut warnings = Vec::new();

    for decl in declarations {
        if let Some(&defined_span) = map.get(decl.name.inner) {
            return Err(Diagnostic::new(
                Severity::Error,
                format!("constant `{}` is already defined", decl.name.inner),
            )
            .add_label(DiagLabel::new_primary(
                "constant is redefined here".to_string(),
                decl.name.span,
            ))
            .add_label(DiagLabel::new_secondary(
                "previous definition is here".to_string(),
                defined_span,
            )));
        }

        let success = resolve_value(&decl.value, context)?;
        if let Some(success_warnings) = success.warnings {
            warnings.extend(success_warnings);
        }
        context.constants.insert(decl.name.inner, success.value.inner);
        map.insert(decl.name.inner, decl.name.span);
    }

    Ok(DiagSuccess::with_warnings((), warnings))
}
