use chumsky::span::SimpleSpanned;

#[derive(Debug, Clone)]
pub struct UnresolvedLayout<'src> {
    pub constants: Box<[SimpleSpanned<ConstDecl<'src>>]>,
    pub directives: Box<[SimpleSpanned<Directive<'src>>]>,
    pub partitions: Box<[SimpleSpanned<UnresolvedPartition<'src>>]>,
}

#[derive(Debug, Clone)]
pub struct ConstDecl<'src> {
    pub name: SimpleSpanned<&'src str>,
    pub value: SimpleSpanned<UnresolvedValue<'src>>,
}

#[derive(Debug, Clone)]
pub struct Directive<'src> {
    pub name: SimpleSpanned<&'src str>,
    pub args: Box<[SimpleSpanned<UnresolvedValue<'src>>]>,
}

#[derive(Debug, Clone)]
pub struct UnresolvedPartition<'src> {
    pub name: SimpleSpanned<&'src str>,
    pub clarifications: Box<[SimpleSpanned<Clarification<'src>>]>,
}

#[derive(Debug, Clone)]
pub struct Clarification<'src> {
    pub name: SimpleSpanned<&'src str>,
    pub args: Box<[SimpleSpanned<UnresolvedValue<'src>>]>,
}

#[derive(Debug, Clone)]
pub enum UnresolvedValue<'src> {
    HexArray(Box<[u8]>),
    Size(Size),
    String(&'src str),
    Identifier(&'src str),
    BuiltinConst(&'src str),
    Clarification {
        name: &'src str,
        args: Box<[SimpleSpanned<UnresolvedValue<'src>>]>,
    },
    BinaryExpr {
        op: BinaryOperation,
        lhs: Box<SimpleSpanned<UnresolvedValue<'src>>>,
        rhs: Box<SimpleSpanned<UnresolvedValue<'src>>>,
    },
    UnaryExpr {
        op: SimpleSpanned<UnaryOperation>,
        operand: Box<SimpleSpanned<UnresolvedValue<'src>>>,
    },
}

#[derive(Debug, Clone, Copy)]
pub enum BinaryOperation {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Clone, Copy)]
pub enum UnaryOperation {
    Pos,
    Neg,
}

#[derive(Debug, Clone, Copy)]
pub struct Size {
    pub value: i64,
    pub unit: SizeUnit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeUnit {
    Bytes,
    KiB,
    MiB,
    GiB,
}

impl SizeUnit {
    pub fn multiplier(&self) -> i64 {
        match self {
            SizeUnit::Bytes => 1,
            SizeUnit::KiB => 1024,
            SizeUnit::MiB => 1024 * 1024,
            SizeUnit::GiB => 1024 * 1024 * 1024,
        }
    }
}
