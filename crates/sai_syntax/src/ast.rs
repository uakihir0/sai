use std::borrow::Cow;

use crate::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};

pub trait AstNode: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(node: SyntaxNode) -> Option<Self>;
    fn syntax(&self) -> &SyntaxNode;
}

pub trait AstToken: Sized {
    fn can_cast(kind: SyntaxKind) -> bool;
    fn cast(token: SyntaxToken) -> Option<Self>;
    fn syntax(&self) -> &SyntaxToken;
}

macro_rules! ast_node {
    ($name:ident, $kind:ident) => {
        #[derive(Clone, Debug)]
        pub struct $name(SyntaxNode);

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == SyntaxKind::$kind
            }

            fn cast(node: SyntaxNode) -> Option<Self> {
                Self::can_cast(node.kind()).then_some(Self(node))
            }

            fn syntax(&self) -> &SyntaxNode {
                &self.0
            }
        }
    };
}

macro_rules! ast_token {
    ($name:ident, $kind:ident) => {
        #[derive(Clone, Debug)]
        pub struct $name(SyntaxToken);

        impl AstToken for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                kind == SyntaxKind::$kind
            }

            fn cast(token: SyntaxToken) -> Option<Self> {
                Self::can_cast(token.kind()).then_some(Self(token))
            }

            fn syntax(&self) -> &SyntaxToken {
                &self.0
            }
        }
    };
}

ast_node!(SourceFile, SOURCE_FILE);
ast_node!(TestDecl, TEST_DECL);
ast_node!(PropertyDecl, PROPERTY_DECL);
ast_node!(Block, BLOCK);
ast_node!(ParamList, PARAM_LIST);
ast_node!(Param, PARAM);
ast_node!(Name, NAME);
ast_node!(TupleIndexExpr, TUPLE_INDEX_EXPR);
ast_node!(FStringExpr, FSTRING_EXPR);
ast_node!(FStringInterp, FSTRING_INTERP);
ast_node!(LiteralExpr, LITERAL);
ast_node!(PathExpr, PATH_EXPR);
ast_node!(SelfExpr, SELF_EXPR);
ast_node!(SelfTypeExpr, SELF_TYPE_EXPR);
ast_node!(ImplicitMemberExpr, IMPLICIT_MEMBER_EXPR);
ast_node!(UnitExpr, UNIT_EXPR);
ast_node!(ParenExpr, PAREN_EXPR);
ast_node!(TupleExpr, TUPLE_EXPR);
ast_node!(ArrayExpr, ARRAY_EXPR);
ast_node!(BlockExpr, BLOCK_EXPR);
ast_node!(UnsafeBlockExpr, UNSAFE_BLOCK_EXPR);
ast_node!(AsyncBlockExpr, ASYNC_BLOCK_EXPR);
ast_node!(IfExpr, IF_EXPR);
ast_node!(MatchExpr, MATCH_EXPR);
ast_node!(LoopExpr, LOOP_EXPR);
ast_node!(WhileExpr, WHILE_EXPR);
ast_node!(ForExpr, FOR_EXPR);
ast_node!(ClosureExpr, CLOSURE_EXPR);
ast_node!(RegionExpr, REGION_EXPR);
ast_node!(ReturnExpr, RETURN_EXPR);
ast_node!(BreakExpr, BREAK_EXPR);
ast_node!(ContinueExpr, CONTINUE_EXPR);
ast_node!(BinExpr, BIN_EXPR);
ast_node!(PrefixExpr, PREFIX_EXPR);
ast_node!(RangeExpr, RANGE_EXPR);
ast_node!(FieldExpr, FIELD_EXPR);
ast_node!(AwaitExpr, AWAIT_EXPR);
ast_node!(CallExpr, CALL_EXPR);
ast_node!(IndexExpr, INDEX_EXPR);
ast_node!(TryExpr, TRY_EXPR);
ast_node!(CastExpr, CAST_EXPR);

