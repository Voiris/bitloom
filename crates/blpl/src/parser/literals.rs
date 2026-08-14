use chumsky::prelude::{Rich, choice, just, text};
use chumsky::{IterParser, Parser};

use super::syntax::Extra;
use crate::diagnostics::{Diagnostic, Severity};
use crate::parser::unresolved::SizeUnit;

pub(super) fn parse_hex_digits<'src>()
-> impl Parser<'src, &'src str, Box<[u8]>, Extra<'src>> + Clone {
    text::digits(16).to_slice().separated_by(just('_')).at_least(1).collect::<Vec<&'src str>>().map(
        |groups| {
            let digits = groups
                .into_iter()
                .flat_map(str::bytes)
                .map(|digit| match digit {
                    b'0'..=b'9' => digit - b'0',
                    b'a'..=b'f' => digit - b'a' + 10,
                    b'A'..=b'F' => digit - b'A' + 10,
                    _ => unreachable!(),
                })
                .collect::<Vec<_>>();

            let mut bytes = Vec::with_capacity(digits.len().div_ceil(2));
            let mut magnitude_started = false;
            let mut index = 0;

            if digits.len() % 2 != 0 {
                let byte = digits[0];

                if byte != 0 {
                    bytes.push(byte);
                    magnitude_started = true;
                }

                index = 1;
            }

            while index < digits.len() {
                let byte = (digits[index] << 4) | digits[index + 1];

                if byte != 0 || magnitude_started {
                    bytes.push(byte);
                    magnitude_started = true;
                }

                index += 2;
            }

            if bytes.is_empty() {
                bytes.push(0);
            }

            bytes.into_boxed_slice()
        },
    )
}

pub(super) fn parse_uint_literal<'src>() -> impl Parser<'src, &'src str, u64, Extra<'src>> + Clone {
    text::digits(10).to_slice().separated_by(just('_')).at_least(1).collect::<String>().try_map(
        |str_uint, span| {
            str_uint.parse::<u64>().map_err(|_| {
                Rich::custom(
                    span,
                    Diagnostic::new(
                        Severity::Error,
                        "decimal integer literal is out of range".to_owned(),
                    )
                    .with_note(format!("the maximum value is {}", u64::MAX)),
                )
            })
        },
    )
}

pub(super) fn parse_size_unit<'src>() -> impl Parser<'src, &'src str, SizeUnit, Extra<'src>> + Clone
{
    choice((
        just("B").to(SizeUnit::Bytes),
        just("KiB").to(SizeUnit::KiB),
        just("MiB").to(SizeUnit::MiB),
        just("GiB").to(SizeUnit::GiB),
    ))
}
