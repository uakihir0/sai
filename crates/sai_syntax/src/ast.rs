use std::{borrow::Cow, collections::HashMap, marker::PhantomData};

use sai_vfs::FileId;

use crate::{
    SyntaxElement, SyntaxKind, SyntaxNode, SyntaxNodePtr, SyntaxNodePtrError, SyntaxToken,
};

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

pub struct AstPtr<N: AstNode> {
    raw: SyntaxNodePtr,
    marker: PhantomData<fn() -> N>,
}

impl<N: AstNode> Clone for AstPtr<N> {
    fn clone(&self) -> Self {
        Self {
            raw: self.raw.clone(),
            marker: PhantomData,
        }
    }
}

impl<N: AstNode> std::fmt::Debug for AstPtr<N> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("AstPtr").field(&self.raw).finish()
    }
}

impl<N: AstNode> PartialEq for AstPtr<N> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<N: AstNode> Eq for AstPtr<N> {}

impl<N: AstNode> std::hash::Hash for AstPtr<N> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&self.raw, state);
    }
}

impl<N: AstNode> AstPtr<N> {
    pub fn new(file: FileId, node: &N) -> Result<Self, SyntaxNodePtrError> {
        Ok(Self {
            raw: SyntaxNodePtr::new(file, node.syntax())?,
            marker: PhantomData,
        })
    }

    pub const fn file(&self) -> FileId {
        self.raw.file()
    }

    pub fn syntax_node_ptr(&self) -> SyntaxNodePtr {
        self.raw.clone()
    }

    pub fn resolve(&self, file: FileId, root: &SyntaxNode) -> Option<N> {
        N::cast(self.raw.resolve(file, root)?)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ErasedAstId(u32);

impl ErasedAstId {
    pub const fn new(index: u32) -> Self {
        Self(index)
    }

    pub const fn index(self) -> u32 {
        self.0
    }
}

pub struct AstId<N: AstNode> {
    raw: ErasedAstId,
    file: FileId,
    identity: crate::red::RootIdentity,
    marker: PhantomData<fn() -> N>,
}

impl<N: AstNode> Copy for AstId<N> {}

impl<N: AstNode> Clone for AstId<N> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<N: AstNode> std::fmt::Debug for AstId<N> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_tuple("AstId").field(&self.raw).finish()
    }
}

impl<N: AstNode> PartialEq for AstId<N> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw && self.file == other.file && self.identity == other.identity
    }
}

impl<N: AstNode> Eq for AstId<N> {}

impl<N: AstNode> std::hash::Hash for AstId<N> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        std::hash::Hash::hash(&(&self.raw, &self.file, &self.identity), state);
    }
}

impl<N: AstNode> AstId<N> {
    pub fn from_erased(map: &AstIdMap, raw: ErasedAstId) -> Option<Self> {
        let pointer = map.pointer(raw)?;
        N::can_cast(pointer.kind()).then_some(Self {
            raw,
            file: map.file,
            identity: map.identity,
            marker: PhantomData,
        })
    }

