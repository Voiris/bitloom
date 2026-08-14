use std::path::PathBuf;

use chumsky::Parser;
use chumsky::span::{SimpleSpan, SimpleSpanned};

use super::constant::const_decl;
use super::directive::directive;
use super::layout::layout_parser;
use super::literals::{parse_hex_digits, parse_size_unit, parse_uint_literal};
use super::parse_source;
use super::partition::{clarification, partition};
use super::unresolved::{
    BinaryOperation, SizeUnit, UnaryOperation, UnresolvedLayout, UnresolvedValue,
};
use super::value::{parse_value, parse_value_args};
use crate::Source;
use crate::diagnostics::{DiagSuccess, Severity};

trait ExpectOk<T> {
    fn expect_ok(self) -> T;
}

impl<T, E> ExpectOk<T> for Result<T, E> {
    fn expect_ok(self) -> T {
        match self {
            Ok(value) => value,
            Err(_) => panic!("expected successful parse"),
        }
    }
}

fn source(content: &str) -> Source {
    Source { path: PathBuf::from("test.blpl"), content: content.to_owned() }
}

fn parse_layout(
    source: &Source,
) -> (UnresolvedLayout<'_>, Option<Vec<crate::diagnostics::Diagnostic>>) {
    match parse_source(source) {
        Ok(DiagSuccess { value, warnings }) => (value, warnings),
        Err(error) => panic!("expected successful parse, got {error:#?}"),
    }
}

fn value(input: &str) -> SimpleSpanned<UnresolvedValue<'_>> {
    match parse_value().parse(input).into_result() {
        Ok(value) => value,
        Err(_) => panic!("failed to parse {input:?}"),
    }
}

fn size_value(value: &SimpleSpanned<UnresolvedValue<'_>>) -> (i64, SizeUnit) {
    match value.inner {
        UnresolvedValue::Size(size) => (size.value, size.unit),
        ref other => panic!("expected size, got {other:?}"),
    }
}

#[test]
fn parses_basic_example_end_to_end() {
    let input = source(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/blpl/examples/basic.blpl"
    )));
    let (layout, warnings) = parse_layout(&input);

    assert!(warnings.is_none());
    assert_eq!(layout.constants.len(), 1);
    assert_eq!(layout.directives.len(), 3);
    assert_eq!(layout.partitions.len(), 2);

    let constant = &layout.constants[0].inner;
    assert_eq!(constant.name.inner, "BITSTREAM_OFFSET");
    assert!(matches!(
        constant.value.inner,
        UnresolvedValue::HexArray(ref bytes) if bytes.as_ref() == [0]
    ));

    assert_eq!(layout.directives[0].inner.name.inner, "flash_size");
    assert_eq!(size_value(&layout.directives[0].inner.args[0]), (8, SizeUnit::MiB));
    assert_eq!(layout.directives[1].inner.name.inner, "erase_size");
    assert!(matches!(
        layout.directives[1].inner.args[0].inner,
        UnresolvedValue::Identifier("default")
    ));
    assert_eq!(layout.directives[2].inner.name.inner, "offset_align");
    assert!(matches!(
        layout.directives[2].inner.args[0].inner,
        UnresolvedValue::Identifier("auto")
    ));

    let partition = &layout.partitions[0].inner;
    assert_eq!(partition.name.inner, "bitstream");
    assert_eq!(layout.partitions[1].inner.name.inner, "firmware");
    assert_eq!(partition.clarifications.len(), 3);
    assert_eq!(partition.clarifications[0].inner.name.inner, "offset");
    assert!(matches!(
        partition.clarifications[0].inner.args[0].inner,
        UnresolvedValue::Identifier("BITSTREAM_OFFSET")
    ));
    assert!(matches!(
        partition.clarifications[2].inner.args[0].inner,
        UnresolvedValue::BuiltinConst("bitstream")
    ));
}

#[test]
fn parses_ignored_input_between_tokens_and_at_file_edges() {
    let input = source(
        "\n/* header */ const\tA /* name */ = 1KiB; // line\n\
         @flag(/* empty */);\n\
         part: item(\"file.bin\") /* tail */,; // eof",
    );
    let (layout, warnings) = parse_layout(&input);

    assert!(warnings.is_none());
    assert_eq!(layout.constants.len(), 1);
    assert_eq!(layout.directives.len(), 1);
    assert_eq!(layout.partitions.len(), 1);
}

#[test]
fn rejects_unterminated_block_comment_and_trailing_garbage() {
    for input in ["/* unterminated", "@ok(); ???"] {
        assert!(layout_parser().parse(input).into_result().is_err(), "{input:?}");
    }
}

#[test]
fn parses_hex_digits_as_big_endian_magnitude() {
    let cases: &[(&str, &[u8])] = &[
        ("0", &[0]),
        ("0000", &[0]),
        ("f", &[0x0f]),
        ("0_f", &[0x0f]),
        ("0001_0203", &[0x01, 0x02, 0x03]),
        ("AB_cd", &[0xab, 0xcd]),
    ];

    for (input, expected) in cases {
        let actual = parse_hex_digits().parse(*input).into_result().expect_ok();
        assert_eq!(actual.as_ref(), *expected, "{input:?}");
    }
}

