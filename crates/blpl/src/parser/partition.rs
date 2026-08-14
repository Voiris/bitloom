use chumsky::prelude::{just, text};
use chumsky::span::SimpleSpanned;
use chumsky::{IterParser, Parser};

use crate::parser::syntax::{Extra, spanned, spanned_token, token};
use crate::parser::unresolved::{Clarification, UnresolvedPartition};
use crate::parser::value::parse_value_args;

pub(super) fn clarification<'src>()
-> impl Parser<'src, &'src str, SimpleSpanned<Clarification<'src>>, Extra<'src>> + Clone {
    spanned_token(
        spanned(text::ascii::ident())
            .then(parse_value_args())
            .map(|(name, args)| Clarification { name, args }),
    )
}

pub(super) fn partition<'src>()
-> impl Parser<'src, &'src str, SimpleSpanned<UnresolvedPartition<'src>>, Extra<'src>> + Clone {
    spanned_token(
        spanned(text::ascii::ident())
            .then(
                clarification()
                    .separated_by(token(just(',')))
                    .allow_trailing()
                    .at_least(1)
                    .collect::<Box<[_]>>()
                    .delimited_by(token(just(':')), just(';')),
            )
            .map(|(name, clarifications)| UnresolvedPartition { name, clarifications }),
    )
}
