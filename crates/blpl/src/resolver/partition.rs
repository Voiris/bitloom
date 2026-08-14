use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;

use chumsky::span::{SimpleSpan, SimpleSpanned};

use super::context::Context;
use super::resolved::{KeywordValue, ResolvedValue, Source, ValueType};
use super::value::{expect_non_negative, resolve_value};
use crate::diagnostics::{DiagLabel, DiagResult, DiagSuccess, Diagnostic, Severity};
use crate::parser::unresolved::{Clarification, UnresolvedPartition};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Placement {
    Auto,
    Exact(u64),
}

#[derive(Debug, Clone)]
pub(super) struct PartitionSpec<'src> {
    pub(super) name: &'src str,
    pub(super) declaration_span: SimpleSpan,
    pub(super) offset: SimpleSpanned<Placement>,
    pub(super) size: SimpleSpanned<Placement>,
    pub(super) magic: Box<[u8]>,
    pub(super) magic_span: Option<SimpleSpan>,
    pub(super) source: Source,
    pub(super) content_size: u64,
    pub(super) source_span: SimpleSpan,
}

#[derive(Default)]
struct PartitionFields {
    offset: Option<SimpleSpanned<Placement>>,
    size: Option<SimpleSpanned<Placement>>,
    magic: Option<(Box<[u8]>, SimpleSpan)>,
    source: Option<(Source, u64, SimpleSpan)>,
}

pub(super) fn resolve_partitions<'src>(
    partitions: &[SimpleSpanned<UnresolvedPartition<'src>>],
    context: &Context<'src>,
) -> DiagResult<Box<[PartitionSpec<'src>]>> {
    let mut names = HashMap::with_capacity(partitions.len());
    let mut resolved = Vec::with_capacity(partitions.len());
    let mut warnings = Vec::new();

    for partition in partitions {
        if let Some(previous_span) = names.insert(partition.name.inner, partition.name.span) {
            return Err(Diagnostic::new(
                Severity::Error,
                format!("partition `{}` is already defined", partition.name.inner),
            )
            .add_label(DiagLabel::new_primary(
                "partition is redefined here".to_owned(),
                partition.name.span,
            ))
            .add_label(DiagLabel::new_secondary(
                "previous definition is here".to_owned(),
                previous_span,
            )));
        }

        let fields = resolve_clarifications(&partition.clarifications, context)?;
        if let Some(mut field_warnings) = fields.warnings {
            warnings.append(&mut field_warnings);
        }

        let fields = fields.value;
        let default = SimpleSpanned { inner: Placement::Auto, span: partition.name.span };
        let Some((source, content_size, source_span)) = fields.source else {
            return Err(Diagnostic::new(
                Severity::Error,
                format!("partition `{}` has no content source", partition.name.inner),
            )
            .add_label(DiagLabel::new_primary(
                "a partition must have a content source and cannot be empty".to_owned(),
                partition.span,
            ))
            .with_help(
                "currently, `file(...)` is the only supported partition content source".to_owned(),
            ));
        };

        if content_size == 0 {
            return Err(Diagnostic::new(
                Severity::Error,
                format!("partition `{}` has an empty content source", partition.name.inner),
            )
            .add_label(DiagLabel::new_primary(
                "this source provides no partition data".to_owned(),
                source_span,
            ))
            .with_note("magic is metadata and does not count as partition content".to_owned()));
        }

        let (magic, magic_span) = match fields.magic {
            Some((magic, span)) => (magic, Some(span)),
            None => (Box::default(), None),
        };

        resolved.push(PartitionSpec {
            name: partition.name.inner,
            declaration_span: partition.span,
            offset: fields.offset.unwrap_or(default),
            size: fields.size.unwrap_or(default),
            magic,
            magic_span,
            source,
            content_size,
            source_span,
        });
    }

    Ok(DiagSuccess::with_warnings(resolved.into_boxed_slice(), warnings))
}

