use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

#[derive(Debug)]
pub(crate) struct Node {
    pub(crate) ast_name: String,
    pub(crate) syntax_kind: String,
    accessors: Vec<Accessor>,
}

#[derive(Debug)]
struct Accessor {
    kind: AccessorKind,
    name: String,
    ty: String,
    selector: Option<ChildSelector>,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum ChildSelector {
    Position(usize),
    BeforeToken {
        syntax_kind: String,
        index: usize,
    },
    AfterToken {
        syntax_kind: String,
        index: usize,
    },
    BeforeTokens {
        syntax_kinds: Vec<String>,
        index: usize,
    },
    AfterTokens {
        syntax_kinds: Vec<String>,
        index: usize,
    },
}

#[derive(Debug, Eq, PartialEq)]
enum AccessorKind {
    Child,
    Children,
    ChildrenBefore,
    Token,
}

#[derive(Debug, Default)]
pub(crate) struct ExistingAstTypes {
    names: BTreeSet<String>,
    unions: BTreeSet<String>,
    union_members: BTreeMap<String, BTreeSet<String>>,
    tokens: BTreeSet<String>,
    methods: BTreeMap<String, BTreeSet<String>>,
}

pub(crate) fn parse_nodes(grammar: &str, existing_types: &ExistingAstTypes) -> Vec<Node> {
    let mut nodes = Vec::new();
    let mut ast_names = BTreeSet::new();
    let mut syntax_kinds = BTreeSet::new();
    let mut accessors = Vec::new();

    for (line_index, line) in grammar.lines().enumerate() {
        let line = line
            .split_once('#')
            .map_or(line, |(content, _)| content)
            .trim();
        if line.is_empty() {
            continue;
        }

        let fields = line.split_whitespace().collect::<Vec<_>>();
        match fields.first().copied() {
            Some("node") => {
                if fields.len() != 3 {
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
                if existing_types.names.contains(ast_name) {
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
                    accessors: Vec::new(),
                });
            }
            Some(
                kind @ ("child" | "child_before" | "child_after" | "child_before_any"
                | "child_after_any" | "children" | "children_before" | "token"),
            ) => {
                let expected_fields = match kind {
                    "child" => 5,
                    "child_before" | "child_after" | "child_before_any" | "child_after_any" => 6,
                    "children_before" => 5,
                    _ => 4,
                };
                if fields.len() != expected_fields {
                    panic!(
                        "sai.ungram:{}: expected `{kind} <Parent> <role> <AstType>{}`",
                        line_index + 1,
                        match kind {
                            "child" => " <index>",
                            "child_before" | "child_after" => " <TOKEN_KIND> <index>",
                            "child_before_any" | "child_after_any" => {
                                " <TOKEN_KIND|TOKEN_KIND...> <index>"
                            }
                            "children_before" => " <TOKEN_KIND>",
                            _ => "",
                        },
                    );
                }
                let role_name = fields[2];
                if !is_rust_value_name(role_name)
                    || matches!(role_name, "can_cast" | "cast" | "syntax")
                {
                    panic!(
                        "sai.ungram:{}: invalid AST role name `{role_name}`",
                        line_index + 1
                    );
                }
                let accessor_kind = match kind {
                    "child" | "child_before" | "child_after" | "child_before_any"
                    | "child_after_any" => AccessorKind::Child,
                    "children" => AccessorKind::Children,
                    "children_before" => AccessorKind::ChildrenBefore,
                    "token" => AccessorKind::Token,
                    _ => unreachable!(),
                };
                let selector = match kind {
                    "child" => Some(ChildSelector::Position(parse_child_index(
                        fields[4],
                        line_index + 1,
                    ))),
                    "child_before" | "child_after" => {
                        let syntax_kind = fields[4];
                        if !is_syntax_kind_name(syntax_kind) {
                            panic!(
                                "sai.ungram:{}: invalid child boundary token kind `{syntax_kind}`",
                                line_index + 1
                            );
                        }
                        let index = parse_child_index(fields[5], line_index + 1);
                        Some(if kind == "child_before" {
                            ChildSelector::BeforeToken {
                                syntax_kind: syntax_kind.to_owned(),
                                index,
                            }
                        } else {
                            ChildSelector::AfterToken {
                                syntax_kind: syntax_kind.to_owned(),
                                index,
                            }
                        })
                    }
                    "child_before_any" | "child_after_any" => {
                        let syntax_kinds = parse_boundary_token_kinds(fields[4], line_index + 1);
                        let index = parse_child_index(fields[5], line_index + 1);
                        Some(if kind == "child_before_any" {
                            ChildSelector::BeforeTokens {
                                syntax_kinds,
                                index,
                            }
                        } else {
                            ChildSelector::AfterTokens {
                                syntax_kinds,
                                index,
                            }
                        })
                    }
                    "children_before" => {
                        let syntax_kind = fields[4];
                        if !is_syntax_kind_name(syntax_kind) {
                            panic!(
                                "sai.ungram:{}: invalid child boundary token kind `{syntax_kind}`",
                                line_index + 1
                            );
                        }
                        Some(ChildSelector::BeforeToken {
                            syntax_kind: syntax_kind.to_owned(),
                            index: 0,
                        })
                    }
                    _ => None,
                };
                accessors.push((
                    line_index + 1,
                    fields[1].to_owned(),
                    Accessor {
                        kind: accessor_kind,
                        name: role_name.to_owned(),
                        ty: fields[3].to_owned(),
                        selector,
                    },
                ));
            }
            Some(_) | None => panic!(
                "sai.ungram:{}: expected a `node`, child-role, `children`, or `token` declaration",
                line_index + 1
            ),
        }
    }

    assert!(!nodes.is_empty(), "sai.ungram declares no syntax nodes");
    let mut declared_roles = BTreeSet::new();
    let mut declared_child_selectors = Vec::<(String, String, ChildSelector)>::new();
    for (line, parent_name, accessor) in accessors {
        let Some(parent) = nodes.iter_mut().find(|node| node.ast_name == parent_name) else {
            panic!("sai.ungram:{line}: unknown AST node `{parent_name}`");
        };
        if existing_types
            .methods
            .get(&parent_name)
            .is_some_and(|methods| methods.contains(&accessor.name))
        {
            panic!(
                "sai.ungram:{line}: AST role `{}::{}` conflicts with a hand-written accessor",
                parent_name, accessor.name
            );
        }
        if !declared_roles.insert((parent_name.clone(), accessor.name.clone())) {
            panic!(
                "sai.ungram:{line}: duplicate AST role `{}::{}`",
                parent_name, accessor.name
            );
        }
        match accessor.kind {
            AccessorKind::Child | AccessorKind::Children | AccessorKind::ChildrenBefore => {
                if !ast_names.contains(&accessor.ty)
                    && !existing_types.unions.contains(&accessor.ty)
                {
                    panic!(
                        "sai.ungram:{line}: `{}` is not a known AST node or union",
                        accessor.ty
                    );
                }
                if accessor.kind == AccessorKind::Child {
                    let selector = accessor
                        .selector
                        .as_ref()
                        .expect("child roles have a selector");
                    if declared_child_selectors.iter().any(
                        |(declared_parent, declared_ty, declared_selector)| {
                            declared_parent == &parent_name
                                && child_roles_overlap(
                                    declared_ty,
                                    &accessor.ty,
                                    declared_selector,
                                    selector,
                                    existing_types,
                                )
                        },
                    ) {
                        panic!(
                            "sai.ungram:{line}: two AST roles select {} child {:?}",
                            accessor.ty, selector
                        );
                    }
                    declared_child_selectors.push((
                        parent_name.clone(),
                        accessor.ty.clone(),
                        selector.clone(),
                    ));
                }
            }
            AccessorKind::Token => {
                if !existing_types.tokens.contains(&accessor.ty) {
                    panic!(
                        "sai.ungram:{line}: `{}` is not a known AST token",
                        accessor.ty
                    );
                }
            }
        }
        parent.accessors.push(accessor);
    }
    nodes
}

fn parse_child_index(value: &str, line: usize) -> usize {
    value.parse::<usize>().unwrap_or_else(|_| {
        panic!("sai.ungram:{line}: child position `{value}` is not a non-negative integer")
    })
}

fn parse_boundary_token_kinds(value: &str, line: usize) -> Vec<String> {
    let mut syntax_kinds = Vec::new();
    let mut unique_syntax_kinds = BTreeSet::new();
    for syntax_kind in value.split('|') {
        if !is_syntax_kind_name(syntax_kind) {
            panic!("sai.ungram:{line}: invalid child boundary token kind `{syntax_kind}`");
        }
        if !unique_syntax_kinds.insert(syntax_kind) {
            panic!("sai.ungram:{line}: duplicate child boundary token kind `{syntax_kind}`");
        }
        syntax_kinds.push(syntax_kind.to_owned());
    }
    syntax_kinds
}

fn child_roles_overlap(
    left_ty: &str,
    right_ty: &str,
    left: &ChildSelector,
    right: &ChildSelector,
    existing_types: &ExistingAstTypes,
) -> bool {
    let left_members = ast_type_members(left_ty, existing_types);
    let right_members = ast_type_members(right_ty, existing_types);
    if !left_members
        .iter()
        .any(|member| right_members.contains(member))
    {
        return false;
    }
    child_selectors_overlap(left, right, &left_members, &right_members)
}

fn child_selectors_overlap(
    left: &ChildSelector,
    right: &ChildSelector,
    left_members: &BTreeSet<String>,
    right_members: &BTreeSet<String>,
) -> bool {
    match (left, right) {
        (ChildSelector::Position(left_index), ChildSelector::Position(right_index)) => {
            rank_indexes_overlap(*left_index, *right_index, left_members, right_members)
        }
        (ChildSelector::Position(position_index), selector) => {
            let Some((is_before, _, boundary_index)) = boundary_selector(selector) else {
                return false;
            };
            if is_before {
                rank_indexes_overlap(*position_index, boundary_index, left_members, right_members)
            } else {
                *position_index >= boundary_index || has_type_only(right_members, left_members)
            }
        }
        (selector, ChildSelector::Position(position_index)) => {
            let Some((is_before, _, boundary_index)) = boundary_selector(selector) else {
                return false;
            };
            if is_before {
                rank_indexes_overlap(boundary_index, *position_index, left_members, right_members)
            } else {
                *position_index >= boundary_index || has_type_only(left_members, right_members)
            }
        }
        _ => {
            let Some((left_before, left_kinds, left_index)) = boundary_selector(left) else {
                return false;
            };
            let Some((right_before, right_kinds, right_index)) = boundary_selector(right) else {
                return false;
            };
            match (left_before, right_before) {
                (true, true) => {
                    rank_indexes_overlap(left_index, right_index, left_members, right_members)
                }
                (false, false) => {
                    let shared_boundary = token_sets_intersect(&left_kinds, &right_kinds)
                        && rank_indexes_overlap(
                            left_index,
                            right_index,
                            left_members,
                            right_members,
                        );
                    let left_boundary_first = has_token_only(&left_kinds, &right_kinds)
                        && (left_index >= right_index
                            || has_type_only(right_members, left_members));
                    let right_boundary_first = has_token_only(&right_kinds, &left_kinds)
                        && (right_index >= left_index
                            || has_type_only(left_members, right_members));
                    shared_boundary || left_boundary_first || right_boundary_first
                }
                (true, false) => {
                    has_token_only(&right_kinds, &left_kinds)
                        && (left_index >= right_index || has_type_only(right_members, left_members))
                }
                (false, true) => {
                    has_token_only(&left_kinds, &right_kinds)
                        && (right_index >= left_index || has_type_only(left_members, right_members))
                }
            }
        }
    }
}

fn rank_indexes_overlap(
    left_index: usize,
    right_index: usize,
    left_members: &BTreeSet<String>,
    right_members: &BTreeSet<String>,
) -> bool {
    left_index == right_index
        || (left_index > right_index && has_type_only(left_members, right_members))
        || (right_index > left_index && has_type_only(right_members, left_members))
}

fn has_type_only(left: &BTreeSet<String>, right: &BTreeSet<String>) -> bool {
    left.iter().any(|member| !right.contains(member))
}

fn token_sets_intersect(left: &[&str], right: &[&str]) -> bool {
    left.iter().any(|kind| right.contains(kind))
}

fn has_token_only(left: &[&str], right: &[&str]) -> bool {
    left.iter().any(|kind| !right.contains(kind))
}

fn boundary_selector(selector: &ChildSelector) -> Option<(bool, Vec<&str>, usize)> {
    match selector {
        ChildSelector::BeforeToken { syntax_kind, index } => {
            Some((true, vec![syntax_kind], *index))
        }
        ChildSelector::AfterToken { syntax_kind, index } => {
            Some((false, vec![syntax_kind], *index))
        }
        ChildSelector::BeforeTokens {
            syntax_kinds,
            index,
        } => Some((
            true,
            syntax_kinds.iter().map(String::as_str).collect(),
            *index,
        )),
        ChildSelector::AfterTokens {
            syntax_kinds,
            index,
        } => Some((
            false,
            syntax_kinds.iter().map(String::as_str).collect(),
            *index,
        )),
        ChildSelector::Position(_) => None,
    }
}

fn is_syntax_kind_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn is_rust_type_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_uppercase())
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && name != "Self"
}

