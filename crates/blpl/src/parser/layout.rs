use chumsky::input::Emitter;
use chumsky::prelude::{Rich, SimpleSpan, choice};
use chumsky::span::SimpleSpanned;
use chumsky::{IterParser, Parser};

use super::syntax::{Extra, ignored};
use super::unresolved::{ConstDecl, Directive, UnresolvedLayout, UnresolvedPartition};
use crate::diagnostics::{DiagLabel, Diagnostic, Severity};
use crate::parser::constant::const_decl;
use crate::parser::directive::directive;
use crate::parser::partition::partition;

#[derive(Debug)]
enum Item<'src> {
    Const(SimpleSpanned<ConstDecl<'src>>),
    Directive(SimpleSpanned<Directive<'src>>),
    Partition(SimpleSpanned<UnresolvedPartition<'src>>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Section {
    Const,
    Directive,
    Partition,
}

fn validate_item_order<'src>(
    items: Vec<Item<'src>>,
    emitter: &mut Emitter<Rich<'src, char, SimpleSpan<usize>, Diagnostic>>,
) -> UnresolvedLayout<'src> {
    let mut section = Section::Const;

    let mut constants = Vec::new();
    let mut directives = Vec::new();
    let mut partitions = Vec::new();

    for item in items {
        match item {
            Item::Const(constant) => {
                if section != Section::Const {
                    let (message, label, help) = match section {
                        Section::Directive => (
                            "constant declared after directives",
                            "constants are recommended to appear before directives",
                            "consider moving the constant before all directives",
                        ),

                        Section::Partition => (
                            "constant declared after partitions",
                            "constants are recommended to appear before partitions",
                            "consider moving the constant before all directives and partitions",
                        ),

                        Section::Const => unreachable!(),
                    };

                    emitter.emit(Rich::custom(
                        constant.span,
                        Diagnostic::new(Severity::Warning, message.to_string())
                            .add_label(DiagLabel::new_primary(label.to_string(), constant.span))
                            .with_help(help.to_string()),
                    ));
                }

                constants.push(constant);
            }

            Item::Directive(directive) => {
                if section == Section::Partition {
                    emitter.emit(Rich::custom(
                        directive.span,
                        Diagnostic::new(
                            Severity::Warning,
                            "directive declared after partitions".to_string(),
                        )
                        .add_label(DiagLabel::new_primary(
                            "directives are recommended to appear before partitions".to_string(),
                            directive.span,
                        ))
                        .with_help(
                            "consider moving the directive before all partition declarations"
                                .to_string(),
                        ),
                    ));
                } else {
                    section = Section::Directive;
                }

                directives.push(directive);
            }

            Item::Partition(partition) => {
                section = Section::Partition;
                partitions.push(partition);
            }
        }
    }

    UnresolvedLayout {
        constants: constants.into_boxed_slice(),
        directives: directives.into_boxed_slice(),
        partitions: partitions.into_boxed_slice(),
    }
}

pub(super) fn layout_parser<'src>()
-> impl Parser<'src, &'src str, UnresolvedLayout<'src>, Extra<'src>> {
    let item = choice((
        const_decl().map(Item::Const),
        directive().map(Item::Directive),
        partition().map(Item::Partition),
    ));

    item.repeated()
        .collect::<Vec<_>>()
        .validate(|items, _, emitter| validate_item_order(items, emitter))
        .padded_by(ignored())
}