ast_token!(IntLit, INT);
ast_token!(FloatLit, FLOAT);
ast_token!(StringLit, STRING);
ast_token!(ByteStringLit, BYTE_STRING);
ast_token!(CharLit, CHAR);
ast_token!(ByteLit, BYTE);
ast_token!(FormatSpec, FSTRING_SPEC);

#[derive(Clone, Debug)]
pub enum Expr {
    Literal(LiteralExpr),
    FormatString(FStringExpr),
    FormatStringInterpolation(FStringInterp),
    Path(PathExpr),
    SelfValue(SelfExpr),
    SelfType(SelfTypeExpr),
    ImplicitMember(ImplicitMemberExpr),
    Unit(UnitExpr),
    Paren(ParenExpr),
    Tuple(TupleExpr),
    Array(ArrayExpr),
    Block(BlockExpr),
    UnsafeBlock(UnsafeBlockExpr),
    AsyncBlock(AsyncBlockExpr),
    If(IfExpr),
    Match(MatchExpr),
    Loop(LoopExpr),
    While(WhileExpr),
    For(ForExpr),
    Closure(ClosureExpr),
    Region(RegionExpr),
    Return(ReturnExpr),
    Break(BreakExpr),
    Continue(ContinueExpr),
    Binary(BinExpr),
    Prefix(PrefixExpr),
    Range(RangeExpr),
    Field(FieldExpr),
    TupleIndex(TupleIndexExpr),
    Await(AwaitExpr),
    Call(CallExpr),
    Index(IndexExpr),
    Try(TryExpr),
    Cast(CastExpr),
}

