use std::path::PathBuf;

use chumsky::span::SimpleSpan;

use crate::diagnostics::{DiagLabel, Diagnostic, Severity};

#[derive(Debug, Clone)]
pub struct Layout {
    pub partitions: Box<[Partition]>,
}

#[derive(Debug, Clone)]
pub struct Partition {
    pub name: String,
    pub offset: u64,
    pub size: u64,
    pub magic: Box<[u8]>,
    pub source: Source,
}

#[derive(Debug, Clone)]
pub enum Source {
    File(PathBuf),
}

#[derive(Debug, Clone)]
pub enum ResolvedValue<'src> {
    HexInt(i64),
    HexArray(SizedValue<Box<[u8]>>),
    Size(i64),
    String(&'src str),
    Path(PathBuf),
    Keyword(KeywordValue),
}

impl<'src> ResolvedValue<'src> {
    pub fn cast(
        self,
        expected: ValueType,
        allowed_keywords: &[KeywordValue],
        span: SimpleSpan,
    ) -> Result<ResolvedValue<'src>, Diagnostic> {
        match (self, expected) {
            // A keyword is accepted only when explicitly allowed by the caller.
            (value @ Self::Keyword(keyword), _) if allowed_keywords.contains(&keyword) => Ok(value),

            // Exact matches
            (value @ Self::String(_), ValueType::String) => Ok(value),
            (value @ Self::Path(_), ValueType::Path) => Ok(value),
            (value @ Self::Size(_), ValueType::Sizable) => Ok(value),

            (value @ Self::HexInt(_), ValueType::HexInt | ValueType::Sizable) => Ok(value),

            (value @ Self::HexArray(_), ValueType::HexArray) => Ok(value),

            // Size -> HexInt
            (Self::Size(value), ValueType::HexInt) => Ok(Self::HexInt(value)),

            // HexArray -> HexInt
            (Self::HexArray(value), ValueType::HexInt)
            | (Self::HexArray(value), ValueType::Sizable) => {
                let out_of_range = || {
                    Diagnostic::new(Severity::Error, "value out of range".to_owned()).add_label(
                        DiagLabel::new_primary(
                            format!("value must be in the range 0..=0x{:X}", i64::MAX),
                            span,
                        ),
                    )
                };

                let padding =
                    size_of::<u64>().checked_sub(value.value.len()).ok_or_else(out_of_range)?;

                let mut buffer = [0; size_of::<u64>()];
                buffer[padding..].copy_from_slice(&value.value);

                let int = i64::try_from(u64::from_be_bytes(buffer)).map_err(|_| out_of_range())?;

                Ok(Self::HexInt(int))
            }

            (Self::HexArray(value), ValueType::SizedHexArray) => Ok(Self::HexArray(value)),

            // Everything else
            (value, _) => {
                let expected_message = std::iter::once(expected.as_str().to_owned())
                    .chain(allowed_keywords.iter().map(|keyword| format!("`{}`", keyword.as_str())))
                    .collect::<Vec<_>>()
                    .join(" or ");

                Err(Diagnostic::new(Severity::Error, format!("expected {expected_message}"))
                    .add_label(DiagLabel::new_primary(
                        format!(
                            "{} does not match {expected_message}",
                            value.value_type().as_str(),
                        ),
                        span,
                    )))
            }
        }
    }

    pub fn value_type(&self) -> ValueType {
        match self {
            Self::HexInt(_) => ValueType::HexInt,

            Self::HexArray(value) => {
                if value.byte_size.is_some() {
                    ValueType::SizedHexArray
                } else {
                    ValueType::HexArray
                }
            }

            Self::Size(_) => ValueType::Sizable,
            Self::String(_) => ValueType::String,
            Self::Path(_) => ValueType::Path,
            Self::Keyword(_) => ValueType::Keyword,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueType {
    HexInt,
    HexArray,
    SizedHexArray,
    Sizable,
    String,
    Path,
    Keyword,
}

impl ValueType {
    pub fn as_str(&self) -> &'static str {
        match self {
            ValueType::HexInt => "hex int",
            ValueType::HexArray => "hex array",
            ValueType::SizedHexArray => "sized hex array",
            ValueType::Sizable => "sizable",
            ValueType::String => "string",
            ValueType::Path => "path",
            ValueType::Keyword => "keyword value",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeywordValue {
    Auto,
    Default,
}

impl KeywordValue {
    pub fn from_keyword(keyword: &str) -> Option<Self> {
        match keyword {
            "auto" => Some(Self::Auto),
            "default" => Some(Self::Default),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Default => "default",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SizedValue<T> {
    pub value: T,
    pub byte_size: Option<usize>,
}

impl<T> SizedValue<T> {
    pub fn new_unsized(value: T) -> SizedValue<T> {
        SizedValue { value, byte_size: None }
    }
}
