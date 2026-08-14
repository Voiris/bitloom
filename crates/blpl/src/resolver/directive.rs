use std::collections::HashMap;

use chumsky::span::{SimpleSpan, SimpleSpanned};

use super::context::Context;
use super::resolved::{KeywordValue, ResolvedValue, ValueType};
use super::value::{expect_args, expect_non_negative, resolve_value};
use crate::diagnostics::{DiagLabel, DiagResult, DiagSuccess, Diagnostic, Severity};
use crate::parser::unresolved::{Directive, UnresolvedValue};

const DIRECTIVE_ORDER: &[&str] = &["flash_size", "erase_size", "offset_align"];
const DEFAULT_ERASE_SIZE: u64 = 0x1000;

struct DirectiveEntry<'declaration, 'src> {
    name_span: SimpleSpan,
    declaration_span: SimpleSpan,
    args: &'declaration [SimpleSpanned<UnresolvedValue<'src>>],
}

pub(super) fn resolve_directives<'src>(
    declarations: &[SimpleSpanned<Directive<'src>>],
    context: &mut Context<'src>,
) -> DiagResult<()> {
    let mut entries = HashMap::with_capacity(declarations.len());
    let mut warnings = Vec::new();
    let mut latest_ordered_directive: Option<(usize, &str, SimpleSpan)> = None;

    for declaration in declarations {
        let name = declaration.name.inner;
        let Some(order) = directive_order(name) else {
            return Err(unknown_directive(name, declaration.name.span));
        };

        if let Some(previous) = entries.get(name) {
            return Err(duplicate_directive(declaration, previous));
        }

        if let Some((latest_order, latest_name, latest_span)) = latest_ordered_directive {
            if order < latest_order {
                warnings.push(out_of_order_directive(
                    name,
                    declaration.name.span,
                    latest_name,
                    latest_span,
                ));
            } else {
                latest_ordered_directive = Some((order, name, declaration.name.span));
            }
        } else {
            latest_ordered_directive = Some((order, name, declaration.name.span));
        }

        entries.insert(
            name,
            DirectiveEntry {
                name_span: declaration.name.span,
                declaration_span: declaration.span,
                args: &declaration.args,
            },
        );
    }

    for name in DIRECTIVE_ORDER {
        let Some(entry) = entries.remove(name) else {
            continue;
        };

        let resolved_args = resolve_args(entry.args, context, &mut warnings)?;
        let resolved = resolve_directive(
            name,
            resolved_args,
            entry.declaration_span,
            entry.name_span,
            context,
        )?;

        if let Some(mut directive_warnings) = resolved.warnings {
            warnings.append(&mut directive_warnings);
        }
    }

    Ok(DiagSuccess::with_warnings((), warnings))
}

fn resolve_args<'src>(
    args: &[SimpleSpanned<UnresolvedValue<'src>>],
    context: &Context<'src>,
    warnings: &mut Vec<Diagnostic>,
) -> Result<Box<[SimpleSpanned<ResolvedValue<'src>>]>, Diagnostic> {
    let mut resolved_args = Vec::with_capacity(args.len());

    for arg in args {
        let resolved = resolve_value(arg, context)?;

        resolved_args.push(resolved.value);
        if let Some(mut value_warnings) = resolved.warnings {
            warnings.append(&mut value_warnings);
        }
    }

    Ok(resolved_args.into_boxed_slice())
}

fn resolve_directive<'src>(
    name: &str,
    args: Box<[SimpleSpanned<ResolvedValue<'src>>]>,
    declaration_span: SimpleSpan,
    name_span: SimpleSpan,
    context: &mut Context<'src>,
) -> DiagResult<()> {
    match name {
        "flash_size" => resolve_flash_size(args, declaration_span, context)?,
        "erase_size" => resolve_erase_size(args, declaration_span, context)?,
        "offset_align" => resolve_offset_align(args, declaration_span, context)?,
        _ => return Err(unknown_directive(name, name_span)),
    }

    Ok(DiagSuccess::without_warnings(()))
}

fn resolve_flash_size<'src>(
    args: Box<[SimpleSpanned<ResolvedValue<'src>>]>,
    span: SimpleSpan,
    context: &mut Context<'src>,
) -> Result<(), Diagnostic> {
    let [arg] = expect_args::<_, 1>(args, span)?;
    let size = arg.inner.cast(ValueType::Sizable, &[], arg.span)?;

    let value = match size {
        ResolvedValue::HexInt(value) | ResolvedValue::Size(value) => value,
        _ => unreachable!(),
    };

    context.flash.size = Some(expect_non_negative(value, arg.span)?);
    Ok(())
}

fn resolve_erase_size<'src>(
    args: Box<[SimpleSpanned<ResolvedValue<'src>>]>,
    span: SimpleSpan,
    context: &mut Context<'src>,
) -> Result<(), Diagnostic> {
    let [arg] = expect_args::<_, 1>(args, span)?;
    let size = arg.inner.cast(ValueType::Sizable, &[KeywordValue::Default], arg.span)?;

    let value = match size {
        ResolvedValue::HexInt(value) | ResolvedValue::Size(value) => {
            expect_non_negative(value, arg.span)?
        }
        ResolvedValue::Keyword(KeywordValue::Default) => DEFAULT_ERASE_SIZE,
        _ => unreachable!(),
    };

    context.flash.erase_size = Some(value);
    Ok(())
}

