use chumsky::Parser;
use chumsky::prelude::{just, text};
use chumsky::span::SimpleSpanned;

use crate::parser::syntax::{Extra, spanned, spanned_token};
use crate::parser::unresolved::Directive;
use crate::parser::value::parse_value_args;

pub(super) fn directive<'src>()
-> impl Parser<'src, &'src str, SimpleSpanned<Directive<'src>>, Extra<'src>> + Clone {
    spanned_token(
        just('@')
            .ignore_then(spanned(text::ascii::ident()))
            .then(parse_value_args())
            .then_ignore(just(';'))
            .map(|(name, args)| Directive { name, args }),
    )
}
