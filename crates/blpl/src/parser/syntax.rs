use chumsky::Parser;
use chumsky::prelude::{Rich, SimpleSpan, any, choice, extra, just, none_of, one_of, text};
use chumsky::span::SimpleSpanned;

use crate::diagnostics::Diagnostic;

pub(super) type Extra<'src> = extra::Err<Rich<'src, char, SimpleSpan<usize>, Diagnostic>>;

pub(super) fn ignored<'src>() -> impl Parser<'src, &'src str, (), Extra<'src>> + Clone {
    let whitespace = one_of(" \t\r\n").ignored();

    let line_comment = just("//").then(none_of('\n').repeated()).ignored();

    let block_comment = just("/*")
        .ignore_then(any().and_is(just("*/").not()).repeated())
        .then_ignore(just("*/"))
        .ignored();

    choice((whitespace, line_comment, block_comment)).repeated().ignored()
}

pub(super) fn token<'src, O, P>(parser: P) -> impl Parser<'src, &'src str, O, Extra<'src>> + Clone
where
    P: Parser<'src, &'src str, O, Extra<'src>> + Clone,
{
    parser.padded_by(ignored())
}

pub(super) fn spanned<'src, O, P>(
    parser: P,
) -> impl Parser<'src, &'src str, SimpleSpanned<O>, Extra<'src>> + Clone
where
    P: Parser<'src, &'src str, O, Extra<'src>> + Clone,
{
    parser.map_with(|o, extra| SimpleSpanned { inner: o, span: extra.span() })
}

pub(super) fn spanned_token<'src, O, P>(
    parser: P,
) -> impl Parser<'src, &'src str, SimpleSpanned<O>, Extra<'src>> + Clone
where
    P: Parser<'src, &'src str, O, Extra<'src>> + Clone,
{
    spanned(parser).padded_by(ignored())
}

pub(super) fn identifier<'src>()
-> impl Parser<'src, &'src str, SimpleSpanned<&'src str>, Extra<'src>> + Clone {
    spanned_token(text::ascii::ident())
}
