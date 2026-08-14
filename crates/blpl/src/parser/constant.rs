use chumsky::Parser;
use chumsky::prelude::{just, one_of};
use chumsky::span::SimpleSpanned;

use crate::parser::syntax::{Extra, identifier, ignored, spanned_token, token};
use crate::parser::unresolved::ConstDecl;
use crate::parser::value::parse_value;

pub(super) fn const_decl<'src>()
-> impl Parser<'src, &'src str, SimpleSpanned<ConstDecl<'src>>, Extra<'src>> + Clone {
    spanned_token(
        just("const")
            .ignore_then(one_of(" \t\r\n"))
            .then_ignore(ignored())
            .ignore_then(identifier())
            .then(parse_value().delimited_by(token(just("=")), just(";")))
            .map(|(name, value)| ConstDecl { name, value }),
    )
}