fn is_rust_value_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(byte) if byte.is_ascii_lowercase() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && name != "_"
        && !matches!(
            name,
            "abstract"
                | "as"
                | "async"
                | "await"
                | "become"
                | "box"
                | "break"
                | "const"
                | "continue"
                | "crate"
                | "do"
                | "dyn"
                | "else"
                | "enum"
                | "extern"
                | "false"
                | "final"
                | "fn"
                | "for"
                | "gen"
                | "if"
                | "impl"
                | "in"
                | "let"
                | "loop"
                | "match"
                | "macro"
                | "mod"
                | "move"
                | "mut"
                | "override"
                | "pub"
                | "priv"
                | "ref"
                | "return"
                | "self"
                | "static"
                | "struct"
                | "super"
                | "trait"
                | "true"
                | "try"
                | "type"
                | "typeof"
                | "unsafe"
                | "unsized"
                | "union"
                | "use"
                | "virtual"
                | "where"
                | "while"
                | "yield"
        )
}

pub(crate) fn existing_ast_types(ast_source: &str) -> ExistingAstTypes {
    let ast_source = strip_rust_comments_and_strings(ast_source);
    let mut types = ExistingAstTypes::default();
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
            types.names.insert(name.to_owned());
        }

        if let Some(rest) = line.strip_prefix("ast_token!(") {
            if let Some(name) = first_identifier(rest) {
                types.names.insert(name.to_owned());
                types.tokens.insert(name.to_owned());
            }
        }
        if let Some(rest) = line.strip_prefix("ast_union!(") {
            if let Some(name) = first_identifier(rest) {
                types.names.insert(name.to_owned());
                types.unions.insert(name.to_owned());
            }
        }
    }

    let enum_names = ast_source
        .lines()
        .map(str::trim)
        .filter_map(|line| {
            ["pub(crate) enum ", "pub enum "]
                .iter()
                .find_map(|prefix| line.strip_prefix(prefix))
        })
        .filter_map(first_identifier)
        .collect::<BTreeSet<_>>();
    let ast_node_impls = ast_source
        .lines()
        .map(str::trim)
        .filter_map(|line| line.strip_prefix("impl AstNode for "))
        .filter_map(first_identifier)
        .collect::<BTreeSet<_>>();
    types.unions.extend(
        enum_names
            .intersection(&ast_node_impls)
            .map(|name| (*name).to_owned()),
    );

    types.names.extend(imported_type_names(&ast_source));
    types.names.extend(
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
    types.union_members = existing_ast_union_members(&ast_source, &types.unions);
    types.methods = existing_ast_methods(&ast_source);
    types
}

