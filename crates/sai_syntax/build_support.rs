use std::collections::BTreeSet;
use std::fmt::Write as _;

#[derive(Debug)]
pub(crate) struct Node {
    pub(crate) ast_name: String,
    pub(crate) syntax_kind: String,
}

pub(crate) fn parse_nodes(grammar: &str, existing_types: &BTreeSet<String>) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut ast_names = BTreeSet::new();
    let mut syntax_kinds = BTreeSet::new();

    for (line_index, line) in grammar.lines().enumerate() {
        let line = line
            .split_once('#')
            .map_or(line, |(content, _)| content)
            .trim();
        if line.is_empty() {
            continue;
        }

        let fields = line.split_whitespace().collect::<Vec<_>>();
        if fields.len() != 3 || fields[0] != "node" {
            panic!(
                "sai.ungram:{}: expected `node <AstName> <SYNTAX_KIND>`",
                line_index + 1
            );
        }
        let ast_name = fields[1];
        let syntax_kind = fields[2];
        if !is_rust_type_name(ast_name) {
            panic!(
                "sai.ungram:{}: invalid AST node name `{ast_name}`",
                line_index + 1
            );
        }
        if existing_types.contains(ast_name) {
            panic!(
                "sai.ungram:{}: AST node name `{ast_name}` conflicts with a type already in ast.rs",
                line_index + 1
            );
        }
        if !syntax_kind
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
        {
            panic!(
                "sai.ungram:{}: invalid syntax kind `{syntax_kind}`",
                line_index + 1
            );
        }
        if !ast_names.insert(ast_name.to_owned()) {
            panic!(
                "sai.ungram:{}: duplicate AST node name `{ast_name}`",
                line_index + 1
            );
        }
        if !syntax_kinds.insert(syntax_kind.to_owned()) {
            panic!(
                "sai.ungram:{}: duplicate syntax kind `{syntax_kind}`",
                line_index + 1
            );
        }
        nodes.push(Node {
            ast_name: ast_name.to_owned(),
            syntax_kind: syntax_kind.to_owned(),
        });
    }

    assert!(!nodes.is_empty(), "sai.ungram declares no syntax nodes");
    nodes
}

fn is_rust_type_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_uppercase())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && name != "Self"
}