impl AstNode for Expr {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::LITERAL
                | SyntaxKind::FSTRING_EXPR
                | SyntaxKind::FSTRING_INTERP
                | SyntaxKind::PATH_EXPR
                | SyntaxKind::SELF_EXPR
                | SyntaxKind::SELF_TYPE_EXPR
                | SyntaxKind::IMPLICIT_MEMBER_EXPR
                | SyntaxKind::UNIT_EXPR
                | SyntaxKind::PAREN_EXPR
                | SyntaxKind::TUPLE_EXPR
                | SyntaxKind::ARRAY_EXPR
                | SyntaxKind::BLOCK_EXPR
                | SyntaxKind::UNSAFE_BLOCK_EXPR
                | SyntaxKind::ASYNC_BLOCK_EXPR
                | SyntaxKind::IF_EXPR
                | SyntaxKind::MATCH_EXPR
                | SyntaxKind::LOOP_EXPR
                | SyntaxKind::WHILE_EXPR
                | SyntaxKind::FOR_EXPR
                | SyntaxKind::CLOSURE_EXPR
                | SyntaxKind::REGION_EXPR
                | SyntaxKind::RETURN_EXPR
                | SyntaxKind::BREAK_EXPR
                | SyntaxKind::CONTINUE_EXPR
                | SyntaxKind::BIN_EXPR
                | SyntaxKind::PREFIX_EXPR
                | SyntaxKind::RANGE_EXPR
                | SyntaxKind::FIELD_EXPR
                | SyntaxKind::TUPLE_INDEX_EXPR
                | SyntaxKind::AWAIT_EXPR
                | SyntaxKind::CALL_EXPR
                | SyntaxKind::INDEX_EXPR
                | SyntaxKind::TRY_EXPR
                | SyntaxKind::CAST_EXPR
        )
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            SyntaxKind::LITERAL => Self::Literal(LiteralExpr::cast(node)?),
            SyntaxKind::FSTRING_EXPR => Self::FormatString(FStringExpr::cast(node)?),
            SyntaxKind::FSTRING_INTERP => {
                Self::FormatStringInterpolation(FStringInterp::cast(node)?)
            }
            SyntaxKind::PATH_EXPR => Self::Path(PathExpr::cast(node)?),
            SyntaxKind::SELF_EXPR => Self::SelfValue(SelfExpr::cast(node)?),
            SyntaxKind::SELF_TYPE_EXPR => Self::SelfType(SelfTypeExpr::cast(node)?),
            SyntaxKind::IMPLICIT_MEMBER_EXPR => {
                Self::ImplicitMember(ImplicitMemberExpr::cast(node)?)
            }
            SyntaxKind::UNIT_EXPR => Self::Unit(UnitExpr::cast(node)?),
            SyntaxKind::PAREN_EXPR => Self::Paren(ParenExpr::cast(node)?),
            SyntaxKind::TUPLE_EXPR => Self::Tuple(TupleExpr::cast(node)?),
            SyntaxKind::ARRAY_EXPR => Self::Array(ArrayExpr::cast(node)?),
            SyntaxKind::BLOCK_EXPR => Self::Block(BlockExpr::cast(node)?),
            SyntaxKind::UNSAFE_BLOCK_EXPR => Self::UnsafeBlock(UnsafeBlockExpr::cast(node)?),
            SyntaxKind::ASYNC_BLOCK_EXPR => Self::AsyncBlock(AsyncBlockExpr::cast(node)?),
            SyntaxKind::IF_EXPR => Self::If(IfExpr::cast(node)?),
            SyntaxKind::MATCH_EXPR => Self::Match(MatchExpr::cast(node)?),
            SyntaxKind::LOOP_EXPR => Self::Loop(LoopExpr::cast(node)?),
            SyntaxKind::WHILE_EXPR => Self::While(WhileExpr::cast(node)?),
            SyntaxKind::FOR_EXPR => Self::For(ForExpr::cast(node)?),
            SyntaxKind::CLOSURE_EXPR => Self::Closure(ClosureExpr::cast(node)?),
            SyntaxKind::REGION_EXPR => Self::Region(RegionExpr::cast(node)?),
            SyntaxKind::RETURN_EXPR => Self::Return(ReturnExpr::cast(node)?),
            SyntaxKind::BREAK_EXPR => Self::Break(BreakExpr::cast(node)?),
            SyntaxKind::CONTINUE_EXPR => Self::Continue(ContinueExpr::cast(node)?),
            SyntaxKind::BIN_EXPR => Self::Binary(BinExpr::cast(node)?),
            SyntaxKind::PREFIX_EXPR => Self::Prefix(PrefixExpr::cast(node)?),
            SyntaxKind::RANGE_EXPR => Self::Range(RangeExpr::cast(node)?),
            SyntaxKind::FIELD_EXPR => Self::Field(FieldExpr::cast(node)?),
            SyntaxKind::TUPLE_INDEX_EXPR => Self::TupleIndex(TupleIndexExpr::cast(node)?),
            SyntaxKind::AWAIT_EXPR => Self::Await(AwaitExpr::cast(node)?),
            SyntaxKind::CALL_EXPR => Self::Call(CallExpr::cast(node)?),
            SyntaxKind::INDEX_EXPR => Self::Index(IndexExpr::cast(node)?),
            SyntaxKind::TRY_EXPR => Self::Try(TryExpr::cast(node)?),
            SyntaxKind::CAST_EXPR => Self::Cast(CastExpr::cast(node)?),
            _ => return None,
        })
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::Literal(node) => node.syntax(),
            Self::FormatString(node) => node.syntax(),
            Self::FormatStringInterpolation(node) => node.syntax(),
            Self::Path(node) => node.syntax(),
            Self::SelfValue(node) => node.syntax(),
            Self::SelfType(node) => node.syntax(),
            Self::ImplicitMember(node) => node.syntax(),
            Self::Unit(node) => node.syntax(),
            Self::Paren(node) => node.syntax(),
            Self::Tuple(node) => node.syntax(),
            Self::Array(node) => node.syntax(),
            Self::Block(node) => node.syntax(),
            Self::UnsafeBlock(node) => node.syntax(),
            Self::AsyncBlock(node) => node.syntax(),
            Self::If(node) => node.syntax(),
            Self::Match(node) => node.syntax(),
            Self::Loop(node) => node.syntax(),
            Self::While(node) => node.syntax(),
            Self::For(node) => node.syntax(),
            Self::Closure(node) => node.syntax(),
            Self::Region(node) => node.syntax(),
            Self::Return(node) => node.syntax(),
            Self::Break(node) => node.syntax(),
            Self::Continue(node) => node.syntax(),
            Self::Binary(node) => node.syntax(),
            Self::Prefix(node) => node.syntax(),
            Self::Range(node) => node.syntax(),
            Self::Field(node) => node.syntax(),
            Self::TupleIndex(node) => node.syntax(),
            Self::Await(node) => node.syntax(),
            Self::Call(node) => node.syntax(),
            Self::Index(node) => node.syntax(),
            Self::Try(node) => node.syntax(),
            Self::Cast(node) => node.syntax(),
        }
    }
}