#[test]
fn rejects_invalid_hex_digit_separators() {
    for input in ["", "_1", "1_", "1__2", "xyz"] {
        assert!(parse_hex_digits().parse(input).into_result().is_err(), "{input:?}");
    }
}

#[test]
fn parses_decimal_uint_and_reports_overflow() {
    assert_eq!(parse_uint_literal().parse("0").into_result().expect_ok(), 0);
    assert_eq!(
        parse_uint_literal().parse("18_446_744_073_709_551_615").into_result().expect_ok(),
        u64::MAX
    );
    assert!(parse_uint_literal().parse("18_446_744_073_709_551_616").into_result().is_err());
}

#[test]
fn parses_all_size_units_and_rejects_unknown_units() {
    let cases = [
        ("B", SizeUnit::Bytes),
        ("KiB", SizeUnit::KiB),
        ("MiB", SizeUnit::MiB),
        ("GiB", SizeUnit::GiB),
    ];

    for (input, expected) in cases {
        assert!(matches!(
            parse_size_unit().parse(input).into_result(),
            Ok(actual) if actual as u8 == expected as u8
        ));
    }
    for input in ["b", "KB", "Kb", "TiB"] {
        assert!(parse_size_unit().parse(input).into_result().is_err(), "{input:?}");
    }
}

#[test]
fn parses_every_atomic_value_kind() {
    assert!(matches!(value("name").inner, UnresolvedValue::Identifier("name")));
    assert!(matches!(value("$bitstream").inner, UnresolvedValue::BuiltinConst("bitstream")));
    assert!(matches!(
        value("0x00_abcd").inner,
        UnresolvedValue::HexArray(ref bytes) if bytes.as_ref() == [0xab, 0xcd]
    ));
    assert_eq!(size_value(&value("42MiB")), (42, SizeUnit::MiB));
    assert!(matches!(value("\"path/to/file\"").inner, UnresolvedValue::String("path/to/file")));
}

#[test]
fn parses_nested_clarifications_before_plain_identifiers() {
    let parsed = value("outer(inner(name), $builtin,)");
    let UnresolvedValue::Clarification { name, args } = parsed.inner else {
        panic!("expected clarification, got {:?}", parsed.inner);
    };

    assert_eq!(name, "outer");
    assert_eq!(args.len(), 2);
    assert!(matches!(
        args[0].inner,
        UnresolvedValue::Clarification { name: "inner", ref args }
            if matches!(args[0].inner, UnresolvedValue::Identifier("name"))
    ));
    assert!(matches!(args[1].inner, UnresolvedValue::BuiltinConst("builtin")));
}

#[test]
fn parses_empty_and_trailing_comma_argument_lists() {
    assert!(parse_value_args().parse("()").into_result().expect_ok().is_empty());
    assert_eq!(parse_value_args().parse("(a, b,)").into_result().expect_ok().len(), 2);
}

#[test]
fn applies_prefix_and_binary_operator_precedence() {
    let parsed = value("1B + 2B * -3B");
    let UnresolvedValue::BinaryExpr { op: BinaryOperation::Add, lhs, rhs } = parsed.inner else {
        panic!("expected addition, got {:?}", parsed.inner);
    };
    assert_eq!(size_value(&lhs), (1, SizeUnit::Bytes));

    let UnresolvedValue::BinaryExpr { op: BinaryOperation::Mul, lhs, rhs } = rhs.inner else {
        panic!("expected multiplication, got {:?}", rhs.inner);
    };
    assert_eq!(size_value(&lhs), (2, SizeUnit::Bytes));
    assert!(matches!(
        rhs.inner,
        UnresolvedValue::UnaryExpr { ref op, ref operand }
            if matches!(op.inner, UnaryOperation::Neg)
                && size_value(operand) == (3, SizeUnit::Bytes)
    ));
}

#[test]
fn parses_every_arithmetic_operator() {
    let binary_cases = [
        ("1B + 2B", BinaryOperation::Add),
        ("1B - 2B", BinaryOperation::Sub),
        ("1B * 2B", BinaryOperation::Mul),
        ("1B / 2B", BinaryOperation::Div),
        ("1B % 2B", BinaryOperation::Mod),
    ];

    for (input, expected) in binary_cases {
        let UnresolvedValue::BinaryExpr { op, .. } = value(input).inner else {
            panic!("expected binary expression for {input:?}");
        };
        assert_eq!(std::mem::discriminant(&op), std::mem::discriminant(&expected));
    }

    for (input, expected) in [("+1B", UnaryOperation::Pos), ("-1B", UnaryOperation::Neg)] {
        let UnresolvedValue::UnaryExpr { op, .. } = value(input).inner else {
            panic!("expected unary expression for {input:?}");
        };
        assert_eq!(std::mem::discriminant(&op.inner), std::mem::discriminant(&expected));
        assert_eq!(op.span, SimpleSpan { start: 0, end: 1, context: () });
    }
}