pub(crate) fn existing_ast_type_names(ast_source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let declaration_prefixes = [
        "pub(crate) struct ",
        "pub(crate) enum ",
        "pub(crate) trait ",
        "pub(crate) type ",
        "pub struct ",
        "pub enum ",
        "pub trait ",
        "pub type ",
        "struct ",
        "enum ",
        "trait ",
        "type ",
    ];

    for line in ast_source.lines() {
        let line = line.trim();
        if let Some(name) = declaration_prefixes
            .iter()
            .find_map(|prefix| line.strip_prefix(prefix))
            .and_then(first_identifier)
        {
            names.insert(name.to_owned());
        }

        if let Some(rest) = line.strip_prefix("ast_token!(") {
            if let Some(name) = first_identifier(rest) {
                names.insert(name.to_owned());
            }
        }
        if let Some(rest) = line.strip_prefix("ast_union!(") {
            if let Some(name) = first_identifier(rest) {
                names.insert(name.to_owned());
            }
        }
    }

    names.extend(imported_type_names(ast_source));
    names.extend(
        [
            "Box",
            "Clone",
            "Copy",
            "Default",
            "Drop",
            "Eq",
            "Fn",
            "FnMut",
            "FnOnce",
            "Hash",
            "IntoIterator",
            "Iterator",
            "Option",
            "Ord",
            "PartialEq",
            "PartialOrd",
            "Result",
            "Send",
            "Sized",
            "String",
            "Sync",
            "Vec",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    names
}

fn first_identifier(source: &str) -> Option<&str> {
    let source = source.trim_start();
    let end = source
        .bytes()
        .position(|byte| !byte.is_ascii_alphanumeric() && byte != b'_')
        .unwrap_or(source.len());
    (end != 0).then_some(&source[..end])
}

fn imported_type_names(ast_source: &str) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut statement = String::new();

    for line in ast_source.lines() {
        let line = line.trim();
        if statement.is_empty() && !line.starts_with("use ") {
            continue;
        }
        statement.push(' ');
        statement.push_str(line);
        if !line.ends_with(';') {
            continue;
        }

        let imports = statement
            .strip_prefix(" use ")
            .unwrap_or(&statement)
            .trim_end_matches(';');
        for import in imports.split(',') {
            let imported = import.split_once(" as ").map_or_else(
                || import.rsplit("::").next().unwrap_or(import),
                |(_, alias)| alias,
            );
            let imported = imported.trim_matches(|character: char| {
                !character.is_ascii_alphanumeric() && character != '_'
            });
            if is_rust_type_name(imported) {
                names.insert(imported.to_owned());
            }
        }
        statement.clear();
    }
    names
}

pub(crate) fn validate_node_kinds(nodes: &[Node], syntax_source: &str) {
    let enum_start = syntax_source
        .find("pub enum SyntaxKind {")
        .expect("find SyntaxKind declaration")
        + "pub enum SyntaxKind {".len();
    let variants = parse_syntax_kind_variants(&syntax_source[enum_start..]);
    let unique_variants = variants.iter().collect::<BTreeSet<_>>();
    assert_eq!(
        variants.len(),
        unique_variants.len(),
        "SyntaxKind contains duplicate variants"
    );
    let eof_index = variants
        .iter()
        .position(|name| name == "EOF")
        .expect("SyntaxKind contains EOF");
    let expected = variants
        .iter()
        .enumerate()
        .filter(|(index, name)| name.as_str() == "ERROR" || *index > eof_index)
        .map(|(_, name)| name.clone())
        .collect::<BTreeSet<_>>();
    let declared = nodes
        .iter()
        .map(|node| node.syntax_kind.clone())
        .collect::<BTreeSet<_>>();

    let missing = expected.difference(&declared).cloned().collect::<Vec<_>>();
    let extra = declared.difference(&expected).cloned().collect::<Vec<_>>();
    if !missing.is_empty() || !extra.is_empty() {
        panic!(
            "sai.ungram node inventory does not match SyntaxKind; missing: {missing:?}; \
             not syntax nodes: {extra:?}"
        );
    }
}

fn parse_syntax_kind_variants(source: &str) -> Vec<String> {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut variants = Vec::new();
    let mut found_end = false;

    while index < bytes.len() {
        skip_trivia(source, &mut index);
        if index >= bytes.len() {
            break;
        }
        if bytes[index] == b'}' {
            found_end = true;
            break;
        }
        if bytes[index..].starts_with(b"#[") {
            skip_attribute(source, &mut index);
            continue;
        }

        let name_start = index;
        while index < bytes.len() && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
        {
            index += 1;
        }
        assert_ne!(
            index, name_start,
            "expected a SyntaxKind variant or `}}` after comments and attributes"
        );
        variants.push(source[name_start..index].to_owned());

        skip_trivia(source, &mut index);
        if index < bytes.len() && bytes[index] == b',' {
            index += 1;
        } else if index < bytes.len() && bytes[index] == b'}' {
            found_end = true;
            break;
        } else {
            panic!("expected `,` or `}}` after a SyntaxKind variant");
        }
    }
    assert!(found_end, "find end of SyntaxKind declaration");
    variants
}

fn skip_trivia(source: &str, index: &mut usize) {
    let bytes = source.as_bytes();
    loop {
        while *index < bytes.len() && bytes[*index].is_ascii_whitespace() {
            *index += 1;
        }
        if *index >= bytes.len() {
            return;
        }
        if bytes[*index..].starts_with(b"//") {
            while *index < bytes.len() && bytes[*index] != b'\n' {
                *index += 1;
            }
        } else if bytes[*index..].starts_with(b"/*") {
            skip_block_comment(source, index);
        } else {
            return;
        }
    }
}

fn skip_block_comment(source: &str, index: &mut usize) {
    let bytes = source.as_bytes();
    let mut depth = 1usize;
    *index += 2;
    while *index < bytes.len() && depth > 0 {
        if bytes[*index..].starts_with(b"/*") {
            depth += 1;
            *index += 2;
        } else if bytes[*index..].starts_with(b"*/") {
            depth -= 1;
            *index += 2;
        } else {
            *index += 1;
        }
    }
    assert_eq!(depth, 0, "unterminated block comment in SyntaxKind");
}

fn skip_attribute(source: &str, index: &mut usize) {
    let bytes = source.as_bytes();
    let mut depth = 0usize;
    while *index < bytes.len() {
        if bytes[*index..].starts_with(b"//") {
            while *index < bytes.len() && bytes[*index] != b'\n' {
                *index += 1;
            }
        } else if bytes[*index..].starts_with(b"/*") {
            skip_block_comment(source, index);
        } else if bytes[*index..].starts_with(b"r") {
            if let Some(end) = raw_string_end(source, *index) {
                *index = end;
            } else {
                *index += 1;
            }
        } else if bytes[*index] == b'"' {
            skip_quoted_string(source, index);
        } else {
            match bytes[*index] {
                b'[' => {
                    depth += 1;
                    *index += 1;
                }
                b']' => {
                    depth -= 1;
                    *index += 1;
                    if depth == 0 {
                        return;
                    }
                }
                _ => *index += 1,
            }
        }
    }
    panic!("unterminated attribute in SyntaxKind declaration");
}

fn raw_string_end(source: &str, start: usize) -> Option<usize> {
    let bytes = source.as_bytes();
    if bytes.get(start) != Some(&b'r') {
        return None;
    }
    let mut quote = start + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let hashes = quote - start - 1;
    let mut index = quote + 1;
    while index < bytes.len() {
        if bytes[index] == b'"'
            && bytes
                .get(index + 1..index + 1 + hashes)
                .is_some_and(|suffix| suffix.iter().all(|byte| *byte == b'#'))
        {
            return Some(index + 1 + hashes);
        }
        index += 1;
    }
    panic!("unterminated raw string in SyntaxKind attribute");
}

fn skip_quoted_string(source: &str, index: &mut usize) {
    let bytes = source.as_bytes();
    *index += 1;
    while *index < bytes.len() {
        match bytes[*index] {
            b'\\' => *index = (*index + 2).min(bytes.len()),
            b'"' => {
                *index += 1;
                return;
            }
            _ => *index += 1,
        }
    }
    panic!("unterminated string in SyntaxKind attribute");
}

pub(crate) fn generate_nodes(nodes: &[Node]) -> String {
    let mut generated = String::from("// @generated from sai.ungram by build.rs.\n");
    for node in nodes {
        writeln!(
            generated,
            "ast_node!({}, {});",
            node.ast_name, node.syntax_kind
        )
        .expect("write to generated source");
    }

    generated.push_str("\npub const AST_NODE_KINDS: &[SyntaxKind] = &[\n");
    for node in nodes {
        writeln!(generated, "    SyntaxKind::{},", node.syntax_kind)
            .expect("write to generated source");
    }
    generated.push_str("];\n");
    generated
}

#[cfg(test)]
mod tests {
    use super::{Node, existing_ast_type_names, generate_nodes, parse_nodes, validate_node_kinds};
    use std::collections::BTreeSet;

    fn node(ast_name: &str, syntax_kind: &str) -> Node {
        Node {
            ast_name: ast_name.to_owned(),
            syntax_kind: syntax_kind.to_owned(),
        }
    }

    fn error_and_source_file() -> [Node; 2] {
        [node("Error", "ERROR"), node("SourceFile", "SOURCE_FILE")]
    }

    #[test]
    fn syntax_kind_inventory_includes_a_final_variant_without_a_trailing_comma() {
        let nodes = [
            node("Error", "ERROR"),
            node("SourceFile", "SOURCE_FILE"),
            node("LastNode", "LAST_NODE"),
        ];
        let source = "pub enum SyntaxKind {\nTOKEN,\nERROR,\nEOF,\nSOURCE_FILE,\nLAST_NODE\n}";
        validate_node_kinds(&nodes, source);
    }

    #[test]
    fn syntax_kind_inventory_rejects_omitting_a_final_variant_without_a_trailing_comma() {
        let nodes = error_and_source_file();
        let source = "pub enum SyntaxKind {\nTOKEN,\nERROR,\nEOF,\nSOURCE_FILE,\nLAST_NODE\n}";
        assert!(std::panic::catch_unwind(|| validate_node_kinds(&nodes, source)).is_err());
    }

    #[test]
    fn syntax_kind_inventory_includes_nodes_between_eof_and_source_file() {
        let nodes = [
            node("Error", "ERROR"),
            node("RecoveryNode", "RECOVERY_NODE"),
            node("SourceFile", "SOURCE_FILE"),
        ];
        let source = "pub enum SyntaxKind {\nTOKEN,\nERROR,\nEOF,\nRECOVERY_NODE,\nSOURCE_FILE\n}";
        validate_node_kinds(&nodes, source);
    }

    #[test]
    fn syntax_kind_inventory_ignores_braces_in_line_comments() {
        let nodes = [
            node("Error", "ERROR"),
            node("SourceFile", "SOURCE_FILE"),
            node("LastNode", "LAST_NODE"),
        ];
        let source = "pub enum SyntaxKind {\nTOKEN, // } is only a comment\nERROR,\nEOF,\nSOURCE_FILE,\nLAST_NODE\n}";
        validate_node_kinds(&nodes, source);
    }

    #[test]
    fn syntax_kind_inventory_ignores_nested_block_comments() {
        let nodes = [
            node("Error", "ERROR"),
            node("SourceFile", "SOURCE_FILE"),
            node("LastNode", "LAST_NODE"),
        ];
        let source = concat!(
            "pub enum SyntaxKind {\n",
            "TOKEN, /* } closes neither the enum nor this nested comment\n",
            "  /* } */ nested comment */\n",
            "ERROR,\n",
            "EOF,\n",
            "SOURCE_FILE,\n",
            "LAST_NODE\n",
            "}",
        );
        validate_node_kinds(&nodes, source);
    }

    #[test]
    fn syntax_kind_inventory_supports_variants_on_one_line() {
        let nodes = [
            node("Error", "ERROR"),
            node("SourceFile", "SOURCE_FILE"),
            node("LastNode", "LAST_NODE"),
        ];
        let source = "pub enum SyntaxKind { TOKEN, ERROR, EOF, SOURCE_FILE, LAST_NODE }";
        validate_node_kinds(&nodes, source);
    }

    #[test]
    fn syntax_kind_inventory_skips_attributes_with_string_contents() {
        let nodes = [
            node("Error", "ERROR"),
            node("SourceFile", "SOURCE_FILE"),
            node("LastNode", "LAST_NODE"),
        ];
        let source = concat!(
            "pub enum SyntaxKind {\n",
            "TOKEN,\n",
            "ERROR,\n",
            "EOF,\n",
            "#[doc = \"comma, brace }, and // are inside this string\"]\n",
            "SOURCE_FILE,\n",
            "#[doc = r#\"raw, brace }, and // are inside this string\"#]\n",
            "LAST_NODE\n",
            "}",
        );
        validate_node_kinds(&nodes, source);
    }

    #[test]
    fn ast_type_names_reject_reserved_and_existing_names() {
        let ast_source = concat!(
            "use std::{borrow::Cow, collections::HashMap};\n",
            "use crate::{SyntaxElement, SyntaxKind};\n",
            "pub enum Expr {}\n",
            "ast_token!(Name, NAME);\n",
            "ast_union!(Type {",
        );
        let existing = existing_ast_type_names(ast_source);
        assert!(existing.contains("Cow"));
        assert!(existing.contains("HashMap"));
        assert!(existing.contains("SyntaxElement"));
        assert!(existing.contains("SyntaxKind"));
        assert!(existing.contains("Expr"));
        assert!(existing.contains("Name"));
        assert!(existing.contains("Type"));
        assert!(existing.contains("Option"));

        let result =
            std::panic::catch_unwind(|| parse_nodes("node Self SOURCE_FILE", &BTreeSet::new()));
        assert!(result.is_err());
    }

    #[test]
    fn parse_nodes_rejects_existing_ast_type_names() {
        let existing = BTreeSet::from(["Expr".to_owned()]);
        let result = std::panic::catch_unwind(|| parse_nodes("node Expr SOURCE_FILE", &existing));
        assert!(result.is_err());
    }

    #[test]
    fn generated_wrappers_and_node_kinds_share_the_declared_inventory() {
        let existing = BTreeSet::new();
        let nodes = parse_nodes("node Error ERROR\nnode SourceFile SOURCE_FILE", &existing);
        let generated = generate_nodes(&nodes);
        assert!(generated.contains("ast_node!(Error, ERROR);"));
        assert!(generated.contains("ast_node!(SourceFile, SOURCE_FILE);"));
        assert!(generated.contains("SyntaxKind::ERROR,"));
        assert!(generated.contains("SyntaxKind::SOURCE_FILE,"));
    }
}