    pub const fn erase(self) -> ErasedAstId {
        self.raw
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AstIdMapError {
    FileMismatch,
    NotFileRoot,
    UnboundRoot,
    TooManyNodes,
}

#[derive(Clone, Debug)]
pub struct AstIdMap {
    file: FileId,
    identity: crate::red::RootIdentity,
    pointers: Box<[SyntaxNodePtr]>,
    ids: HashMap<SyntaxNodePtr, ErasedAstId>,
}

impl AstIdMap {
    pub fn new(file: FileId, root: &SyntaxNode) -> Result<Self, AstIdMapError> {
        if !root.belongs_to_file(file) {
            return Err(AstIdMapError::FileMismatch);
        }
        if root.kind() != SyntaxKind::SOURCE_FILE || root.parent().is_some() {
            return Err(AstIdMapError::NotFileRoot);
        }
        if matches!(root.root_identity(), crate::red::RootIdentity::Tree(_)) {
            return Err(AstIdMapError::UnboundRoot);
        }

        let mut pointers = Vec::new();
        let mut ids = HashMap::new();
        for node in root.descendants().filter(|node| is_item_like(node.kind())) {
            let raw = ErasedAstId::new(
                u32::try_from(pointers.len()).map_err(|_| AstIdMapError::TooManyNodes)?,
            );
            let pointer =
                SyntaxNodePtr::new(file, &node).map_err(|_| AstIdMapError::FileMismatch)?;
            ids.insert(pointer.clone(), raw);
            pointers.push(pointer);
        }

        Ok(Self {
            file,
            identity: root.root_identity(),
            pointers: pointers.into_boxed_slice(),
            ids,
        })
    }

    pub const fn file(&self) -> FileId {
        self.file
    }

    pub fn len(&self) -> usize {
        self.pointers.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pointers.is_empty()
    }

    pub fn pointer(&self, id: ErasedAstId) -> Option<SyntaxNodePtr> {
        self.pointers
            .get(usize::try_from(id.index()).ok()?)
            .cloned()
    }

    pub fn get<N: AstNode>(&self, id: AstId<N>) -> Option<AstPtr<N>> {
        if id.file != self.file || id.identity != self.identity {
            return None;
        }
        let raw = self.pointer(id.erase())?;
        N::can_cast(raw.kind()).then_some(AstPtr {
            raw,
            marker: PhantomData,
        })
    }

    pub fn ast_id<N: AstNode>(&self, node: &N) -> Option<AstId<N>> {
        if self.identity != node.syntax().root_identity()
            || !node.syntax().belongs_to_file(self.file)
        {
            return None;
        }

        let pointer = SyntaxNodePtr::new(self.file, node.syntax()).ok()?;
        let raw = *self.ids.get(&pointer)?;
        Some(AstId {
            raw,
            file: self.file,
            identity: self.identity,
            marker: PhantomData,
        })
    }
}

fn is_item_like(kind: SyntaxKind) -> bool {
    matches!(
        kind,
        SyntaxKind::USE_DECL
            | SyntaxKind::FN_DECL
            | SyntaxKind::TYPE_PARAM
            | SyntaxKind::CONST_PARAM
            | SyntaxKind::TYPE_DECL
            | SyntaxKind::DISTINCT_DECL
            | SyntaxKind::ENUM_DECL
            | SyntaxKind::FIELD_DECL
            | SyntaxKind::VARIANT_DECL
            | SyntaxKind::VARIANT_FIELD
            | SyntaxKind::RESOURCE_DECL
            | SyntaxKind::TRAIT_DECL
            | SyntaxKind::ASSOC_TYPE_DECL
            | SyntaxKind::IMPL_DECL
            | SyntaxKind::ASSOC_TYPE_DEF
            | SyntaxKind::CONST_DECL
            | SyntaxKind::INVARIANT_DECL
            | SyntaxKind::EFFECT_DECL
            | SyntaxKind::INTERFACE_DECL
            | SyntaxKind::IFACE_RESOURCE
            | SyntaxKind::WORLD_DECL
            | SyntaxKind::WORLD_IMPORT
            | SyntaxKind::WORLD_EXPORT
            | SyntaxKind::WORLD_INCLUDE
            | SyntaxKind::EXTERN_BLOCK
            | SyntaxKind::TEST_DECL
            | SyntaxKind::PROPERTY_DECL
    )
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

macro_rules! ast_union {
    ($name:ident { $($variant:ident($node:ident, $kind:ident)),+ $(,)? }) => {
        #[derive(Clone, Debug)]
        pub enum $name {
            $($variant($node)),+
        }

        impl AstNode for $name {
            fn can_cast(kind: SyntaxKind) -> bool {
                matches!(kind, $(SyntaxKind::$kind)|+)
            }

            fn cast(node: SyntaxNode) -> Option<Self> {
                match node.kind() {
                    $(SyntaxKind::$kind => Some(Self::$variant($node::cast(node)?)),)+
                    _ => None,
                }
            }

            fn syntax(&self) -> &SyntaxNode {
                match self {
                    $(Self::$variant(node) => node.syntax(),)+
                }
            }
        }
    };
}

include!(concat!(env!("OUT_DIR"), "/ast_nodes.rs"));

ast_token!(IntLit, INT);
ast_token!(FloatLit, FLOAT);
ast_token!(StringLit, STRING);
ast_token!(ByteStringLit, BYTE_STRING);
ast_token!(CharLit, CHAR);
ast_token!(ByteLit, BYTE);
ast_token!(FormatSpec, FSTRING_SPEC);
ast_token!(PkgPath, PKG_PATH);

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

ast_union!(Item {
    Use(UseDecl, USE_DECL),
    Function(FnDecl, FN_DECL),
    Type(TypeDecl, TYPE_DECL),
    Enum(EnumDecl, ENUM_DECL),
    Distinct(DistinctDecl, DISTINCT_DECL),
    Resource(ResourceDecl, RESOURCE_DECL),
    Trait(TraitDecl, TRAIT_DECL),
    Impl(ImplDecl, IMPL_DECL),
    Const(ConstDecl, CONST_DECL),
    Invariant(InvariantDecl, INVARIANT_DECL),
    Effect(EffectDecl, EFFECT_DECL),
    Interface(InterfaceDecl, INTERFACE_DECL),
    World(WorldDecl, WORLD_DECL),
    ExternBlock(ExternBlock, EXTERN_BLOCK),
    Test(TestDecl, TEST_DECL),
    Property(PropertyDecl, PROPERTY_DECL),
});

ast_union!(Stmt {
    Let(LetStmt, LET_STMT),
    Var(VarStmt, VAR_STMT),
    Assign(AssignStmt, ASSIGN_STMT),
    Assert(AssertStmt, ASSERT_STMT),
    Expr(ExprStmt, EXPR_STMT),
});

ast_union!(GenericParam {
    Type(TypeParam, TYPE_PARAM),
    Const(ConstParam, CONST_PARAM),
});

ast_union!(Bound {
    Trait(TraitBound, TRAIT_BOUND),
    FnSugar(FnSugarBound, FN_SUGAR_BOUND),
});

ast_union!(Pattern {
    Or(OrPat, OR_PAT),
    At(AtPat, AT_PAT),
    Wildcard(WildcardPat, WILDCARD_PAT),
    Binding(BindingPat, BINDING_PAT),
    Literal(LiteralPat, LITERAL_PAT),
    Range(RangePat, RANGE_PAT),
    Tuple(TuplePat, TUPLE_PAT),
    Parenthesized(ParenPat, PAREN_PAT),
    Slice(SlicePat, SLICE_PAT),
    Constructor(CtorPat, CTOR_PAT),
    ImplicitMember(ImplicitMemberPat, IMPLICIT_MEMBER_PAT),
});

ast_union!(RangePatEndpoint {
    Literal(LiteralPat, LITERAL_PAT),
    ConstantPath(ConstPath, CONST_PATH),
});

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

impl Param {
    pub fn is_mutable(&self) -> bool {
        self.syntax()
            .children_with_tokens()
            .any(|element| matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::VAR_KW))
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

ast_union!(Type {
    PathType(PathType, PATH_TYPE),
    SelfType(SelfType, SELF_TYPE),
    BorrowType(BorrowType, BORROW_TYPE),
    OwnedType(OwnedType, OWNED_TYPE),
    TupleType(TupleType, TUPLE_TYPE),
    ParenType(ParenType, PAREN_TYPE),
    InferType(InferType, INFER_TYPE),
    FnPtrType(FnPtrType, FN_PTR_TYPE),
});

fn direct_child_at<N: AstNode>(node: &SyntaxNode, index: usize) -> Option<N> {
    node.children().filter_map(N::cast).nth(index)
}

fn direct_children_before<N: AstNode>(
    node: &SyntaxNode,
    boundary: SyntaxKind,
) -> impl Iterator<Item = N> + '_ {
    node.children_with_tokens()
        .take_while(
            move |element| !matches!(element, SyntaxElement::Token(token) if token.kind() == boundary),
        )
        .filter_map(|element| match element {
            SyntaxElement::Node(child) => N::cast(child),
            SyntaxElement::Token(_) => None,
        })
}

fn direct_child_before<N: AstNode>(
    node: &SyntaxNode,
    boundary: SyntaxKind,
    index: usize,
) -> Option<N> {
    direct_child_before_any(node, &[boundary], index)
}

fn direct_child_before_any<N: AstNode>(
    node: &SyntaxNode,
    boundaries: &[SyntaxKind],
    index: usize,
) -> Option<N> {
    node.children_with_tokens()
        .take_while(|element| {
            !matches!(element, SyntaxElement::Token(token) if boundaries.contains(&token.kind()))
        })
        .filter_map(|element| match element {
            SyntaxElement::Node(child) => N::cast(child),
            SyntaxElement::Token(_) => None,
        })
        .nth(index)
}

fn direct_child_after<N: AstNode>(
    node: &SyntaxNode,
    boundary: SyntaxKind,
    index: usize,
) -> Option<N> {
    direct_child_after_any(node, &[boundary], index)
}

fn direct_child_after_any<N: AstNode>(
    node: &SyntaxNode,
    boundaries: &[SyntaxKind],
    index: usize,
) -> Option<N> {
    let mut found_boundary = false;
    node.children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) if boundaries.contains(&token.kind()) => {
                found_boundary = true;
                None
            }
            SyntaxElement::Node(child) if found_boundary => N::cast(child),
            _ => None,
        })
        .nth(index)
}

