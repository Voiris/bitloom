use chumsky::span::{SimpleSpan, SimpleSpanned};

use super::context::Context;
use super::resolved::{KeywordValue, ResolvedValue, SizedValue, ValueType};
use crate::diagnostics::{DiagLabel, DiagResult, DiagSuccess, Diagnostic, Severity};
use crate::parser::unresolved::{BinaryOperation, UnaryOperation, UnresolvedValue};

pub(super) fn resolve_value<'src>(
    value: &SimpleSpanned<UnresolvedValue<'src>>,
    context: &Context<'src>,
) -> DiagResult<SimpleSpanned<ResolvedValue<'src>>> {
    let resolved_value = match &value.inner {
        UnresolvedValue::HexArray(hex_array) => {
            ResolvedValue::HexArray(SizedValue::new_unsized(hex_array.clone()))
        }
        UnresolvedValue::Size(size) => {
            let Some(value) = size.value.checked_mul(size.unit.multiplier()) else {
                return Err(Diagnostic::new(Severity::Error, "size is too large".to_string())
                    .add_label(DiagLabel::new_primary(
                        format!("multiplication by this unit overflows {}", i64::MAX),
                        value.span,
                    ))
                    .with_help("use a smaller size".to_string()));
            };

            ResolvedValue::Size(value)
        }
        UnresolvedValue::String(string) => ResolvedValue::String(string),
        UnresolvedValue::Identifier(ident) => match KeywordValue::from_keyword(ident) {
            Some(keyword) => ResolvedValue::Keyword(keyword),
            None => context.get_const(ident, value.span)?.value.clone(),
        },
        UnresolvedValue::BuiltinConst(builtin) => {
            context.get_builtin(builtin, value.span)?.value.clone()
        }
        UnresolvedValue::Clarification { name, args } => {
            return resolve_clarification(name, args, value.span, context);
        }
        UnresolvedValue::UnaryExpr { op, operand } => {
            return resolve_unary_expression(*op, operand, context);
        }
        UnresolvedValue::BinaryExpr { op, lhs, rhs } => {
            return resolve_binary_expression(*op, lhs, rhs, context);
        }
    };

    Ok(DiagSuccess::without_warnings(SimpleSpanned { inner: resolved_value, span: value.span }))
}

fn resolve_clarification<'src>(
    name: &'src str,
    args: &[SimpleSpanned<UnresolvedValue<'src>>],
    span: SimpleSpan<usize>,
    context: &Context<'src>,
) -> DiagResult<SimpleSpanned<ResolvedValue<'src>>> {
    match name {
        "sized" => resolve_sized(args, span, context),

        _ => Err(Diagnostic::new(Severity::Error, format!("unknown clarification `{name}`"))
            .add_label(DiagLabel::new_primary(
                "this clarification does not exist".to_string(),
                span,
            ))),
    }
}

fn resolve_sized<'src>(
    args: &[SimpleSpanned<UnresolvedValue<'src>>],
    span: SimpleSpan<usize>,
    context: &Context<'src>,
) -> DiagResult<SimpleSpanned<ResolvedValue<'src>>> {
    let [value_arg, size_arg] = args else {
        return Err(Diagnostic::new(
            Severity::Error,
            format!("expected 2 arguments, but got {}", args.len()),
        )
        .add_label(DiagLabel::new_primary("incorrect number of arguments".to_owned(), span)));
    };

    let value_success = resolve_value(value_arg, context)?;
    let size_success = resolve_value(size_arg, context)?;

    let mut warnings = value_success.warnings.unwrap_or_default();
    warnings.extend(size_success.warnings.unwrap_or_default());

    let value_span = value_success.value.span;
    let size_span = size_success.value.span;

    let value = value_success.value.inner.cast(ValueType::HexArray, &[], value_span)?;
    let size = size_success.value.inner.cast(ValueType::Sizable, &[], size_span)?;

    let ResolvedValue::HexArray(mut value) = value else {
        unreachable!();
    };
    let size = match size {
        ResolvedValue::HexInt(size) | ResolvedValue::Size(size) => size,
        _ => unreachable!(),
    };

    let byte_size = usize::try_from(expect_non_negative(size, size_span)?).map_err(|_| {
        Diagnostic::new(Severity::Error, "size is too large".to_owned()).add_label(
            DiagLabel::new_primary(
                "size does not fit in the target address space".to_owned(),
                size_span,
            ),
        )
    })?;

    if value.value.len() > byte_size {
        return Err(Diagnostic::new(Severity::Error, "declared size is too small".to_owned())
            .add_label(DiagLabel::new_primary(
                format!(
                    "size must be at least {} byte{}",
                    value.value.len(),
                    if value.value.len() == 1 { "" } else { "s" },
                ),
                size_span,
            )));
    }

    value.byte_size = Some(byte_size);

    Ok(DiagSuccess::with_warnings(
        SimpleSpanned { inner: ResolvedValue::HexArray(value), span },
        warnings,
    ))
}

