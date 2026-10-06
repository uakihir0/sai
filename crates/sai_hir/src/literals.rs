use sai_intern::Symbol;

/// A decoded literal value carried by HIR.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Lit<'hir> {
    Int { value: Option<u128>, negated: bool },
    Float { text: Symbol, negated: bool },
    Str(Symbol),
    ByteStr(&'hir [u8]),
    Char(char),
    Byte(u8),
    Bool(bool),
}