fn strip_rust_comments_and_strings(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;

    while index < bytes.len() {
        if bytes[index..].starts_with(b"//") {
            let start = index;
            while index < bytes.len() && bytes[index] != b'\n' {
                index += 1;
            }
            append_masked_source(&source[start..index], &mut output);
        } else if bytes[index..].starts_with(b"/*") {
            let start = index;
            skip_block_comment(source, &mut index);
            append_masked_source(&source[start..index], &mut output);
        } else if bytes[index] == b'r'
            && let Some(end) = raw_string_end(source, index)
        {
            append_masked_source(&source[index..end], &mut output);
            index = end;
        } else if bytes[index] == b'"' {
            let start = index;
            skip_quoted_string(source, &mut index);
            append_masked_source(&source[start..index], &mut output);
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }

    String::from_utf8(output).expect("masking Rust trivia preserves valid UTF-8")
}

fn append_masked_source(source: &str, output: &mut Vec<u8>) {
    output.extend(source.bytes().map(|byte| {
        if matches!(byte, b'\n' | b'\r') {
            byte
        } else {
            b' '
        }
    }));
}

fn existing_ast_union_members(
    ast_source: &str,
    unions: &BTreeSet<String>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut members = BTreeMap::<String, BTreeSet<String>>::new();
    let mut active_union = None::<String>;
    let mut active_body = String::new();

    for line in ast_source.lines().map(str::trim) {
        if active_union.is_some() {
            let end = line.find('}').unwrap_or(line.len());
            active_body.push_str(&line[..end]);
            active_body.push('\n');
            if end < line.len() {
                let union_name = active_union.take().expect("active union name");
                members
                    .entry(union_name)
                    .or_default()
                    .extend(union_variant_ast_types(&active_body));
                active_body.clear();
            }
            continue;
        }

        let union_name = line
            .strip_prefix("ast_union!(")
            .and_then(first_identifier)
            .or_else(|| {
                ["pub(crate) enum ", "pub enum "]
                    .iter()
                    .find_map(|prefix| line.strip_prefix(prefix))
                    .and_then(first_identifier)
                    .filter(|name| unions.contains(*name))
            });
        let Some(union_name) = union_name else {
            continue;
        };
        let Some((_, body)) = line.split_once('{') else {
            continue;
        };
        active_union = Some(union_name.to_owned());
        let end = body.find('}').unwrap_or(body.len());
        active_body.push_str(&body[..end]);
        active_body.push('\n');
        if end < body.len() {
            members
                .entry(union_name.to_owned())
                .or_default()
                .extend(union_variant_ast_types(&active_body));
            active_union = None;
            active_body.clear();
        }
    }

    members
}

fn union_variant_ast_types(body: &str) -> BTreeSet<String> {
    let mut members = BTreeSet::new();
    let mut cursor = 0;
    while let Some(open_relative) = body[cursor..].find('(') {
        let open = cursor + open_relative;
        let mut depth = 1usize;
        let mut close = None;
        for (relative, character) in body[open + 1..].char_indices() {
            match character {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        close = Some(open + 1 + relative);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(close) = close else {
            break;
        };
        if let Some(member) = first_identifier(
            body[open + 1..close]
                .split(',')
                .next()
                .unwrap_or_default()
                .trim(),
        ) {
            members.insert(member.to_owned());
        }
        cursor = close + 1;
    }
    members
}

fn ast_type_members(ast_type: &str, existing_types: &ExistingAstTypes) -> BTreeSet<String> {
    fn collect(
        ast_type: &str,
        existing_types: &ExistingAstTypes,
        visiting: &mut BTreeSet<String>,
        members: &mut BTreeSet<String>,
    ) {
        let Some(union_members) = existing_types.union_members.get(ast_type) else {
            members.insert(ast_type.to_owned());
            return;
        };
        if !visiting.insert(ast_type.to_owned()) {
            members.insert(ast_type.to_owned());
            return;
        }
        for member in union_members {
            collect(member, existing_types, visiting, members);
        }
        visiting.remove(ast_type);
    }

    let mut members = BTreeSet::new();
    collect(ast_type, existing_types, &mut BTreeSet::new(), &mut members);
    members
}

fn existing_ast_methods(ast_source: &str) -> BTreeMap<String, BTreeSet<String>> {
    let mut methods = BTreeMap::<String, BTreeSet<String>>::new();
    let mut current_impl = None::<String>;

    for line in ast_source.lines() {
        let trimmed = line.trim();
        if !line.chars().next().is_some_and(char::is_whitespace) {
            current_impl = trimmed.strip_prefix("impl ").and_then(|header| {
                if header.contains(" for ") {
                    None
                } else {
                    first_identifier(header).map(str::to_owned)
                }
            });
            continue;
        }

        let Some(parent) = current_impl.as_ref() else {
            continue;
        };
        let method = ["pub(crate) fn ", "pub fn ", "pub const fn ", "fn "]
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix))
            .and_then(first_identifier);
        if let Some(method) = method {
            methods
                .entry(parent.clone())
                .or_default()
                .insert(method.to_owned());
        }
    }
    methods
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
    for node in nodes {
        for accessor in &node.accessors {
            let syntax_kind = match accessor.selector.as_ref() {
                Some(ChildSelector::BeforeToken { syntax_kind, .. })
                | Some(ChildSelector::AfterToken { syntax_kind, .. }) => Some(syntax_kind),
                _ => None,
            };
            let mut syntax_kinds = syntax_kind.into_iter().collect::<Vec<_>>();
            match accessor.selector.as_ref() {
                Some(ChildSelector::BeforeTokens {
                    syntax_kinds: many, ..
                })
                | Some(ChildSelector::AfterTokens {
                    syntax_kinds: many, ..
                }) => {
                    syntax_kinds.extend(many);
                }
                _ => {}
            }
            for syntax_kind in syntax_kinds {
                if !variants.contains(syntax_kind) {
                    panic!(
                        "sai.ungram child role `{}::{}` uses unknown SyntaxKind `{syntax_kind}`",
                        node.ast_name, accessor.name
                    );
                }
                if nodes
                    .iter()
                    .any(|node| node.syntax_kind == syntax_kind.as_str())
                {
                    panic!(
                        "sai.ungram child role `{}::{}` uses node kind `{syntax_kind}` as a token boundary",
                        node.ast_name, accessor.name
                    );
                }
            }
        }
    }
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
        for accessor in &node.accessors {
            match accessor.kind {
                AccessorKind::Child => {
                    let selector = accessor
                        .selector
                        .as_ref()
                        .expect("child roles have a selector");
                    let accessor_expression = match selector {
                        ChildSelector::Position(index) => {
                            format!("direct_child_at(self.syntax(), {index})")
                        }
                        ChildSelector::BeforeToken { syntax_kind, index } => format!(
                            "direct_child_before(self.syntax(), SyntaxKind::{syntax_kind}, {index})"
                        ),
                        ChildSelector::AfterToken { syntax_kind, index } => format!(
                            "direct_child_after(self.syntax(), SyntaxKind::{syntax_kind}, {index})"
                        ),
                        ChildSelector::BeforeTokens {
                            syntax_kinds,
                            index,
                        } => format!(
                            "direct_child_before_any(self.syntax(), &[{}], {index})",
                            syntax_kinds
                                .iter()
                                .map(|syntax_kind| format!("SyntaxKind::{syntax_kind}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                        ChildSelector::AfterTokens {
                            syntax_kinds,
                            index,
                        } => format!(
                            "direct_child_after_any(self.syntax(), &[{}], {index})",
                            syntax_kinds
                                .iter()
                                .map(|syntax_kind| format!("SyntaxKind::{syntax_kind}"))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ),
                    };
                    writeln!(
                        generated,
                        "impl {} {{ pub fn {}(&self) -> Option<{}> {{ {} }} }}",
                        node.ast_name, accessor.name, accessor.ty, accessor_expression
                    )
                }
                AccessorKind::Children => writeln!(
                    generated,
                    "impl {} {{ pub fn {}(&self) -> impl Iterator<Item = {}> + '_ {{ self.syntax().children().filter_map({}::cast) }} }}",
                    node.ast_name, accessor.name, accessor.ty, accessor.ty
                ),
                AccessorKind::ChildrenBefore => {
                    let Some(ChildSelector::BeforeToken { syntax_kind, .. }) =
                        accessor.selector.as_ref()
                    else {
                        panic!("children_before roles have a token boundary");
                    };
                    writeln!(
                        generated,
                        "impl {} {{ pub fn {}(&self) -> impl Iterator<Item = {}> + '_ {{ direct_children_before(self.syntax(), SyntaxKind::{syntax_kind}) }} }}",
                        node.ast_name, accessor.name, accessor.ty
                    )
                }
                AccessorKind::Token => writeln!(
                    generated,
                    "impl {} {{ pub fn {}(&self) -> Option<{}> {{ direct_token(self.syntax()) }} }}",
                    node.ast_name, accessor.name, accessor.ty
                ),
            }
            .expect("write to generated source");
        }
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
    use super::{
        ExistingAstTypes, Node, existing_ast_types, generate_nodes, parse_nodes,
        validate_node_kinds,
    };
    use std::collections::BTreeSet;

    fn node(ast_name: &str, syntax_kind: &str) -> Node {
        Node {
            ast_name: ast_name.to_owned(),
            syntax_kind: syntax_kind.to_owned(),
            accessors: Vec::new(),
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
        let existing = existing_ast_types(ast_source);
        assert!(existing.names.contains("Cow"));
        assert!(existing.names.contains("HashMap"));
        assert!(existing.names.contains("SyntaxElement"));
        assert!(existing.names.contains("SyntaxKind"));
        assert!(existing.names.contains("Expr"));
        assert!(existing.names.contains("Name"));
        assert!(existing.names.contains("Type"));
        assert!(existing.names.contains("Option"));
        assert!(existing.tokens.contains("Name"));
        assert!(existing.unions.contains("Type"));

        let result = std::panic::catch_unwind(|| {
            parse_nodes("node Self SOURCE_FILE", &ExistingAstTypes::default())
        });
        assert!(result.is_err());
    }

    #[test]
    fn parse_nodes_rejects_existing_ast_type_names() {
        let existing = ExistingAstTypes {
            names: BTreeSet::from(["Expr".to_owned()]),
            ..ExistingAstTypes::default()
        };
        let result = std::panic::catch_unwind(|| parse_nodes("node Expr SOURCE_FILE", &existing));
        assert!(result.is_err());
    }

    #[test]
    fn parse_nodes_rejects_overlapping_roles_through_ast_unions() {
        let existing = existing_ast_types(concat!(
            "pub enum Expr {\n",
            "    /// A brace `}` and parenthesis ( in this comment are inert.\n",
            "    #[doc = \"a string brace } and parenthesis ( are inert\"]\n",
            "    #[doc = r#\"a raw string brace } and parenthesis ( are inert\"#]\n",
            "    /* A block-comment brace } and parenthesis ( /* nested */ are inert. */\n",
            "    Path(\n",
            "        PathExpr,\n",
            "    ),\n",
            "    Literal(LiteralExpr),\n",
            "}\n",
            "impl AstNode for Expr {}\n",
            "pub enum NestedExpr {\n",
            "    Expr(Expr),\n",
            "}\n",
            "impl AstNode for NestedExpr {}\n",
            "ast_union!(Pattern {\n",
            "    Binding(BindingPat, BINDING_PAT),\n",
            "});\n",
        ));
        assert!(existing.unions.contains("Expr"));
        assert!(existing.unions.contains("NestedExpr"));
        assert!(existing.union_members.get("Expr").is_some_and(|members| {
            members.contains("PathExpr") && members.contains("LiteralExpr")
        }));
        assert!(
            existing
                .union_members
                .get("NestedExpr")
                .is_some_and(|members| members.contains("Expr"))
        );
        assert!(
            existing
                .union_members
                .get("Pattern")
                .is_some_and(|members| members.contains("BindingPat"))
        );

        let overlapping = concat!(
            "node Parent PARENT\n",
            "node PathExpr PATH_EXPR\n",
            "node LiteralExpr LITERAL\n",
            "child_before Parent expression NestedExpr DOT 1\n",
            "child_before Parent path PathExpr DOT 0",
        );
        assert!(std::panic::catch_unwind(|| parse_nodes(overlapping, &existing)).is_err());

        let non_overlapping = concat!(
            "node Parent PARENT\n",
            "node PathExpr PATH_EXPR\n",
            "node LiteralExpr LITERAL\n",
            "child_before Parent first_expression Expr DOT 0\n",
            "child_before Parent second_path PathExpr DOT 1",
        );
        assert_eq!(parse_nodes(non_overlapping, &existing).len(), 3);
    }

    #[test]
    fn existing_ast_types_require_an_exact_ast_node_impl_name() {
        let existing = existing_ast_types(concat!(
            "pub enum Expr {}\n",
            "impl AstNode for ExprExtended {}\n",
        ));
        assert!(!existing.unions.contains("Expr"));
        let result = std::panic::catch_unwind(|| {
            parse_nodes(
                "node Parent PARENT\nnode Child CHILD\nchild Parent value Expr 0",
                &existing,
            )
        });
        assert!(result.is_err());
    }

    #[test]
    fn parse_nodes_validates_and_attaches_child_roles() {
        let existing = ExistingAstTypes {
            unions: BTreeSet::from(["Type".to_owned()]),
            tokens: BTreeSet::from(["StringLit".to_owned()]),
            ..ExistingAstTypes::default()
        };
        let nodes = parse_nodes(
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "node Other OTHER\n",
                "node Extra EXTRA\n",
                "children Parent children Child\n",
                "child Parent first Child 0\n",
                "child Parent second Child 1\n",
                "child Parent child Type 0\n",
                "child_before Parent before Extra COMMA 0\n",
                "child_after Parent after Extra COMMA 0\n",
                "child_before_any Parent before_any Other DOT2|DOT2EQ 0\n",
                "child_after_any Parent after_any Other DOT2|DOT2EQ 0\n",
                "children_before Parent preceding Child COMMA\n",
                "token Parent token StringLit",
            ),
            &existing,
        );

        let generated = generate_nodes(&nodes);
        assert!(generated.contains("pub fn children(&self) -> impl Iterator<Item = Child>"));
        assert!(generated.contains("pub fn first(&self) -> Option<Child>"));
        assert!(generated.contains("pub fn second(&self) -> Option<Child>"));
        assert!(generated.contains("pub fn child(&self) -> Option<Type>"));
        assert!(generated.contains("direct_child_at(self.syntax(), 0)"));
        assert!(generated.contains("direct_child_at(self.syntax(), 1)"));
        assert!(generated.contains("direct_child_before(self.syntax(), SyntaxKind::COMMA, 0)"));
        assert!(generated.contains("direct_child_after(self.syntax(), SyntaxKind::COMMA, 0)"));
        assert!(generated.contains(
            "direct_child_before_any(self.syntax(), &[SyntaxKind::DOT2, SyntaxKind::DOT2EQ], 0)"
        ));
        assert!(generated.contains(
            "direct_child_after_any(self.syntax(), &[SyntaxKind::DOT2, SyntaxKind::DOT2EQ], 0)"
        ));
        assert!(generated.contains("direct_children_before(self.syntax(), SyntaxKind::COMMA)"));
        assert!(generated.contains("pub fn token(&self) -> Option<StringLit>"));
    }

    #[test]
    fn child_boundary_roles_validate_syntax_token_kinds() {
        let nodes = parse_nodes(
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "child_before Parent before Child COMMA 0",
            ),
            &ExistingAstTypes::default(),
        );
        validate_node_kinds(&nodes, "pub enum SyntaxKind { COMMA, EOF, PARENT, CHILD }");

        let nodes = parse_nodes(
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "child_before_any Parent before Child COMMA|DOT 0",
            ),
            &ExistingAstTypes::default(),
        );
        validate_node_kinds(
            &nodes,
            "pub enum SyntaxKind { COMMA, DOT, EOF, PARENT, CHILD }",
        );

        let nodes = parse_nodes(
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "children_before Parent before Child COMMA",
            ),
            &ExistingAstTypes::default(),
        );
        validate_node_kinds(&nodes, "pub enum SyntaxKind { COMMA, EOF, PARENT, CHILD }");

        let nodes = parse_nodes(
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "child_after Parent after Child MISSING_TOKEN 0",
            ),
            &ExistingAstTypes::default(),
        );
        assert!(
            std::panic::catch_unwind(|| {
                validate_node_kinds(&nodes, "pub enum SyntaxKind { COMMA, EOF, PARENT, CHILD }")
            })
            .is_err()
        );

        for boundary in ["COMMA|MISSING_TOKEN", "COMMA|CHILD"] {
            let nodes = parse_nodes(
                &format!(
                    "node Parent PARENT\nnode Child CHILD\nchild_after_any Parent after Child {boundary} 0"
                ),
                &ExistingAstTypes::default(),
            );
            assert!(
                std::panic::catch_unwind(|| {
                    validate_node_kinds(&nodes, "pub enum SyntaxKind { COMMA, EOF, PARENT, CHILD }")
                })
                .is_err()
            );
        }

        let nodes = parse_nodes(
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "children_before Parent before Child CHILD",
            ),
            &ExistingAstTypes::default(),
        );
        assert!(
            std::panic::catch_unwind(|| {
                validate_node_kinds(&nodes, "pub enum SyntaxKind { COMMA, EOF, PARENT, CHILD }")
            })
            .is_err()
        );

        let nodes = parse_nodes(
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "child_before Parent before Child CHILD 0",
            ),
            &ExistingAstTypes::default(),
        );
        assert!(
            std::panic::catch_unwind(|| {
                validate_node_kinds(&nodes, "pub enum SyntaxKind { COMMA, EOF, PARENT, CHILD }")
            })
            .is_err()
        );
    }

    #[test]
    fn parse_nodes_rejects_duplicate_and_unknown_child_roles() {
        let existing = ExistingAstTypes::default();
        let duplicate = concat!(
            "node Parent PARENT\n",
            "node Child CHILD\n",
            "child Parent item Child 0\n",
            "children Parent item Child",
        );
        assert!(std::panic::catch_unwind(|| parse_nodes(duplicate, &existing)).is_err());

        let unknown_parent = "node Child CHILD\nchild Missing item Child 0";
        assert!(std::panic::catch_unwind(|| parse_nodes(unknown_parent, &existing)).is_err());

        let unknown_type = "node Parent PARENT\nchild Parent item Missing 0";
        assert!(std::panic::catch_unwind(|| parse_nodes(unknown_type, &existing)).is_err());

        for boundary in ["", "|COMMA", "COMMA|", "COMMA||DOT", "COMMA|COMMA"] {
            let malformed = format!(
                "node Parent PARENT\nnode Child CHILD\nchild_before_any Parent item Child {boundary} 0"
            );
            assert!(
                std::panic::catch_unwind(|| parse_nodes(&malformed, &existing)).is_err(),
                "invalid token-boundary list should be rejected: {boundary:?}"
            );
        }

        let overlapping = [
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "child_before_any Parent first Child COMMA 0\n",
                "child_before_any Parent second Child DOT 0",
            ),
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "child_after_any Parent first Child COMMA 1\n",
                "child_after_any Parent second Child DOT 0",
            ),
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "child Parent first Child 0\n",
                "child_before Parent second Child COMMA 0",
            ),
            concat!(
                "node Parent PARENT\n",
                "node Child CHILD\n",
                "child_before Parent first Child COMMA 0\n",
                "child_after Parent second Child DOT 0",
            ),
        ];
        for declarations in overlapping {
            assert!(
                std::panic::catch_unwind(|| parse_nodes(declarations, &existing)).is_err(),
                "overlapping child roles should be rejected:\n{declarations}"
            );
        }
    }

    #[test]
    fn parse_nodes_rejects_two_roles_selecting_the_same_child_position() {
        let grammar = concat!(
            "node Parent PARENT\n",
            "node Child CHILD\n",
            "child Parent lhs Child 0\n",
            "child Parent rhs Child 0",
        );
        assert!(
            std::panic::catch_unwind(|| parse_nodes(grammar, &ExistingAstTypes::default()))
                .is_err()
        );
    }

    #[test]
    fn parse_nodes_rejects_trait_and_hand_written_accessor_collisions() {
        let existing = existing_ast_types("impl Parent {\n    pub fn label(&self) {}\n}");
        let handwritten = "node Parent PARENT\nchild Parent label Parent 0";
        assert!(std::panic::catch_unwind(|| parse_nodes(handwritten, &existing)).is_err());

        let trait_method = "node Parent PARENT\nchild Parent syntax Parent 0";
        assert!(
            std::panic::catch_unwind(|| {
                parse_nodes(trait_method, &ExistingAstTypes::default())
            })
            .is_err()
        );
    }

    #[test]
    fn generated_wrappers_and_node_kinds_share_the_declared_inventory() {
        let existing = ExistingAstTypes::default();
        let nodes = parse_nodes("node Error ERROR\nnode SourceFile SOURCE_FILE", &existing);
        let generated = generate_nodes(&nodes);
        assert!(generated.contains("ast_node!(Error, ERROR);"));
        assert!(generated.contains("ast_node!(SourceFile, SOURCE_FILE);"));
        assert!(generated.contains("SyntaxKind::ERROR,"));
        assert!(generated.contains("SyntaxKind::SOURCE_FILE,"));
    }
}
