use chumsky::input::MapExtra;
use chumsky::prelude::{Rich, choice, just, none_of, recursive, text};
use chumsky::span::SimpleSpanned;
use chumsky::{IterParser, Parser, pratt};

use super::literals::{parse_hex_digits, parse_size_unit, parse_uint_literal};
use super::syntax::{Extra, spanned_token, token};
use super::unresolved::{BinaryOperation, Size, UnaryOperation, UnresolvedValue};
use crate::diagnostics::{Diagnostic, Severity};

fn unary_expr<'src>(
    op: SimpleSpanned<UnaryOperation>,
    operand: SimpleSpanned<UnresolvedValue<'src>>,
    extra: &mut MapExtra<'src, '_, &'src str, Extra<'src>>,
) -> SimpleSpanned<UnresolvedValue<'src>> {
    SimpleSpanned {
        inner: UnresolvedValue::UnaryExpr { op, operand: Box::new(operand) },
        span: extra.span(),
    }
}

fn binary_expr<'src>(
    lhs: SimpleSpanned<UnresolvedValue<'src>>,
    op: BinaryOperation,
    rhs: SimpleSpanned<UnresolvedValue<'src>>,
    extra: &mut MapExtra<'src, '_, &'src str, Extra<'src>>,
) -> SimpleSpanned<UnresolvedValue<'src>> {
    SimpleSpanned {
        inner: UnresolvedValue::BinaryExpr { op, lhs: Box::new(lhs), rhs: Box::new(rhs) },
        span: extra.span(),
    }
}

pub(super) fn parse_value<'src>()
-> impl Parser<'src, &'src str, SimpleSpanned<UnresolvedValue<'src>>, Extra<'src>> + Clone {
    recursive(|value| {
        let identifier = spanned_token(text::ascii::ident().map(UnresolvedValue::Identifier));

        let builtin_identifier = spanned_token(
            just('$').ignore_then(text::ascii::ident()).map(UnresolvedValue::BuiltinConst),
        );

        let hex_array = spanned_token(
            just("0x").ignore_then(parse_hex_digits()).map(UnresolvedValue::HexArray),
        );

        let size = spanned_token(
            parse_uint_literal()
                .try_map(|uint, span| {
                    if uint > i64::MAX as u64 {
                        return Err(Rich::custom(
                            span,
                            Diagnostic::new(
                                Severity::Error,
                                "integer literal is out of range".to_owned(),
                            )
                            .with_note("the maximum value is 9223372036854775807".to_owned()),
                        ));
                    }

                    Ok(uint as i64)
                })
                .then(parse_size_unit())
                .map(|(value, unit)| Size { value, unit })
                .map(UnresolvedValue::Size),
        );

        let clarification = spanned_token(
            token(text::ascii::ident())
                .then(
                    value
                        .clone()
                        .separated_by(token(just(',')))
                        .allow_trailing()
                        .collect::<Vec<_>>()
                        .map(Vec::into_boxed_slice)
                        .delimited_by(token(just('(')), token(just(')'))),
                )
                .map(|(name, args)| UnresolvedValue::Clarification { name, args }),
        );

        let parenthesized = value.clone().delimited_by(token(just('(')), token(just(')')));

        let string = spanned_token(
            none_of(['"', '\n', '\r'])
                .repeated()
                .to_slice()
                .delimited_by(just('"'), just('"'))
                .map(UnresolvedValue::String),
        );

        let atom = choice((
            clarification,
            identifier,
            builtin_identifier,
            hex_array,
            size,
            parenthesized,
            string,
        ));

        atom.pratt((
            pratt::prefix(5, spanned_token(just('-').to(UnaryOperation::Neg)), unary_expr),
            pratt::prefix(5, spanned_token(just('+').to(UnaryOperation::Pos)), unary_expr),
            pratt::infix(pratt::left(4), token(just('*')).to(BinaryOperation::Mul), binary_expr),
            pratt::infix(pratt::left(4), token(just('/')).to(BinaryOperation::Div), binary_expr),
            pratt::infix(pratt::left(4), token(just('%')).to(BinaryOperation::Mod), binary_expr),
            pratt::infix(pratt::left(3), token(just('+')).to(BinaryOperation::Add), binary_expr),
            pratt::infix(pratt::left(3), token(just('-')).to(BinaryOperation::Sub), binary_expr),
        ))
    })
}

pub(super) fn parse_value_args<'src>()
-> impl Parser<'src, &'src str, Box<[SimpleSpanned<UnresolvedValue<'src>>]>, Extra<'src>> + Clone {
    parse_value()
        .separated_by(token(just(',')))
        .allow_trailing()
        .collect::<Vec<_>>()
        .map(Vec::into_boxed_slice)
        .delimited_by(token(just('(')), token(just(')')))
}