fn resolve_binary_expression<'src>(
    operation: BinaryOperation,
    lhs: &SimpleSpanned<UnresolvedValue<'src>>,
    rhs: &SimpleSpanned<UnresolvedValue<'src>>,
    context: &Context<'src>,
) -> DiagResult<SimpleSpanned<ResolvedValue<'src>>> {
    let span = SimpleSpan { start: lhs.span.start, end: rhs.span.end, context: () };
    let rhs_span = rhs.span;

    let lhs_success = resolve_value(lhs, context)?;
    let rhs_success = resolve_value(rhs, context)?;

    let mut warnings = lhs_success.warnings.unwrap_or_default();
    warnings.extend(rhs_success.warnings.unwrap_or_default());

    let lhs = lhs_success.value.inner.cast(ValueType::HexInt, &[], lhs_success.value.span)?;
    let rhs = rhs_success.value.inner.cast(ValueType::HexInt, &[], rhs_success.value.span)?;

    let (ResolvedValue::HexInt(lhs_value), ResolvedValue::HexInt(rhs_value)) = (lhs, rhs) else {
        unreachable!();
    };

    let value =
        match operation {
            BinaryOperation::Add => {
                let value = lhs_value.checked_add(rhs_value).ok_or_else(|| {
                    Diagnostic::new(Severity::Error, "integer addition overflow".to_owned())
                        .add_label(DiagLabel::new_primary(
                            "the result does not fit in a signed 64-bit integer".to_owned(),
                            span,
                        ))
                })?;

                ResolvedValue::HexInt(value)
            }
            BinaryOperation::Sub => {
                let value = lhs_value.checked_sub(rhs_value).ok_or_else(|| {
                    Diagnostic::new(Severity::Error, "integer subtraction overflow".to_owned())
                        .add_label(DiagLabel::new_primary(
                            "the result does not fit in a signed 64-bit integer".to_owned(),
                            span,
                        ))
                })?;

                ResolvedValue::HexInt(value)
            }
            BinaryOperation::Mul => {
                let value = lhs_value.checked_mul(rhs_value).ok_or_else(|| {
                    Diagnostic::new(Severity::Error, "integer multiplication overflow".to_owned())
                        .add_label(DiagLabel::new_primary(
                            "the result does not fit in a signed 64-bit integer".to_owned(),
                            span,
                        ))
                })?;

                ResolvedValue::HexInt(value)
            }
            BinaryOperation::Div => {
                if rhs_value == 0 {
                    return Err(Diagnostic::new(Severity::Error, "division by zero".to_owned())
                        .add_label(DiagLabel::new_primary(
                            "the divisor evaluates to zero".to_owned(),
                            rhs_span,
                        )));
                }

                let value = lhs_value.checked_div(rhs_value).ok_or_else(|| {
                    Diagnostic::new(Severity::Error, "integer division overflow".to_owned())
                        .add_label(DiagLabel::new_primary(
                            "the result does not fit in a signed 64-bit integer".to_owned(),
                            span,
                        ))
                })?;

                ResolvedValue::HexInt(value)
            }
            BinaryOperation::Mod => {
                if rhs_value == 0 {
                    return Err(Diagnostic::new(Severity::Error, "remainder by zero".to_owned())
                        .add_label(DiagLabel::new_primary(
                            "the divisor evaluates to zero".to_owned(),
                            rhs_span,
                        )));
                }

                let value = lhs_value.checked_rem(rhs_value).ok_or_else(|| {
                    Diagnostic::new(Severity::Error, "integer remainder overflow".to_owned())
                        .add_label(DiagLabel::new_primary(
                            "the operation cannot be represented by a signed 64-bit integer"
                                .to_owned(),
                            span,
                        ))
                })?;

                ResolvedValue::HexInt(value)
            }
        };

    Ok(DiagSuccess::with_warnings(SimpleSpanned { inner: value, span }, warnings))
}

fn resolve_unary_expression<'src>(
    operation: SimpleSpanned<UnaryOperation>,
    operand: &SimpleSpanned<UnresolvedValue<'src>>,
    context: &Context<'src>,
) -> DiagResult<SimpleSpanned<ResolvedValue<'src>>> {
    let span = SimpleSpan { start: operation.span.start, end: operand.span.end, context: () };

    let operand_success = resolve_value(operand, context)?;

    let operand =
        operand_success.value.inner.cast(ValueType::HexInt, &[], operand_success.value.span)?;

    let ResolvedValue::HexInt(operand_value) = operand else {
        unreachable!();
    };

    let value = match operation.inner {
        UnaryOperation::Neg => operand_value.checked_neg().ok_or_else(|| {
            Diagnostic::new(Severity::Error, "integer negation overflow".to_owned()).add_label(
                DiagLabel::new_primary(
                    "the result does not fit in a signed 64-bit integer".to_owned(),
                    span,
                ),
            )
        })?,
        UnaryOperation::Pos => operand_value,
    };

    Ok(DiagSuccess::new(
        SimpleSpanned { inner: ResolvedValue::HexInt(value), span },
        operand_success.warnings,
    ))
}