fn direct_token<T: AstToken>(node: &SyntaxNode) -> Option<T> {
    node.children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) => T::cast(token),
            SyntaxElement::Node(_) => None,
        })
}

fn direct_token_of_kind(node: &SyntaxNode, kind: SyntaxKind) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == kind => Some(token),
            SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
        })
}

fn direct_token_of_kinds(node: &SyntaxNode, kinds: &[SyntaxKind]) -> Option<SyntaxToken> {
    node.children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if kinds.contains(&token.kind()) => Some(token),
            SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
        })
}

impl LiteralPat {
    pub fn minus_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::MINUS)
    }

    pub fn value_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kinds(
            self.syntax(),
            &[
                SyntaxKind::INT,
                SyntaxKind::FLOAT,
                SyntaxKind::STRING,
                SyntaxKind::BYTE_STRING,
                SyntaxKind::CHAR,
                SyntaxKind::BYTE,
                SyntaxKind::TRUE_KW,
                SyntaxKind::FALSE_KW,
            ],
        )
    }
}

impl Discriminant {
    pub fn minus_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::MINUS)
    }
}

impl DistinctDecl {
    pub fn inner_pub_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::PUB_KW)
    }
}

impl TraitDecl {
    pub fn unsafe_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::UNSAFE_KW)
    }
}

