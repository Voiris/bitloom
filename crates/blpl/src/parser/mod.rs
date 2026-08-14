mod constant;
mod directive;
mod layout;
mod literals;
mod partition;
mod syntax;
pub mod unresolved;
mod value;

#[cfg(test)]
mod tests;

use chumsky::Parser;
use chumsky::error::RichReason;
use chumsky::prelude::{Rich, SimpleSpan};
use unresolved::UnresolvedLayout;

use crate::Source;
use crate::diagnostics::{DiagLabel, DiagResult, DiagSuccess, Diagnostic, Severity};
use crate::parser::layout::layout_parser;

fn map_errors<'src>(
    errors: Vec<Rich<'src, char, SimpleSpan<usize>, Diagnostic>>,
    allow_expected_found: bool,
) -> Vec<Diagnostic> {
    errors
        .into_iter()
        .map(|e| {
            let span = *e.span();

            match e.into_reason() {
                RichReason::Custom(diag) => diag,
                RichReason::ExpectedFound { .. } if !allow_expected_found => {
                    unreachable!(
                        "successful parse produced RichReason::ExpectedFound; \
                            recovery was probably enabled"
                    )
                }
                RichReason::ExpectedFound { expected, found } => {
                    let expected = expected
                        .into_iter()
                        .map(|pattern| pattern.to_string())
                        .collect::<Vec<String>>();

                    let expected = match expected.as_slice() {
                        [] => "something else".to_owned(),
                        [single] => single.clone(),
                        [rest @ .., last] => {
                            format!("{} or {last}", rest.join(", "))
                        }
                    };

                    let found = found
                        .map(|token| format!("'{}'", token.into_inner()))
                        .unwrap_or_else(|| "end of input".to_owned());

                    Diagnostic {
                        severity: Severity::Error,
                        message: format!("unexpected {found}"),
                        labels: vec![DiagLabel::new_primary(format!("expected {expected}"), span)],
                        help: None,
                        note: None,
                    }
                }
            }
        })
        .collect::<Vec<_>>()
}

pub fn parse_source(source: &Source) -> DiagResult<UnresolvedLayout<'_>> {
    let (output, errors) = layout_parser().parse(&source.content).into_output_errors();

    match output {
        Some(unresolved_layout) => {
            let warnings = map_errors(errors, false);

            Ok(DiagSuccess::with_warnings(unresolved_layout, warnings))
        }
        None => {
            let Some(error) = map_errors(errors, true).into_iter().next() else {
                unreachable!("failed parse produced no diagnostics")
            };

            Err(error)
        }
    }
}