pub(super) fn expect_args<T, const N: usize>(
    args: Box<[T]>,
    span: SimpleSpan<usize>,
) -> Result<[T; N], Diagnostic> {
    let boxed: Box<[T; N]> = args.try_into().map_err(|args: Box<[T]>| {
        Diagnostic::new(
            Severity::Error,
            format!(
                "expected {} argument{}, but got {}",
                N,
                if N == 1 { "" } else { "s" },
                args.len(),
            ),
        )
        .add_label(DiagLabel::new_primary("incorrect number of arguments".to_string(), span))
    })?;

    Ok(*boxed)
}

pub(super) fn expect_non_negative(value: i64, span: SimpleSpan<usize>) -> Result<u64, Diagnostic> {
    u64::try_from(value).map_err(|_| {
        Diagnostic::new(Severity::Error, "value out of range".to_owned()).add_label(
            DiagLabel::new_primary(
                format!("value must be in the range 0..=0x{:X}", i64::MAX,),
                span,
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::unresolved::{Size, SizeUnit};

    fn operand(value: i64, start: usize) -> SimpleSpanned<UnresolvedValue<'static>> {
        SimpleSpanned {
            inner: UnresolvedValue::Size(Size { value, unit: SizeUnit::Bytes }),
            span: SimpleSpan { start, end: start + 1, context: () },
        }
    }

    fn hex_array(bytes: &[u8], start: usize) -> SimpleSpanned<UnresolvedValue<'static>> {
        SimpleSpanned {
            inner: UnresolvedValue::HexArray(bytes.to_vec().into_boxed_slice()),
            span: SimpleSpan { start, end: start + bytes.len(), context: () },
        }
    }

    #[test]
    fn resolves_sized_hex_array() {
        let args = [hex_array(&[0x12, 0x34], 6), operand(4, 10)];
        let span = SimpleSpan { start: 0, end: 12, context: () };

        let resolved = resolve_sized(&args, span, &Context::default())
            .expect("hex array should support an explicit size");

        let ResolvedValue::HexArray(value) = resolved.value.inner else {
            panic!("sized should preserve the hex array type");
        };

        assert_eq!(&*value.value, &[0x12, 0x34]);
        assert_eq!(value.byte_size, Some(4));
        assert_eq!(resolved.value.span, span);
    }

    #[test]
    fn rejects_sized_for_unsupported_value_type() {
        let args = [operand(1, 6), operand(4, 10)];

        let error = resolve_sized(
            &args,
            SimpleSpan { start: 0, end: 12, context: () },
            &Context::default(),
        )
        .expect_err("only hex arrays should support sized");

        assert_eq!(error.message, "expected hex array");
    }

    #[test]
    fn rejects_size_smaller_than_hex_array() {
        let args = [hex_array(&[0x12, 0x34], 6), operand(1, 10)];

        let error = resolve_sized(
            &args,
            SimpleSpan { start: 0, end: 12, context: () },
            &Context::default(),
        )
        .expect_err("declared size should fit the hex array");

        assert_eq!(error.message, "declared size is too small");
    }

    #[test]
    fn resolves_all_binary_operations_as_hex_ints() {
        let cases = [
            (BinaryOperation::Add, 26),
            (BinaryOperation::Sub, 14),
            (BinaryOperation::Mul, 120),
            (BinaryOperation::Div, 3),
            (BinaryOperation::Mod, 2),
        ];

        for (operation, expected) in cases {
            let resolved = resolve_binary_expression(
                operation,
                &operand(20, 0),
                &operand(6, 2),
                &Context::default(),
            )
            .expect("binary expression should resolve");

            let ResolvedValue::HexInt(value) = resolved.value.inner else {
                panic!("binary expression should produce a hex int");
            };

            assert_eq!(value, expected);
        }
    }

    #[test]
    fn rejects_zero_divisor() {
        for operation in [BinaryOperation::Div, BinaryOperation::Mod] {
            let error = resolve_binary_expression(
                operation,
                &operand(20, 0),
                &operand(0, 2),
                &Context::default(),
            )
            .expect_err("zero divisor should be rejected");

            assert!(error.message.ends_with("by zero"));
            assert_eq!(error.labels[0].span, operand(0, 2).span);
        }
    }

    #[test]
    fn rejects_binary_operation_overflow() {
        let error = resolve_binary_expression(
            BinaryOperation::Add,
            &operand(i64::MAX, 0),
            &operand(1, 2),
            &Context::default(),
        )
        .expect_err("overflow should be rejected");

        assert_eq!(error.message, "integer addition overflow");
    }
}
