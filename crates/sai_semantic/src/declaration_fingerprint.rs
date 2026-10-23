use crate::SymbolKind;
use sai_stable_hash::{Fingerprint, StableHasher};
use sai_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken};
use std::fmt;

/// Failure to replace the declaration's own name while fingerprinting it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeclarationFingerprintError {
    NameTokenNotInDeclaration,
}

impl fmt::Display for DeclarationFingerprintError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NameTokenNotInDeclaration => {
                formatter.write_str("the symbol name token is not in its declaration")
            }
        }
    }
}

impl std::error::Error for DeclarationFingerprintError {}

/// Computes the persistent symbol fingerprint defined by impl/12 §12.2.4.
///
/// The supplied name token is replaced with empty text. Pass `None` for
/// declarations whose symbol has no own name token, and pass the string token
/// for `test` and `property` declarations.
pub fn declaration_fingerprint(
    kind: SymbolKind,
    declaration: &SyntaxNode,
    name_token: Option<&SyntaxToken>,
) -> Result<Fingerprint, DeclarationFingerprintError> {
    let mut hasher = StableHasher::new();
    let kind_name = kind.to_string();
    hasher.write_raw_bytes(kind_name.as_bytes());
    hasher.write_tag(0);

    let mut stack = vec![SyntaxElement::Node(declaration.clone())];
    let mut matched_name_token = false;

    while let Some(element) = stack.pop() {
        match element {
            SyntaxElement::Node(node) if node.kind() == SyntaxKind::DOCS => {}
            SyntaxElement::Node(node) => {
                stack.extend(
                    node.children_with_tokens()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev(),
                );
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::NEWLINE => {}
            SyntaxElement::Token(token) => {
                let is_name_token = name_token.is_some_and(|name| token.is_same_token(name));
                if is_name_token {
                    matched_name_token = true;
                }

                // SyntaxKind's variant spelling is persisted in symbol indexes.
                // Renaming one is an index compatibility change, guarded by the
                // golden fingerprint test below.
                let token_kind = format!("{:?}", token.kind());
                write_length_prefixed(&mut hasher, token_kind.as_bytes());
                let text = if is_name_token { "" } else { token.text() };
                write_length_prefixed(&mut hasher, text.as_bytes());
            }
        }
    }

    if name_token.is_some() && !matched_name_token {
        return Err(DeclarationFingerprintError::NameTokenNotInDeclaration);
    }

    Ok(hasher.finish())
}

fn write_length_prefixed(hasher: &mut StableHasher, bytes: &[u8]) {
    hasher.write_len(u64::try_from(bytes.len()).unwrap_or(u64::MAX));
    hasher.write_raw_bytes(bytes);
}

#[cfg(test)]
mod tests {
    use super::{DeclarationFingerprintError, declaration_fingerprint};
    use crate::SymbolKind;
    use sai_syntax::{SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, parse};
    use sai_vfs::{FileId, SnapshotFile, SourceFile, WorkspaceSnapshot};
    use std::sync::Arc;

    fn parse_root(source: &str) -> SyntaxNode {
        let file_id = FileId::new(1);
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(file_id, source, false).expect("valid source")),
            "src/lib.sai",
            Some("acme:sample@1.0.0".to_owned()),
            None,
        )])
        .expect("valid snapshot");
        let parsed = parse(&snapshot, file_id).expect("parse succeeds");
        parsed.root
    }

    fn parse_function(source: &str) -> (SyntaxNode, SyntaxToken) {
        let root = parse_root(source);
        let declaration = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::FN_DECL)
            .expect("function declaration");
        let name_token = function_name(&declaration);
        (declaration, name_token)
    }

    fn function_name(declaration: &SyntaxNode) -> SyntaxToken {
        declaration
            .descendants()
            .find(|node| node.kind() == SyntaxKind::NAME)
            .and_then(|name| name.first_token())
            .expect("function name token")
    }

    fn parse_string_named_declaration(
        source: &str,
        declaration_kind: SyntaxKind,
    ) -> (SyntaxNode, SyntaxToken) {
        let root = parse_root(source);
        let declaration = root
            .descendants()
            .find(|node| node.kind() == declaration_kind)
            .expect("string-named declaration");
        let mut stack = vec![SyntaxElement::Node(declaration.clone())];
        let mut name_token = None;
        while let Some(element) = stack.pop() {
            match element {
                SyntaxElement::Node(node) => {
                    stack.extend(
                        node.children_with_tokens()
                            .collect::<Vec<_>>()
                            .into_iter()
                            .rev(),
                    );
                }
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::STRING => {
                    name_token = Some(token);
                    break;
                }
                SyntaxElement::Token(_) => {}
            }
        }
        (
            declaration,
            name_token.expect("declaration name string token"),
        )
    }

    #[test]
    fn rename_comments_trivia_and_newlines_do_not_change_fingerprint() {
        let (first_declaration, first_name) =
            parse_function("/// original docs\nfn original() -> i64 { 1 }\n");
        let (second_declaration, second_name) =
            parse_function("/// revised docs\nfn renamed() -> i64 {/* body comment */\n  1\n}\n");

        assert_eq!(
            declaration_fingerprint(SymbolKind::Fn, &first_declaration, Some(&first_name))
                .expect("first fingerprint"),
            declaration_fingerprint(SymbolKind::Fn, &second_declaration, Some(&second_name))
                .expect("second fingerprint")
        );
    }

    #[test]
    fn docs_nodes_are_excluded_from_the_token_sequence() {
        let first_root = parse_root("/// original documentation\nfn original() {}\n");
        let first_name = function_name(&first_root);
        let second_root = parse_root("/// rewritten documentation\nfn renamed() {}\n");
        let second_name = function_name(&second_root);

        assert_eq!(
            declaration_fingerprint(SymbolKind::Fn, &first_root, Some(&first_name))
                .expect("first fingerprint"),
            declaration_fingerprint(SymbolKind::Fn, &second_root, Some(&second_name))
                .expect("second fingerprint")
        );
    }

    #[test]
    fn test_name_strings_are_replaced() {
        let (first, first_name) = parse_string_named_declaration(
            "test \"original test name\" {}\n",
            SyntaxKind::TEST_DECL,
        );
        let (second, second_name) = parse_string_named_declaration(
            "test \"renamed test name\" {}\n",
            SyntaxKind::TEST_DECL,
        );

        assert_eq!(
            declaration_fingerprint(SymbolKind::Test, &first, Some(&first_name))
                .expect("first fingerprint"),
            declaration_fingerprint(SymbolKind::Test, &second, Some(&second_name))
                .expect("second fingerprint")
        );
    }

    #[test]
    fn property_name_strings_are_replaced() {
        let (first, first_name) = parse_string_named_declaration(
            "property \"original property name\" (value: U8) { value == value }\n",
            SyntaxKind::PROPERTY_DECL,
        );
        let (second, second_name) = parse_string_named_declaration(
            "property \"renamed property name\" (value: U8) { value == value }\n",
            SyntaxKind::PROPERTY_DECL,
        );

        assert_eq!(
            declaration_fingerprint(SymbolKind::Property, &first, Some(&first_name))
                .expect("first fingerprint"),
            declaration_fingerprint(SymbolKind::Property, &second, Some(&second_name))
                .expect("second fingerprint")
        );
    }

    #[test]
    fn signature_body_and_symbol_kind_changes_change_fingerprint() {
        let (original, original_name) = parse_function("fn value() -> i64 { 1 }\n");
        let (signature, signature_name) = parse_function("fn value() -> u64 { 1 }\n");
        let (body, body_name) = parse_function("fn value() -> i64 { 2 }\n");
        let original_fingerprint =
            declaration_fingerprint(SymbolKind::Fn, &original, Some(&original_name))
                .expect("original fingerprint");
        assert_eq!(
            original_fingerprint.to_string(),
            "6535243ebe5da9b6f388092a97b75ded"
        );

        assert_ne!(
            original_fingerprint,
            declaration_fingerprint(SymbolKind::Fn, &signature, Some(&signature_name))
                .expect("signature fingerprint")
        );
        assert_ne!(
            original_fingerprint,
            declaration_fingerprint(SymbolKind::Fn, &body, Some(&body_name))
                .expect("body fingerprint")
        );
        assert_ne!(
            original_fingerprint,
            declaration_fingerprint(SymbolKind::AssocFn, &original, Some(&original_name))
                .expect("associated function fingerprint")
        );
    }

    #[test]
    fn a_name_token_from_another_declaration_is_rejected() {
        let (declaration, _) = parse_function("fn same() {}\n");
        let (_, foreign_name) = parse_function("fn same() {}\n");

        assert_eq!(
            declaration_fingerprint(SymbolKind::Fn, &declaration, Some(&foreign_name)),
            Err(DeclarationFingerprintError::NameTokenNotInDeclaration)
        );
    }
}