#[derive(Clone, Debug)]
pub enum FStringPiece<'a> {
    Text(Cow<'a, str>),
    Interp {
        expr: Option<Expr>,
        spec: Option<FormatSpec>,
    },
}

impl IntLit {
    pub fn value(&self) -> Option<u128> {
        if self.syntax().has_error() {
            return None;
        }

        parse_integer(self.syntax().text())
    }
}

fn parse_integer(text: &str) -> Option<u128> {
    let text = text.as_bytes();
    let (radix, digits) = if let Some(digits) = text.strip_prefix(b"0x") {
        (16, digits)
    } else if let Some(digits) = text.strip_prefix(b"0o") {
        (8, digits)
    } else if let Some(digits) = text.strip_prefix(b"0b") {
        (2, digits)
    } else {
        (10, text)
    };
    if digits.is_empty() {
        return None;
    }

    let mut value = 0_u128;
    let mut previous_was_digit = false;
    for byte in digits {
        if *byte == b'_' {
            if !previous_was_digit {
                return None;
            }
            previous_was_digit = false;
            continue;
        }

        let digit = crate::digit_value(*byte)?;
        if digit >= radix {
            return None;
        }
        value = value
            .checked_mul(u128::from(radix))?
            .checked_add(u128::from(digit))?;
        previous_was_digit = true;
    }

    previous_was_digit.then_some(value)
}

impl FloatLit {
    pub fn value_f64(&self) -> Option<f64> {
        self.parse_value()
    }

    pub fn value_f32(&self) -> Option<f32> {
        self.parse_value()
    }

    fn parse_value<T: std::str::FromStr>(&self) -> Option<T> {
        if self.syntax().has_error() {
            return None;
        }

        let text = self.syntax().text();
        if text.contains('_') {
            let cleaned = text.replace('_', "");
            cleaned.parse::<T>().ok()
        } else {
            text.parse::<T>().ok()
        }
    }
}

impl CharLit {
    pub fn value(&self) -> char {
        if self.syntax().has_error() {
            return '\u{fffd}';
        }

        let Some(body) = self
            .syntax()
            .text()
            .strip_prefix('\'')
            .and_then(|text| text.strip_suffix('\''))
        else {
            return '\u{fffd}';
        };
        match decode_char_body(body, crate::EscapeMode::Char) {
            Some(crate::EscapeValue::Scalar(value)) => value,
            Some(crate::EscapeValue::Byte(value)) => char::from(value),
            None => '\u{fffd}',
        }
    }
}

impl ByteLit {
    pub fn value(&self) -> u8 {
        if self.syntax().has_error() {
            return 0;
        }

        let Some(body) = self
            .syntax()
            .text()
            .strip_prefix("b'")
            .and_then(|text| text.strip_suffix('\''))
        else {
            return 0;
        };
        match decode_char_body(body, crate::EscapeMode::Byte) {
            Some(crate::EscapeValue::Byte(value)) => value,
            Some(crate::EscapeValue::Scalar(value)) => {
                u8::try_from(u32::from(value)).unwrap_or_default()
            }
            None => 0,
        }
    }
}

impl TupleIndexExpr {
    pub fn index(&self) -> Option<u32> {
        let literal = direct_token::<IntLit>(self.syntax())?;
        let text = literal.syntax().text();
        if text.is_empty()
            || !text.bytes().all(|byte| byte.is_ascii_digit())
            || (text.len() > 1 && text.starts_with('0'))
        {
            return None;
        }
        text.parse().ok()
    }
}

