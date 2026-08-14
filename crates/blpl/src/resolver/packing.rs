use chumsky::span::SimpleSpan;

use super::context::{FlashConfig, LayoutConfig};
use super::partition::{PartitionSpec, Placement};
use super::resolved::{Layout, Partition};
use crate::diagnostics::{DiagLabel, DiagResult, DiagSuccess, Diagnostic, Severity};

struct PackedPartition {
    partition: Partition,
    offset_span: SimpleSpan,
    size_span: SimpleSpan,
}

pub(super) fn pack_layout(
    partitions: Box<[PartitionSpec<'_>]>,
    flash: &FlashConfig,
    config: &LayoutConfig,
) -> DiagResult<Layout> {
    let alignment = config.offset_align.unwrap_or(1);
    if alignment == 0 {
        let mut diagnostic = Diagnostic::new(
            Severity::Error,
            "offset alignment must be greater than zero".to_owned(),
        );
        if let Some(partition) = partitions.first() {
            diagnostic = diagnostic.add_label(DiagLabel::new_primary(
                "partitions cannot be placed with zero alignment".to_owned(),
                partition.declaration_span,
            ));
        }
        return Err(diagnostic);
    }

    let mut packed = Vec::with_capacity(partitions.len());
    let mut warnings = Vec::new();
    let mut cursor = 0u64;
    let mut previous_offset: Option<(&str, u64, SimpleSpan)> = None;

    for spec in partitions {
        let offset = match spec.offset.inner {
            Placement::Auto => align_up(cursor, alignment, spec.offset.span)?,
            Placement::Exact(offset) => {
                if offset % alignment != 0 {
                    return Err(Diagnostic::new(
                        Severity::Error,
                        format!("partition `{}` offset is not aligned", spec.name),
                    )
                    .add_label(DiagLabel::new_primary(
                        format!("offset must be a multiple of {alignment}"),
                        spec.offset.span,
                    )));
                }
                offset
            }
        };

        let magic_size = u64::try_from(spec.magic.len()).map_err(|_| {
            Diagnostic::new(Severity::Error, "magic value is too large".to_owned()).add_label(
                DiagLabel::new_primary(
                    "magic size does not fit in the target address space".to_owned(),
                    spec.magic_span.unwrap_or(spec.declaration_span),
                ),
            )
        })?;
        let required_size = spec.content_size.max(magic_size);
        let size = match spec.size.inner {
            Placement::Auto => required_size,
            Placement::Exact(size) => size,
        };

        ensure_content_fits(&spec, size, magic_size)?;

        let end = offset.checked_add(size).ok_or_else(|| {
            Diagnostic::new(
                Severity::Error,
                format!("partition `{}` end offset overflows", spec.name),
            )
            .add_label(DiagLabel::new_primary(
                "offset plus size does not fit in the target address space".to_owned(),
                spec.size.span,
            ))
        })?;

        if let Some(flash_size) = flash.size {
            if size > flash_size {
                return Err(Diagnostic::new(
                    Severity::Error,
                    format!("partition `{}` is larger than the flash", spec.name),
                )
                .add_label(DiagLabel::new_primary(
                    format!("partition size must not exceed {flash_size} bytes"),
                    spec.size.span,
                )));
            }
            if end > flash_size {
                return Err(Diagnostic::new(
                    Severity::Error,
                    format!("partition `{}` exceeds the flash size", spec.name),
                )
                .add_label(DiagLabel::new_primary(
                    format!("partition ends at {end}, but flash size is {flash_size}"),
                    spec.size.span,
                ))
                .add_label(DiagLabel::new_secondary(
                    "partition starts at this offset".to_owned(),
                    spec.offset.span,
                )));
            }
        }

        if let Some((previous_name, previous_value, previous_span)) = previous_offset
            && offset < previous_value
        {
            warnings.push(
                Diagnostic::new(
                    Severity::Warning,
                    format!("partition `{}` is declared out of offset order", spec.name),
                )
                .add_label(DiagLabel::new_primary(
                    format!("offset {offset} is lower than the previous partition offset"),
                    spec.offset.span,
                ))
                .add_label(DiagLabel::new_secondary(
                    format!("partition `{previous_name}` has offset {previous_value}"),
                    previous_span,
                ))
                .with_help("declare partitions in ascending offset order".to_owned()),
            );
        }
        previous_offset = Some((spec.name, offset, spec.offset.span));
        cursor = cursor.max(end);

        packed.push(PackedPartition {
            partition: Partition {
                name: spec.name.to_owned(),
                offset,
                size,
                magic: spec.magic,
                source: spec.source,
            },
            offset_span: spec.offset.span,
            size_span: spec.size.span,
        });
    }

    packed.sort_by_key(|entry| entry.partition.offset);
    validate_intervals(&packed)?;

    let partitions =
        packed.into_iter().map(|entry| entry.partition).collect::<Vec<_>>().into_boxed_slice();

    Ok(DiagSuccess::with_warnings(Layout { partitions }, warnings))
}

fn ensure_content_fits(
    spec: &PartitionSpec<'_>,
    partition_size: u64,
    magic_size: u64,
) -> Result<(), Diagnostic> {
    if spec.content_size > partition_size {
        return Err(Diagnostic::new(
            Severity::Error,
            format!("content source does not fit in partition `{}`", spec.name),
        )
        .add_label(DiagLabel::new_primary(
            format!(
                "partition is {partition_size} bytes, but its content requires {} bytes",
                spec.content_size,
            ),
            spec.size.span,
        ))
        .add_label(DiagLabel::new_secondary(
            "content source is specified here".to_owned(),
            spec.source_span,
        )));
    }

    if magic_size > partition_size {
        return Err(Diagnostic::new(
            Severity::Error,
            format!("magic does not fit in partition `{}`", spec.name),
        )
        .add_label(DiagLabel::new_primary(
            format!("partition is {partition_size} bytes, but magic is {magic_size} bytes"),
            spec.size.span,
        ))
        .add_label(DiagLabel::new_secondary(
            "magic is specified here".to_owned(),
            spec.magic_span.unwrap_or(spec.declaration_span),
        )));
    }

    Ok(())
}

fn validate_intervals(partitions: &[PackedPartition]) -> Result<(), Diagnostic> {
    for pair in partitions.windows(2) {
        let current = &pair[0];
        let next = &pair[1];
        let current_end = current
            .partition
            .offset
            .checked_add(current.partition.size)
            .expect("partition end was checked while packing");

        if current_end > next.partition.offset {
            return Err(Diagnostic::new(
                Severity::Error,
                format!(
                    "partition `{}` overlaps partition `{}`",
                    current.partition.name, next.partition.name,
                ),
            )
            .add_label(DiagLabel::new_primary(
                format!(
                    "this offset lies inside `{}`, which ends at {current_end}",
                    current.partition.name,
                ),
                next.offset_span,
            ))
            .add_label(DiagLabel::new_secondary(
                "this partition size crosses the next offset".to_owned(),
                current.size_span,
            )));
        }
    }

    Ok(())
}

fn align_up(value: u64, alignment: u64, span: SimpleSpan) -> Result<u64, Diagnostic> {
    let remainder = value % alignment;
    if remainder == 0 {
        return Ok(value);
    }

    value.checked_add(alignment - remainder).ok_or_else(|| {
        Diagnostic::new(Severity::Error, "aligned offset overflows".to_owned()).add_label(
            DiagLabel::new_primary(
                format!("cannot align this offset to a multiple of {alignment}"),
                span,
            ),
        )
    })
}