fn resolve_clarifications<'src>(
    clarifications: &[SimpleSpanned<Clarification<'src>>],
    context: &Context<'src>,
) -> DiagResult<PartitionFields> {
    let mut fields = PartitionFields::default();
    let mut seen = HashMap::with_capacity(clarifications.len());
    let mut warnings = Vec::new();

    for clarification in clarifications {
        let name = clarification.name.inner;
        if !matches!(name, "offset" | "size" | "magic" | "file") {
            return Err(Diagnostic::new(
                Severity::Error,
                format!("unknown partition clarification `{name}`"),
            )
            .add_label(DiagLabel::new_primary(
                "this clarification does not exist".to_owned(),
                clarification.name.span,
            )));
        }

        if let Some(previous_span) = seen.insert(name, clarification.name.span) {
            return Err(Diagnostic::new(
                Severity::Error,
                format!("clarification `{name}` is already specified"),
            )
            .add_label(DiagLabel::new_primary(
                "clarification is repeated here".to_owned(),
                clarification.name.span,
            ))
            .add_label(DiagLabel::new_secondary(
                "previous clarification is here".to_owned(),
                previous_span,
            )));
        }

        let value = resolve_argument(clarification, context)?;
        if let Some(mut value_warnings) = value.warnings {
            warnings.append(&mut value_warnings);
        }
        let value = value.value;

        match name {
            "offset" => fields.offset = Some(resolve_placement(value)?),
            "size" => fields.size = Some(resolve_placement(value)?),
            "magic" => fields.magic = Some(resolve_magic(value)?),
            "file" => {
                fields.source =
                    Some(resolve_file(value, clarification.name.span, &context.base_dir)?)
            }
            _ => unreachable!(),
        }
    }

    Ok(DiagSuccess::with_warnings(fields, warnings))
}

fn resolve_argument<'src>(
    clarification: &SimpleSpanned<Clarification<'src>>,
    context: &Context<'src>,
) -> DiagResult<SimpleSpanned<ResolvedValue<'src>>> {
    let [argument] = &*clarification.args else {
        return Err(Diagnostic::new(
            Severity::Error,
            format!("expected 1 argument, but got {}", clarification.args.len()),
        )
        .add_label(DiagLabel::new_primary(
            "incorrect number of arguments".to_owned(),
            clarification.span,
        )));
    };

    resolve_value(argument, context)
}

fn resolve_placement<'src>(
    value: SimpleSpanned<ResolvedValue<'src>>,
) -> Result<SimpleSpanned<Placement>, Diagnostic> {
    let span = value.span;
    let value = value.inner.cast(ValueType::Sizable, &[KeywordValue::Auto], span)?;
    let placement = match value {
        ResolvedValue::Keyword(KeywordValue::Auto) => Placement::Auto,
        ResolvedValue::HexInt(value) | ResolvedValue::Size(value) => {
            Placement::Exact(expect_non_negative(value, span)?)
        }
        _ => unreachable!(),
    };

    Ok(SimpleSpanned { inner: placement, span })
}

fn resolve_magic<'src>(
    value: SimpleSpanned<ResolvedValue<'src>>,
) -> Result<(Box<[u8]>, SimpleSpan), Diagnostic> {
    let span = value.span;
    let value = value.inner.cast(ValueType::HexArray, &[], span)?;
    let ResolvedValue::HexArray(value) = value else {
        unreachable!();
    };

    let byte_size = value.byte_size.unwrap_or(value.value.len());
    let mut magic = vec![0; byte_size];
    let padding = byte_size.checked_sub(value.value.len()).ok_or_else(|| {
        Diagnostic::new(Severity::Error, "magic size is too small".to_owned()).add_label(
            DiagLabel::new_primary("magic does not fit in its declared size".to_owned(), span),
        )
    })?;
    magic[padding..].copy_from_slice(&value.value);

    Ok((magic.into_boxed_slice(), span))
}

fn resolve_file<'src>(
    value: SimpleSpanned<ResolvedValue<'src>>,
    clarification_span: SimpleSpan,
    base_dir: &std::path::Path,
) -> Result<(Source, u64, SimpleSpan), Diagnostic> {
    let value_span = value.span;
    let path = match value.inner {
        ResolvedValue::String(path) => PathBuf::from(path),
        ResolvedValue::Path(path) => path,
        value => {
            let value = value.cast(ValueType::String, &[], value_span)?;
            let ResolvedValue::String(path) = value else {
                unreachable!();
            };
            PathBuf::from(path)
        }
    };

    let path = if path.is_relative() { base_dir.join(path) } else { path };
    let metadata = fs::metadata(&path).map_err(|error| {
        Diagnostic::new(Severity::Error, format!("cannot read partition file `{}`", path.display()))
            .add_label(DiagLabel::new_primary(
                "the file referenced by this `file` clarification is unavailable".to_owned(),
                clarification_span,
            ))
            .add_label(DiagLabel::new_secondary(error.to_string(), value_span))
    })?;

    if !metadata.is_file() {
        return Err(Diagnostic::new(
            Severity::Error,
            format!("partition source `{}` is not a file", path.display()),
        )
        .add_label(DiagLabel::new_primary(
            "this `file` clarification must reference a regular file".to_owned(),
            clarification_span,
        )));
    }

    Ok((Source::File(path), metadata.len(), clarification_span))
}