fn decode_char_body(body: &str, mode: crate::EscapeMode) -> Option<crate::EscapeValue> {
    if body.starts_with('\\') {
        let escape = crate::scan_escape(body, 0, mode);
        if !escape.valid || escape.end != body.len() {
            return None;
        }
        return escape.value;
    }

    let mut chars = body.chars();
    let value = chars.next()?;
    chars
        .next()
        .is_none()
        .then_some(crate::EscapeValue::Scalar(value))
}

impl SourceFile {
    pub fn test_decls(&self) -> impl Iterator<Item = TestDecl> + '_ {
        self.syntax().children().filter_map(TestDecl::cast)
    }

    pub fn property_decls(&self) -> impl Iterator<Item = PropertyDecl> + '_ {
        self.syntax().children().filter_map(PropertyDecl::cast)
    }
}

impl TestDecl {
    pub fn name(&self) -> Option<StringLit> {
        direct_token(self.syntax())
    }

    pub fn body(&self) -> Option<Block> {
        direct_child(self.syntax())
    }
}

impl PropertyDecl {
    pub fn name(&self) -> Option<StringLit> {
        direct_token(self.syntax())
    }

    pub fn params(&self) -> Option<ParamList> {
        direct_child(self.syntax())
    }

    pub fn body(&self) -> Option<Block> {
        direct_child(self.syntax())
    }
}

impl ParamList {
    pub fn params(&self) -> impl Iterator<Item = Param> + '_ {
        self.syntax().children().filter_map(Param::cast)
    }
}

impl Param {
    pub fn is_mutable(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .any(|element| matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::VAR_KW))
    }

    pub fn name(&self) -> Option<Name> {
        direct_child(self.syntax())
    }

    pub fn ty(&self) -> Option<Type> {
        self.syntax().children().find_map(Type::cast)
    }
}

impl Name {
    pub fn token(&self) -> Option<SyntaxToken> {
        self.syntax()
            .children_with_tokens()
            .find_map(|element| match element {
                SyntaxElement::Token(token) => Some(token),
                SyntaxElement::Node(_) => None,
            })
    }
}

impl StringLit {
    pub fn text(&self) -> &str {
        self.syntax().text()
    }

    pub fn value(&self) -> Cow<'_, str> {
        crate::literal::decode_string_value(self.syntax().text())
    }
}

impl ByteStringLit {
    pub fn value(&self) -> Cow<'_, [u8]> {
        crate::literal::decode_byte_string_value(self.syntax().text())
    }
}

impl FormatSpec {
    pub fn name(&self) -> &str {
        let text = self
            .syntax()
            .text()
            .strip_prefix(':')
            .unwrap_or(self.syntax().text());
        let name = text.split_once('(').map_or(text, |(name, _)| name);
        name.strip_prefix("r#").unwrap_or(name)
    }

    pub fn arg(&self) -> Option<u128> {
        if self.syntax().has_error() {
            return None;
        }
        let text = self.syntax().text().strip_prefix(':')?;
        let (_, arg) = text.split_once('(')?;
        parse_integer(arg.strip_suffix(')')?)
    }
}

impl FStringExpr {
    pub fn pieces(&self) -> impl Iterator<Item = FStringPiece<'_>> + '_ {
        let syntax = self.syntax();
        let green_children = syntax.green().children();
        let mut pieces = Vec::new();

        for (index, element) in syntax.children_with_tokens().enumerate() {
            match element {
                SyntaxElement::Token(token) => {
                    let segment = match token.kind() {
                        SyntaxKind::FSTRING_START => crate::literal::FStringTextSegment::Start,
                        SyntaxKind::FSTRING_TEXT => crate::literal::FStringTextSegment::Middle,
                        SyntaxKind::FSTRING_END => crate::literal::FStringTextSegment::End,
                        _ => continue,
                    };
                    let Some(green_token) =
                        green_children.get(index).and_then(|child| child.as_token())
                    else {
                        continue;
                    };
                    push_fstring_text(
                        &mut pieces,
                        crate::literal::decode_fstring_text(green_token.text(), segment),
                    );
                }
                SyntaxElement::Node(interpolation)
                    if interpolation.kind() == SyntaxKind::FSTRING_INTERP =>
                {
                    let expr = interpolation.children().find_map(Expr::cast);
                    let spec = interpolation.children_with_tokens().find_map(|element| {
                        let SyntaxElement::Token(token) = element else {
                            return None;
                        };
                        FormatSpec::cast(token)
                    });
                    pieces.push(FStringPiece::Interp { expr, spec });
                }
                SyntaxElement::Node(_) => {}
            }
        }

        pieces.into_iter()
    }
}