impl ImplDecl {
    pub fn unsafe_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::UNSAFE_KW)
    }

    pub fn trait_ref(&self) -> Option<Type> {
        direct_token_of_kind(self.syntax(), SyntaxKind::FOR_KW)
            .and_then(|_| direct_child_before(self.syntax(), SyntaxKind::FOR_KW, 0))
    }

    pub fn inherent_type(&self) -> Option<Type> {
        if direct_token_of_kind(self.syntax(), SyntaxKind::FOR_KW).is_some()
            || self.interface_path().is_some()
        {
            return None;
        }
        direct_child_at(self.syntax(), 0)
    }
}

impl ExternBlock {
    pub fn unsafe_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::UNSAFE_KW)
    }
}

impl FnDecl {
    pub fn unsafe_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::UNSAFE_KW)
    }

    pub fn async_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::ASYNC_KW)
    }
}

impl ModuleHeader {
    pub fn unsafe_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::UNSAFE_KW)
    }
}

impl Visibility {
    pub fn pub_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::PUB_KW)
    }

    pub fn export_token(&self) -> Option<SyntaxToken> {
        direct_token_of_kind(self.syntax(), SyntaxKind::EXPORT_KW)
    }
}

impl SelfParam {
    pub fn is_mutable(&self) -> bool {
        direct_token_of_kind(self.syntax(), SyntaxKind::VAR_KW).is_some()
    }
}

impl GenericParamList {
    pub fn params(&self) -> impl Iterator<Item = GenericParam> + '_ {
        self.syntax().children().filter_map(GenericParam::cast)
    }
}

impl BoundList {
    pub fn bounds(&self) -> impl Iterator<Item = Bound> + '_ {
        self.syntax().children().filter_map(Bound::cast)
    }
}

impl WherePred {
    pub fn bounds(&self) -> impl Iterator<Item = Bound> + '_ {
        self.syntax().children().filter_map(Bound::cast)
    }
}