#[test]
fn binary_operators_are_left_associative_and_parentheses_override_precedence() {
    let parsed = value("1B - 2B - 3B");
    assert!(matches!(
        parsed.inner,
        UnresolvedValue::BinaryExpr {
            op: BinaryOperation::Sub,
            ref lhs,
            ref rhs,
        } if matches!(lhs.inner, UnresolvedValue::BinaryExpr { op: BinaryOperation::Sub, .. })
            && size_value(rhs) == (3, SizeUnit::Bytes)
    ));

    let parsed = value("(1B + 2B) * 3B");
    assert!(matches!(
        parsed.inner,
        UnresolvedValue::BinaryExpr {
            op: BinaryOperation::Mul,
            ref lhs,
            ..
        } if matches!(lhs.inner, UnresolvedValue::BinaryExpr { op: BinaryOperation::Add, .. })
    ));
}

#[test]
fn rejects_incomplete_values_and_multiline_strings() {
    for input in ["", "$", "0x", "1", "call(", "1B +", "\"line\nline\""] {
        assert!(parse_value().parse(input).into_result().is_err(), "{input:?}");
    }
}

#[test]
fn parses_constant_and_preserves_token_spans() {
    let parsed = const_decl().parse("  const VALUE = 4KiB;  ").into_result().expect_ok();

    assert_eq!(parsed.inner.name.inner, "VALUE");
    assert_eq!(parsed.inner.name.span.into_range(), 8..13);
    assert_eq!(parsed.inner.value.span.into_range(), 16..20);
    assert_eq!(parsed.span.into_range(), 2..21);
}

#[test]
fn constant_requires_keyword_whitespace_assignment_and_semicolon() {
    for input in ["constX = 1B;", "const X 1B;", "const X = 1B"] {
        assert!(const_decl().parse(input).into_result().is_err(), "{input:?}");
    }
}

#[test]
fn parses_directives_and_requires_terminator() {
    let parsed = directive().parse("@align(4KiB, 8KiB,);").into_result().expect_ok();
    assert_eq!(parsed.inner.name.inner, "align");
    assert_eq!(parsed.inner.args.len(), 2);
    assert!(directive().parse("@align(4KiB)").into_result().is_err());
}

#[test]
fn parses_partition_clarifications_and_requires_at_least_one() {
    let parsed = partition().parse("app: offset(1B), size(2B),;").into_result().expect_ok();
    assert_eq!(parsed.inner.name.inner, "app");
    assert_eq!(parsed.inner.clarifications.len(), 2);
    assert!(partition().parse("app:;").into_result().is_err());

    let parsed = clarification().parse("file(\"app.bin\")").into_result().expect_ok();
    assert_eq!(parsed.inner.name.inner, "file");
    assert!(matches!(parsed.inner.args[0].inner, UnresolvedValue::String("app.bin")));
}

#[test]
fn accepts_empty_layout_and_comment_only_layout() {
    for input in ["", "  \n// nothing\n/* still nothing */"] {
        let parsed = layout_parser().parse(input).into_result().expect_ok();
        assert!(parsed.constants.is_empty());
        assert!(parsed.directives.is_empty());
        assert!(parsed.partitions.is_empty());
    }
}

#[test]
fn emits_warnings_for_items_declared_out_of_recommended_order() {
    let input = source("part: item(); @late(); const LAST = 1B;");
    let (_, warnings) = parse_layout(&input);
    let warnings = warnings.expect("expected ordering warnings");

    assert_eq!(warnings.len(), 2);
    assert_eq!(warnings[0].severity, Severity::Warning);
    assert_eq!(warnings[0].message, "directive declared after partitions");
    assert_eq!(warnings[1].severity, Severity::Warning);
    assert_eq!(warnings[1].message, "constant declared after partitions");
    assert!(warnings.iter().all(|warning| !warning.labels.is_empty()));
    assert!(warnings.iter().all(|warning| warning.help.is_some()));
}

#[test]
fn distinguishes_constant_after_directives_warning() {
    let input = source("@first(); const LATE = 1B;");
    let (_, warnings) = parse_layout(&input);
    let warnings = warnings.expect("expected ordering warning");

    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].message, "constant declared after directives");
}

#[test]
fn maps_syntax_errors_to_diagnostics() {
    let input = source("@broken(");
    let Err(error) = parse_source(&input) else {
        panic!("expected parse failure");
    };

    assert_eq!(error.severity, Severity::Error);
    assert!(!error.labels.is_empty());
}

#[test]
fn reports_size_literal_out_of_signed_range() {
    let input = source("const TOO_LARGE = 9223372036854775808B;");
    let Err(error) = parse_source(&input) else {
        panic!("expected parse failure");
    };

    assert_eq!(error.message, "integer literal is out of range");
    assert_eq!(error.note.as_deref(), Some("the maximum value is 9223372036854775807"));
}