fn push_fstring_text<'a>(pieces: &mut Vec<FStringPiece<'a>>, text: Cow<'a, str>) {
    if text.is_empty() {
        return;
    }

    if let Some(FStringPiece::Text(previous)) = pieces.last_mut() {
        let mut joined = std::mem::replace(previous, Cow::Borrowed("")).into_owned();
        joined.push_str(text.as_ref());
        *previous = Cow::Owned(joined);
    } else {
        pieces.push(FStringPiece::Text(text));
    }
}

#[derive(Clone, Debug)]
pub enum Type {
    PathType(PathType),
    SelfType(SelfType),
    BorrowType(BorrowType),
    OwnedType(OwnedType),
    TupleType(TupleType),
    ParenType(ParenType),
    InferType(InferType),
    FnPtrType(FnPtrType),
}

ast_node!(PathType, PATH_TYPE);
ast_node!(SelfType, SELF_TYPE);
ast_node!(BorrowType, BORROW_TYPE);
ast_node!(OwnedType, OWNED_TYPE);
ast_node!(TupleType, TUPLE_TYPE);
ast_node!(ParenType, PAREN_TYPE);
ast_node!(InferType, INFER_TYPE);
ast_node!(FnPtrType, FN_PTR_TYPE);

impl AstNode for Type {
    fn can_cast(kind: SyntaxKind) -> bool {
        matches!(
            kind,
            SyntaxKind::PATH_TYPE
                | SyntaxKind::SELF_TYPE
                | SyntaxKind::BORROW_TYPE
                | SyntaxKind::OWNED_TYPE
                | SyntaxKind::TUPLE_TYPE
                | SyntaxKind::PAREN_TYPE
                | SyntaxKind::INFER_TYPE
                | SyntaxKind::FN_PTR_TYPE
        )
    }

    fn cast(node: SyntaxNode) -> Option<Self> {
        Some(match node.kind() {
            SyntaxKind::PATH_TYPE => Self::PathType(PathType::cast(node)?),
            SyntaxKind::SELF_TYPE => Self::SelfType(SelfType::cast(node)?),
            SyntaxKind::BORROW_TYPE => Self::BorrowType(BorrowType::cast(node)?),
            SyntaxKind::OWNED_TYPE => Self::OwnedType(OwnedType::cast(node)?),
            SyntaxKind::TUPLE_TYPE => Self::TupleType(TupleType::cast(node)?),
            SyntaxKind::PAREN_TYPE => Self::ParenType(ParenType::cast(node)?),
            SyntaxKind::INFER_TYPE => Self::InferType(InferType::cast(node)?),
            SyntaxKind::FN_PTR_TYPE => Self::FnPtrType(FnPtrType::cast(node)?),
            _ => return None,
        })
    }

    fn syntax(&self) -> &SyntaxNode {
        match self {
            Self::PathType(node) => node.syntax(),
            Self::SelfType(node) => node.syntax(),
            Self::BorrowType(node) => node.syntax(),
            Self::OwnedType(node) => node.syntax(),
            Self::TupleType(node) => node.syntax(),
            Self::ParenType(node) => node.syntax(),
            Self::InferType(node) => node.syntax(),
            Self::FnPtrType(node) => node.syntax(),
        }
    }
}

fn direct_child<N: AstNode>(node: &SyntaxNode) -> Option<N> {
    node.children().find_map(N::cast)
}

fn direct_token<T: AstToken>(node: &SyntaxNode) -> Option<T> {
    node.children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) => T::cast(token),
            SyntaxElement::Node(_) => None,
        })
}