fn resolve_offset_align<'src>(
    args: Box<[SimpleSpanned<ResolvedValue<'src>>]>,
    span: SimpleSpan,
    context: &mut Context<'src>,
) -> Result<(), Diagnostic> {
    let [arg] = expect_args::<_, 1>(args, span)?;
    let alignment = arg.inner.cast(ValueType::Sizable, &[KeywordValue::Auto], arg.span)?;

    let value = match alignment {
        ResolvedValue::HexInt(value) | ResolvedValue::Size(value) => {
            expect_non_negative(value, arg.span)?
        }
        ResolvedValue::Keyword(KeywordValue::Auto) => {
            let Some(erase_size) = context.flash.erase_size else {
                return Err(Diagnostic::new(
                    Severity::Error,
                    "cannot determine automatic offset alignment".to_owned(),
                )
                .add_label(DiagLabel::new_primary(
                    "automatic alignment depends on the erase size".to_owned(),
                    arg.span,
                ))
                .with_help("specify `erase_size` before `offset_align`".to_owned()));
            };

            erase_size
        }
        _ => unreachable!(),
    };

    context.layout.offset_align = Some(value);
    Ok(())
}

fn directive_order(name: &str) -> Option<usize> {
    DIRECTIVE_ORDER.iter().position(|candidate| *candidate == name)
}

fn duplicate_directive(
    declaration: &SimpleSpanned<Directive<'_>>,
    previous: &DirectiveEntry<'_, '_>,
) -> Diagnostic {
    Diagnostic::new(
        Severity::Error,
        format!("directive `{}` is already specified", declaration.name.inner),
    )
    .add_label(DiagLabel::new_primary(
        "directive is respecified here".to_owned(),
        declaration.name.span,
    ))
    .add_label(DiagLabel::new_secondary(
        "previous definition is here".to_owned(),
        previous.name_span,
    ))
    .with_help("a directive may only be specified once".to_owned())
}

fn unknown_directive(name: &str, span: SimpleSpan) -> Diagnostic {
    Diagnostic::new(Severity::Error, format!("unknown directive `{name}`"))
        .add_label(DiagLabel::new_primary("this directive does not exist".to_owned(), span))
}

fn out_of_order_directive(
    name: &str,
    span: SimpleSpan,
    previous_name: &str,
    previous_span: SimpleSpan,
) -> Diagnostic {
    Diagnostic::new(Severity::Warning, format!("directive `{name}` is out of order"))
        .add_label(DiagLabel::new_primary(format!("`{name}` should be declared earlier"), span))
        .add_label(DiagLabel::new_secondary(
            format!("`{previous_name}` was declared here"),
            previous_span,
        ))
        .with_help(format!("use directive order: {}", DIRECTIVE_ORDER.join(", ")))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::Source;
    use crate::parser::parse_source;

    fn source(content: &str) -> Source {
        Source { path: PathBuf::from("directives.blpl"), content: content.to_owned() }
    }

    #[test]
    fn uses_default_erase_size_for_automatic_offset_alignment() {
        let source = source("@erase_size(default); @offset_align(auto);");
        let parsed = parse_source(&source).expect("directives should parse");
        let mut context = Context::default();

        let resolved = resolve_directives(&parsed.value.directives, &mut context)
            .expect("directives should resolve");

        assert!(resolved.warnings.is_none());
        assert_eq!(context.flash.erase_size, Some(DEFAULT_ERASE_SIZE));
        assert_eq!(context.layout.offset_align, Some(DEFAULT_ERASE_SIZE));
    }

    #[test]
    fn resolves_directives_in_canonical_order() {
        let source = source(
            "@offset_align(auto);\n\
             @erase_size(8KiB);\n\
             @flash_size(16MiB);",
        );
        let parsed = parse_source(&source).expect("directives should parse");
        let mut context = Context::default();

        let resolved = resolve_directives(&parsed.value.directives, &mut context)
            .expect("directives should resolve");
        let warnings = resolved.warnings.expect("out-of-order directives should warn");

        assert_eq!(warnings.len(), 2);
        assert!(warnings.iter().all(|warning| warning.severity == Severity::Warning));
        assert_eq!(context.flash.size, Some(16 * 1024 * 1024));
        assert_eq!(context.flash.erase_size, Some(8 * 1024));
        assert_eq!(context.layout.offset_align, Some(8 * 1024));
    }

    #[test]
    fn accepts_canonical_directive_order_without_warnings() {
        let source = source(
            "@flash_size(16MiB);\n\
             @erase_size(8KiB);\n\
             @offset_align(auto);",
        );
        let parsed = parse_source(&source).expect("directives should parse");
        let mut context = Context::default();

        let resolved = resolve_directives(&parsed.value.directives, &mut context)
            .expect("directives should resolve");

        assert!(resolved.warnings.is_none());
        assert_eq!(context.layout.offset_align, Some(8 * 1024));
    }

    #[test]
    fn rejects_automatic_alignment_without_erase_size() {
        let source = source("@offset_align(auto);");
        let parsed = parse_source(&source).expect("directive should parse");
        let mut context = Context::default();

        let error = resolve_directives(&parsed.value.directives, &mut context)
            .expect_err("automatic alignment should require an erase size");

        assert_eq!(error.message, "cannot determine automatic offset alignment");
    }
}
