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

ast_token!(StringLit, STRING);

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
