#![forbid(unsafe_code)]

use std::sync::Arc;

use sai_fmt::parse_source;
use sai_package::lexer::{PackageTokenKind, lex_tokens};
use sai_package::manifest::{PackageRole, parse_for_role};
use sai_syntax::{
    LexOutput, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken, TokenFlags, TriviaKind, lex,
};
use sai_vfs::{FileId, SnapshotFile, SourceDecodeError, SourceFile, TextRange, WorkspaceSnapshot};

const SOURCE_PATH: &str = "src/lib.sai";
const MAX_LAYOUT_VARIANTS: usize = 16;

/// Failure while deriving formatter layout variants from a fixture.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LayoutVariantError {
    /// The test path must be a nonempty repository-relative path using `/`.
    InvalidTestPath,
    /// The source bytes could not be decoded or indexed by the reference lexer.
    InvalidSource,
    /// The source was not accepted by the reference formatter parser.
    RefusedSource,
    /// A generated candidate was refused or changed significant tokens.
    InvalidVariant,
    /// The requested variant count exceeds the conformance contract.
    TooManyVariants { requested: usize, maximum: usize },
}

impl std::fmt::Display for LayoutVariantError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidTestPath => {
                formatter.write_str("test path must be a normalized repository-relative path")
            }
            Self::InvalidSource => formatter.write_str("source could not be decoded or lexed"),
            Self::RefusedSource => formatter.write_str("source is refused by the reference parser"),
            Self::InvalidVariant => formatter
                .write_str("generated layout variant was refused or changed significant tokens"),
            Self::TooManyVariants { requested, maximum } => write!(
                formatter,
                "requested {requested} variants, maximum is {maximum}"
            ),
        }
    }
}

impl std::error::Error for LayoutVariantError {}

/// Derive deterministic, meaning-preserving source layout variants.
///
/// Candidate positions come from token boundaries produced by the reference
/// lexer. Parser-skipped signature positions are derived from the reference
/// syntax tree. A candidate is accepted only when the reference formatter
/// parser accepts it and its syntax tree, with `NEWLINE` and EOF tokens
/// removed, is identical to the input tree. Comments and blank-line gaps are
/// never edited. The pseudo-random generator is seeded with BLAKE3 of the
/// normalized, repository-relative `test_path`. The result always contains
/// `count` entries; sources without useful layout gaps are represented by
/// repeated input bytes.
pub fn generate_layout_variants(
    test_path: &str,
    input: &[u8],
    count: usize,
) -> Result<Vec<Vec<u8>>, LayoutVariantError> {
    if !is_normalized_test_path(test_path) {
        return Err(LayoutVariantError::InvalidTestPath);
    }
    if count > MAX_LAYOUT_VARIANTS {
        return Err(LayoutVariantError::TooManyVariants {
            requested: count,
            maximum: MAX_LAYOUT_VARIANTS,
        });
    }
    if count == 0 {
        return Ok(Vec::new());
    }

    let parsed = parse_source(input).map_err(|_| LayoutVariantError::RefusedSource)?;
    let baseline = syntax_signature(parsed.root());
    let (source, lexed) = lex_fixture(input)?;
    let gaps = candidate_gaps(parsed.root(), input, &source, &lexed);
    let mut random = Xoshiro256::from_seed(*blake3::hash(test_path.as_bytes()).as_bytes());
    let mut variants = Vec::with_capacity(count);

    for _ in 0..count {
        let mut changes = Vec::new();
        for gap in &gaps {
            if random.next_u64() % 100 >= 45 {
                continue;
            }
            let options = [gap.safe_spaces, gap.safe_newline]
                .into_iter()
                .filter(|safe| *safe)
                .count();
            if options == 0 {
                continue;
            }
            let selected = random.next_u64() as usize % options;
            let use_newline = if gap.safe_spaces && gap.safe_newline {
                selected == 1
            } else {
                gap.safe_newline
            };
            let replacement = if use_newline {
                let indent = (random.next_u64() % 9) as usize;
                let mut replacement = Vec::with_capacity(1 + indent);
                replacement.push(b'\n');
                replacement.resize(1 + indent, b' ');
                replacement
            } else {
                let spaces = 1 + (random.next_u64() % 8) as usize;
                vec![b' '; spaces]
            };
            if replacement.as_slice() != gap.original {
                changes.push((gap.start, gap.end, replacement));
            }
        }

        let candidate = if changes.is_empty() {
            input.to_vec()
        } else {
            apply_changes(input, changes)
        };
        let accepted = parse_source(&candidate)
            .ok()
            .is_some_and(|parsed| syntax_signature(parsed.root()) == baseline);
        variants.push(if accepted { candidate } else { input.to_vec() });
    }

    Ok(variants)
}

/// Derive deterministic, meaning-preserving `sai.package` layout variants.
///
/// The reference package lexer supplies token boundaries. Horizontal
/// whitespace is varied throughout; line breaks are added or removed only
/// before package continuation words and next to braces, where the package
/// grammar guarantees that they do not alter the parsed contract. Comments
/// and blank-line gaps remain byte-for-byte intact. Candidates must retain
/// the significant token sequence and be accepted by the package parser.
pub fn generate_package_layout_variants(
    test_path: &str,
    input: &[u8],
    count: usize,
) -> Result<Vec<Vec<u8>>, LayoutVariantError> {
    if !is_normalized_test_path(test_path) {
        return Err(LayoutVariantError::InvalidTestPath);
    }
    if count > MAX_LAYOUT_VARIANTS {
        return Err(LayoutVariantError::TooManyVariants {
            requested: count,
            maximum: MAX_LAYOUT_VARIANTS,
        });
    }
    if count == 0 {
        return Ok(Vec::new());
    }

    let baseline = package_signature(input).ok_or(LayoutVariantError::RefusedSource)?;
    let lexed = lex_tokens(input);
    let tokens = lexed
        .tokens
        .iter()
        .filter(|token| {
            token.kind != PackageTokenKind::Eof && token.kind != PackageTokenKind::Newline
        })
        .collect::<Vec<_>>();
    let gaps = tokens
        .windows(2)
        .filter_map(|pair| {
            let left = pair[0];
            let right = pair[1];
            let original = input.get(left.span.end..right.span.start)?;
            let horizontal_only = original.iter().all(|byte| matches!(byte, b' ' | b'\t'));
            let single_line_break = original.iter().filter(|byte| **byte == b'\n').count() == 1
                && original
                    .iter()
                    .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'));
            let right_text = input.get(right.span.clone())?;
            let safe_newline = right_text == b"from"
                || right_text == b"dir"
                || right_text == b"only"
                || right_text == b"grant"
                || left.kind
                    == PackageTokenKind::Punctuator(
                        sai_package::lexer::PackagePunctuator::LeftBrace,
                    )
                || right.kind
                    == PackageTokenKind::Punctuator(
                        sai_package::lexer::PackagePunctuator::RightBrace,
                    );
            (horizontal_only || (single_line_break && safe_newline)).then_some((
                left.span.end,
                right.span.start,
                safe_newline,
            ))
        })
        .collect::<Vec<_>>();
    let mut random = Xoshiro256::from_seed(*blake3::hash(test_path.as_bytes()).as_bytes());
    let mut variants = Vec::with_capacity(count);

    for _ in 0..count {
        let mut changes = Vec::new();
        for (start, end, safe_newline) in &gaps {
            if random.next_u64() % 100 >= 45 {
                continue;
            }
            let choose_newline = *safe_newline && random.next_u64() % 2 == 0;
            let replacement = if choose_newline {
                let indent = (random.next_u64() % 9) as usize;
                let mut replacement = Vec::with_capacity(1 + indent);
                replacement.push(b'\n');
                replacement.resize(1 + indent, b' ');
                replacement
            } else {
                let spaces = 1 + (random.next_u64() % 8) as usize;
                vec![b' '; spaces]
            };
            if input.get(*start..*end) != Some(replacement.as_slice()) {
                changes.push((*start, *end, replacement));
            }
        }

        let candidate = if changes.is_empty() {
            input.to_vec()
        } else {
            apply_changes(input, changes)
        };
        let accepted = package_signature(&candidate).is_some_and(|signature| signature == baseline);
        if !accepted {
            return Err(LayoutVariantError::InvalidVariant);
        }
        variants.push(candidate);
    }

    Ok(variants)
}

fn package_signature(input: &[u8]) -> Option<Vec<(PackageTokenKind, Vec<u8>)>> {
    let lexed = lex_tokens(input);
    if lexed.invalid_utf8 || lexed.error_count != 0 {
        return None;
    }
    let parsed = parse_for_role(input, PackageRole::Root);
    if parsed.lexical_error_count() != 0 || parsed.refusing_error_count() != 0 {
        return None;
    }
    lexed
        .tokens
        .iter()
        .filter(|token| {
            token.kind != PackageTokenKind::Eof && token.kind != PackageTokenKind::Newline
        })
        .map(|token| {
            input
                .get(token.span.clone())
                .map(|text| (token.kind, text.to_vec()))
        })
        .collect()
}

#[derive(Clone)]
struct CandidateGap<'a> {
    start: usize,
    end: usize,
    original: &'a [u8],
    safe_spaces: bool,
    safe_newline: bool,
}

fn lex_fixture(bytes: &[u8]) -> Result<(Arc<SourceFile>, LexOutput), LayoutVariantError> {
    let file_id = FileId::new(0);
    let source = Arc::new(
        SourceFile::decode(file_id, bytes).map_err(|error| match error {
            SourceDecodeError::SourceTooLarge | SourceDecodeError::UnremovedLeadingBom => {
                LayoutVariantError::InvalidSource
            }
        })?,
    );
    let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
        Arc::clone(&source),
        SOURCE_PATH,
        None,
        None,
    )])
    .map_err(|_| LayoutVariantError::InvalidSource)?;
    let lexed = lex(&snapshot, file_id).map_err(|_| LayoutVariantError::InvalidSource)?;
    Ok((source, lexed))
}

fn candidate_gaps<'a>(
    root: &SyntaxNode,
    input: &'a [u8],
    source: &SourceFile,
    lexed: &LexOutput,
) -> Vec<CandidateGap<'a>> {
    let parser_skipped_positions = parser_skipped_positions(root);
    let significant = lexed
        .tokens
        .iter()
        .filter(|token| !matches!(token.kind, SyntaxKind::NEWLINE | SyntaxKind::EOF))
        .collect::<Vec<_>>();
    let mut gaps = Vec::new();
    let mut brackets = Vec::new();

    for (pair_index, pair) in significant.windows(2).enumerate() {
        let left = &pair[0];
        let right = &pair[1];
        update_bracket_stack(&mut brackets, left.kind);
        let Some(decoded_range) = TextRange::new(left.range.end(), right.range.start()) else {
            continue;
        };
        let Some(original_range) = source.to_original_range(decoded_range) else {
            continue;
        };
        let start = original_range.start().to_usize();
        let end = original_range.end().to_usize();
        let Some(original) = input.get(start..end) else {
            continue;
        };
        let decoded_start = decoded_range.start().to_usize();
        let decoded_end = decoded_range.end().to_usize();
        let Some(decoded_gap) = source.text().get(decoded_start..decoded_end) else {
            continue;
        };
        if !decoded_gap.chars().all(char::is_whitespace)
            || contains_comment(lexed, decoded_range)
            || decoded_gap
                .chars()
                .filter(|character| *character == '\n')
                .count()
                > 1
        {
            continue;
        }

        let has_line_break = decoded_gap.contains('\n');
        let parser_skips_s4 = parser_skipped_positions.contains(&right.range.start().to_usize());
        let safe_without_newline = newline_is_lexically_safe(
            left,
            right,
            &significant,
            pair_index,
            &brackets,
            parser_skips_s4,
        );
        let safe_spaces = !has_line_break || safe_without_newline;
        let safe_newline = has_line_break || safe_without_newline;
        if safe_spaces || safe_newline {
            gaps.push(CandidateGap {
                start,
                end,
                original,
                safe_spaces,
                safe_newline,
            });
        }
    }
    gaps
}

fn parser_skipped_positions(root: &SyntaxNode) -> std::collections::HashSet<usize> {
    let mut positions = std::collections::HashSet::new();
    let mut work = vec![root.clone()];
    while let Some(node) = work.pop() {
        for element in node.children_with_tokens() {
            match element {
                SyntaxElement::Node(child) => work.push(child),
                SyntaxElement::Token(token) => {
                    if parser_skips_s4_before(&token) {
                        positions.insert(token.text_range().start().to_usize());
                    }
                }
            }
        }
    }
    positions
}

fn parser_skips_s4_before(token: &SyntaxToken) -> bool {
    match token.kind() {
        SyntaxKind::THIN_ARROW => {
            has_direct_parent(token, SyntaxKind::RET_TYPE, SyntaxKind::FN_DECL)
        }
        SyntaxKind::L_BRACE => [
            (SyntaxKind::BLOCK, SyntaxKind::FN_DECL),
            (SyntaxKind::FIELD_LIST, SyntaxKind::STRUCT_DEF),
            (SyntaxKind::FIELD_LIST, SyntaxKind::RESOURCE_DECL),
            (SyntaxKind::VARIANT_LIST, SyntaxKind::ENUM_DECL),
            (SyntaxKind::INVARIANT_EXPR_LIST, SyntaxKind::INVARIANT_DECL),
            (SyntaxKind::MATCH_ARM_LIST, SyntaxKind::MATCH_EXPR),
            (SyntaxKind::ITEM_LIST, SyntaxKind::TRAIT_DECL),
            (SyntaxKind::ITEM_LIST, SyntaxKind::IMPL_DECL),
            (SyntaxKind::ITEM_LIST, SyntaxKind::INTERFACE_DECL),
            (SyntaxKind::ITEM_LIST, SyntaxKind::IFACE_RESOURCE),
            (SyntaxKind::ITEM_LIST, SyntaxKind::WORLD_DECL),
            (SyntaxKind::ITEM_LIST, SyntaxKind::EXTERN_BLOCK),
            (SyntaxKind::BLOCK, SyntaxKind::TEST_DECL),
            (SyntaxKind::BLOCK, SyntaxKind::PROPERTY_DECL),
        ]
        .into_iter()
        .any(|(list, declaration)| has_direct_parent(token, list, declaration)),
        SyntaxKind::WHERE_KW | SyntaxKind::LIDENT => [
            (SyntaxKind::WHERE_CLAUSE, SyntaxKind::FN_DECL),
            (SyntaxKind::WHERE_CLAUSE, SyntaxKind::STRUCT_DEF),
            (SyntaxKind::WHERE_CLAUSE, SyntaxKind::ENUM_DECL),
            (SyntaxKind::WHERE_CLAUSE, SyntaxKind::TRAIT_DECL),
            (SyntaxKind::WHERE_CLAUSE, SyntaxKind::IMPL_DECL),
            (SyntaxKind::REFINEMENT_CLAUSE, SyntaxKind::ALIAS_DEF),
            (SyntaxKind::REFINEMENT_CLAUSE, SyntaxKind::DISTINCT_DECL),
            (SyntaxKind::COVERS_CLAUSE, SyntaxKind::EFFECT_DECL),
            (SyntaxKind::BORROWS_CLAUSE, SyntaxKind::FN_DECL),
            (SyntaxKind::EFFECTS_CLAUSE, SyntaxKind::FN_DECL),
            (SyntaxKind::REQUIRES_CLAUSE, SyntaxKind::FN_DECL),
            (SyntaxKind::ENSURES_CLAUSE, SyntaxKind::FN_DECL),
        ]
        .into_iter()
        .any(|(clause, declaration)| has_direct_parent(token, clause, declaration)),
        SyntaxKind::GT => token.parent().ancestors().any(|node| {
            matches!(
                node.kind(),
                SyntaxKind::GENERIC_PARAM_LIST | SyntaxKind::GENERIC_ARG_LIST
            ) && node
                .last_token()
                .is_some_and(|last| token.is_same_token(&last))
        }),
        // The grammar skips newlines before closing braces in blocks and lists.
        SyntaxKind::R_BRACE => true,
        _ => false,
    }
}

fn has_direct_parent(token: &SyntaxToken, child_kind: SyntaxKind, parent_kind: SyntaxKind) -> bool {
    token.parent().ancestors().any(|node| {
        node.kind() == child_kind
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == parent_kind)
            && node
                .first_token()
                .is_some_and(|first| token.is_same_token(&first))
    })
}

fn newline_is_lexically_safe(
    left: &sai_syntax::Token,
    right: &sai_syntax::Token,
    significant: &[&sai_syntax::Token],
    pair_index: usize,
    brackets: &[SyntaxKind],
    parser_skips_s4: bool,
) -> bool {
    if parser_skips_s4 || left.kind == SyntaxKind::L_BRACE {
        return true;
    }

    let in_paren_or_square = matches!(
        brackets.last(),
        Some(SyntaxKind::L_PAREN | SyntaxKind::L_BRACKET)
    );
    if in_paren_or_square || !left.flags.contains(TokenFlags::IN_E) {
        return true;
    }

    match right.kind {
        SyntaxKind::AND_KW | SyntaxKind::OR_KW | SyntaxKind::ELSE_KW => true,
        SyntaxKind::DOT => significant.get(pair_index + 2).is_some_and(|next| {
            matches!(
                next.kind,
                SyntaxKind::LIDENT | SyntaxKind::AWAIT_KW | SyntaxKind::INT
            )
        }),
        _ => false,
    }
}

fn update_bracket_stack(brackets: &mut Vec<SyntaxKind>, kind: SyntaxKind) {
    match kind {
        SyntaxKind::L_PAREN
        | SyntaxKind::L_BRACKET
        | SyntaxKind::L_BRACE
        | SyntaxKind::FSTRING_INTERP_OPEN => brackets.push(kind),
        SyntaxKind::R_PAREN => pop_bracket(brackets, SyntaxKind::L_PAREN),
        SyntaxKind::R_BRACKET => pop_bracket(brackets, SyntaxKind::L_BRACKET),
        SyntaxKind::R_BRACE => pop_bracket(brackets, SyntaxKind::L_BRACE),
        SyntaxKind::FSTRING_INTERP_CLOSE => {
            if let Some(index) = brackets
                .iter()
                .rposition(|bracket| *bracket == SyntaxKind::FSTRING_INTERP_OPEN)
            {
                brackets.truncate(index);
            }
        }
        _ => {}
    }
}

fn pop_bracket(brackets: &mut Vec<SyntaxKind>, open: SyntaxKind) {
    let boundary = brackets
        .iter()
        .rposition(|bracket| *bracket == SyntaxKind::FSTRING_INTERP_OPEN)
        .map_or(0, |index| index + 1);
    if let Some(relative_index) = brackets[boundary..]
        .iter()
        .rposition(|bracket| *bracket == open)
    {
        brackets.truncate(boundary + relative_index);
    }
}

fn contains_comment(lexed: &LexOutput, range: TextRange) -> bool {
    let first = lexed
        .trivia
        .partition_point(|trivia| trivia.range.end() <= range.start());
    lexed.trivia[first..]
        .iter()
        .take_while(|trivia| trivia.range.start() < range.end())
        .any(|trivia| {
            matches!(
                trivia.kind,
                TriviaKind::LineComment
                    | TriviaKind::BlockComment
                    | TriviaKind::Shebang
                    | TriviaKind::Skipped
            )
        })
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum SignatureElement {
    NodeStart(SyntaxKind),
    NodeEnd(SyntaxKind),
    Token(SyntaxKind, String),
}

enum SignatureWork {
    Element(SyntaxElement),
    NodeEnd(SyntaxKind),
}

fn syntax_signature(root: &SyntaxNode) -> Vec<SignatureElement> {
    let mut signature = Vec::new();
    let mut work = vec![SignatureWork::Element(SyntaxElement::Node(root.clone()))];
    while let Some(entry) = work.pop() {
        match entry {
            SignatureWork::Element(SyntaxElement::Node(node)) => {
                signature.push(SignatureElement::NodeStart(node.kind()));
                work.push(SignatureWork::NodeEnd(node.kind()));
                work.extend(
                    node.children_with_tokens()
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .map(SignatureWork::Element),
                );
            }
            SignatureWork::Element(SyntaxElement::Token(token)) => {
                if !matches!(token.kind(), SyntaxKind::NEWLINE | SyntaxKind::EOF) {
                    signature.push(SignatureElement::Token(
                        token.kind(),
                        token.text().to_string(),
                    ));
                }
            }
            SignatureWork::NodeEnd(kind) => signature.push(SignatureElement::NodeEnd(kind)),
        }
    }
    signature
}

fn apply_changes(input: &[u8], changes: Vec<(usize, usize, Vec<u8>)>) -> Vec<u8> {
    // Candidate gaps are visited in source order, so changes are already sorted.
    let capacity = input.len()
        + changes
            .iter()
            .map(|(start, end, replacement)| replacement.len().saturating_sub(end - start))
            .sum::<usize>();
    let mut output = Vec::with_capacity(capacity);
    let mut cursor = 0;
    for (start, end, replacement) in changes {
        output.extend_from_slice(&input[cursor..start]);
        output.extend_from_slice(&replacement);
        cursor = end;
    }
    output.extend_from_slice(&input[cursor..]);
    output
}

fn is_normalized_test_path(path: &str) -> bool {
    !path.is_empty()
        && !path.starts_with('/')
        && !path.contains('\\')
        && !path
            .as_bytes()
            .get(..2)
            .is_some_and(|prefix| prefix[0].is_ascii_alphabetic() && prefix[1] == b':')
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

struct Xoshiro256 {
    state: [u64; 4],
}

impl Xoshiro256 {
    fn from_seed(seed: [u8; 32]) -> Self {
        let mut state = [0u64; 4];
        for (index, chunk) in seed.chunks_exact(8).enumerate() {
            state[index] = u64::from_le_bytes(chunk.try_into().expect("fixed-size seed chunk"));
        }
        if state == [0; 4] {
            state[0] = 1;
        }
        Self { state }
    }

    fn next_u64(&mut self) -> u64 {
        let result = self.state[1].wrapping_mul(5).rotate_left(7).wrapping_mul(9);
        let temporary = self.state[1] << 17;
        self.state[2] ^= self.state[0];
        self.state[3] ^= self.state[1];
        self.state[1] ^= self.state[2];
        self.state[0] ^= self.state[3];
        self.state[2] ^= temporary;
        self.state[3] = self.state[3].rotate_left(45);
        result
    }
}

#[cfg(test)]
mod tests {
    use sai_fmt::{format_package, format_source};

    use super::{
        LayoutVariantError, MAX_LAYOUT_VARIANTS, generate_layout_variants,
        generate_package_layout_variants,
    };

    const TEST_PATH: &str = "conformance/impl-14-formatter/function-spacing.sai";
    const PACKAGE_TEST_PATH: &str = "conformance/impl-14-formatter/package-manifest.t";
    const PACKAGE_INPUT: &[u8] = include_bytes!(
        "../../../tests/conformance/impl-14-formatter/package-manifest.t/sai.package"
    );

    fn fixture() -> (Vec<u8>, Vec<u8>) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        (
            std::fs::read(root.join("tests/conformance/impl-14-formatter/function-spacing.sai"))
                .expect("read formatter input fixture"),
            std::fs::read(
                root.join("tests/conformance/impl-14-formatter/function-spacing.expected.sai"),
            )
            .expect("read formatter expected output"),
        )
    }

    fn has_newline_between(source: &[u8], left: &[u8], right: &[u8]) -> bool {
        source
            .windows(left.len())
            .enumerate()
            .filter(|(_, window)| *window == left)
            .any(|(index, _)| {
                let mut cursor = index + left.len();
                let mut saw_newline = false;
                while let Some(byte) = source.get(cursor) {
                    match byte {
                        b'\n' => saw_newline = true,
                        b' ' => {}
                        _ => return saw_newline && source[cursor..].starts_with(right),
                    }
                    cursor += 1;
                }
                false
            })
    }

    #[test]
    fn generates_sixteen_deterministic_safe_layouts() {
        let (input, expected) = fixture();
        let variants =
            generate_layout_variants(TEST_PATH, &input, 16).expect("generate 16 variants");
        let repeated = generate_layout_variants(TEST_PATH, &input, 16).expect("repeat generation");

        assert_eq!(variants, repeated);
        assert_eq!(variants.len(), 16);
        assert!(variants.iter().any(|variant| variant != &input));
        assert!(
            variants
                .iter()
                .any(|variant| has_newline_between(variant, b")", b"->")),
            "no layout broke before the function return arrow"
        );
        assert!(
            variants
                .iter()
                .any(|variant| has_newline_between(variant, b"Bool", b"{")),
            "no layout broke before the function body"
        );
        for variant in variants {
            assert!(
                variant.starts_with(
                    b"//@ kind: fmt\n//@ stage: 0\n//@ spec: impl/14 \xc2\xa714.4.2\n"
                )
            );
            assert_eq!(
                format_source(&variant)
                    .expect("format layout variant")
                    .as_bytes(),
                expected,
            );
        }
    }

    #[test]
    fn generates_sixteen_deterministic_package_layouts() {
        let variants = generate_package_layout_variants(PACKAGE_TEST_PATH, PACKAGE_INPUT, 16)
            .expect("generate package layout variants");
        let repeated = generate_package_layout_variants(PACKAGE_TEST_PATH, PACKAGE_INPUT, 16)
            .expect("repeat package variant generation");
        let expected = format_package(PACKAGE_INPUT)
            .expect("format package fixture")
            .into_bytes();

        assert_eq!(variants, repeated);
        assert_eq!(variants.len(), 16);
        assert_eq!(
            variants
                .iter()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            16,
            "tracked package fixture did not produce sixteen distinct layouts"
        );
        assert!(
            variants.iter().any(|variant| variant != PACKAGE_INPUT),
            "no package layout variant changed a gap"
        );
        assert!(
            variants.iter().any(|variant| {
                variant.iter().filter(|byte| **byte == b'\n').count()
                    != PACKAGE_INPUT.iter().filter(|byte| **byte == b'\n').count()
            }),
            "no package layout variant added or removed a safe line break"
        );
        for variant in variants {
            assert_eq!(
                format_package(&variant)
                    .expect("format package layout variant")
                    .as_bytes(),
                expected
            );
        }
    }

    #[test]
    fn rejects_absolute_and_parent_relative_test_paths() {
        let (input, _) = fixture();
        for path in [
            "/tmp/test.sai",
            "C:/temp/test.sai",
            "C:temp/test.sai",
            "../test.sai",
        ] {
            assert_eq!(
                generate_layout_variants(path, &input, 1),
                Err(LayoutVariantError::InvalidTestPath),
                "accepted invalid fixture path {path}"
            );
        }
    }

    #[test]
    fn rejects_invalid_package_layout_inputs_and_excessive_counts() {
        assert_eq!(
            generate_package_layout_variants(PACKAGE_TEST_PATH, b"package {", 1),
            Err(LayoutVariantError::RefusedSource)
        );
        assert_eq!(
            generate_package_layout_variants(PACKAGE_TEST_PATH, PACKAGE_INPUT, 17),
            Err(LayoutVariantError::TooManyVariants {
                requested: 17,
                maximum: MAX_LAYOUT_VARIANTS,
            })
        );
        assert_eq!(
            generate_package_layout_variants("../package.sai", PACKAGE_INPUT, 1),
            Err(LayoutVariantError::InvalidTestPath)
        );
    }

    #[test]
    fn rejects_variant_counts_above_the_f6_contract() {
        let (input, _) = fixture();
        assert_eq!(
            generate_layout_variants(TEST_PATH, &input, MAX_LAYOUT_VARIANTS + 1),
            Err(LayoutVariantError::TooManyVariants {
                requested: MAX_LAYOUT_VARIANTS + 1,
                maximum: MAX_LAYOUT_VARIANTS,
            })
        );
    }

    #[test]
    fn comment_only_source_still_gets_sixteen_layout_entries() {
        let input =
            b"//@ kind: fmt\n//@ stage: 0\n//@ spec: impl/14 \xc2\xa714.4.2\n// only comments\n";
        let variants = generate_layout_variants(TEST_PATH, input, 16)
            .expect("comment-only source remains a valid layout input");

        assert_eq!(variants.len(), 16);
        assert!(variants.iter().all(|variant| variant == input));
    }

    #[test]
    fn preserves_comments_and_blank_lines_while_varying_layout() {
        let input = b"//@ kind: fmt\n//@ stage: 0\n//@ spec: impl/14 \xc2\xa714.4.2\npub fn value()->Bool{\n// keep this comment\n\ntrue\n}\n";
        let baseline = format_source(input).expect("format original source");
        let variants = generate_layout_variants(TEST_PATH, input, 16)
            .expect("generate source layout variants");

        assert_eq!(variants.len(), 16);
        assert!(variants.iter().any(|variant| variant != input));
        for variant in variants {
            assert_eq!(
                variant
                    .windows(b"// keep this comment".len())
                    .filter(|window| *window == b"// keep this comment")
                    .count(),
                1
            );
            assert_eq!(
                variant
                    .windows(2)
                    .filter(|window| *window == b"\n\n")
                    .count(),
                input.windows(2).filter(|window| *window == b"\n\n").count(),
                "layout mutation changed the blank-line count"
            );
            assert_eq!(
                format_source(&variant).expect("format generated layout"),
                baseline,
                "generated layout changed formatter output"
            );
        }
    }

    #[test]
    fn nested_if_blocks_do_not_disable_other_layout_variants() {
        let mut input =
            b"//@ kind: fmt\n//@ stage: 0\n//@ spec: impl/14 \xc2\xa714.4.2\npub fn value()->Bool{\n"
                .to_vec();
        for _ in 0..16 {
            input.extend_from_slice(b"if true{true}\n");
        }
        input.extend_from_slice(b"}\n");
        let variants = generate_layout_variants(
            "conformance/impl-14-formatter/nested-blocks.sai",
            &input,
            16,
        )
        .expect("generate layouts with nested if blocks");

        assert!(
            variants.iter().any(|variant| variant != &input),
            "nested blocks suppressed every useful layout change"
        );
        for variant in variants {
            assert!(
                !has_newline_between(&variant, b"true", b"{"),
                "inserted an unsafe newline before an if body"
            );
            assert_eq!(
                format_source(&variant).expect("format nested-block layout"),
                format_source(&input).expect("format nested-block source")
            );
        }
    }

    #[test]
    fn generates_s4_breaks_for_type_bodies_and_refinement_clauses() {
        let cases = [
            (
                "conformance/layout/struct-body.sai",
                b"type Person { name: Str }".as_slice(),
                b"Person".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/struct-where.sai",
                b"type Box<T> where T: Trait {}".as_slice(),
                b">".as_slice(),
                b"where".as_slice(),
            ),
            (
                "conformance/layout/struct-where-body.sai",
                b"type Box<T> where T: Trait {}".as_slice(),
                b"Trait".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/alias-refinement.sai",
                b"type Positive = I32 where self > 0".as_slice(),
                b"I32".as_slice(),
                b"where".as_slice(),
            ),
            (
                "conformance/layout/distinct-refinement.sai",
                b"distinct Positive = I32 where self > 0".as_slice(),
                b"I32".as_slice(),
                b"where".as_slice(),
            ),
            (
                "conformance/layout/enum-where.sai",
                b"enum Color where Color: Eq { Red }".as_slice(),
                b"Color".as_slice(),
                b"where".as_slice(),
            ),
            (
                "conformance/layout/enum-body.sai",
                b"enum Color { Red }".as_slice(),
                b"Color".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/resource-body.sai",
                b"resource File {}".as_slice(),
                b"File".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/invariant-body.sai",
                b"invariant Person { true }".as_slice(),
                b"Person".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/trait-where.sai",
                b"trait Show where Self: Copy {}".as_slice(),
                b"Show".as_slice(),
                b"where".as_slice(),
            ),
            (
                "conformance/layout/trait-body.sai",
                b"trait Show {}".as_slice(),
                b"Show".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/impl-where.sai",
                b"impl Copy for I32 where I32: Copy {}".as_slice(),
                b"I32".as_slice(),
                b"where".as_slice(),
            ),
            (
                "conformance/layout/impl-body.sai",
                b"impl Copy for I32 {}".as_slice(),
                b"I32".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/interface-body.sai",
                b"interface demo {}".as_slice(),
                b"demo".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/iface-resource-body.sai",
                b"interface demo { resource File {} }".as_slice(),
                b"File".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/world-body.sai",
                b"world demo {}".as_slice(),
                b"demo".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/extern-body.sai",
                b"extern \"wasm\" module \"m\" {}".as_slice(),
                b"\"m\"".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/test-body.sai",
                b"test \"works\" { true }".as_slice(),
                b"\"works\"".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/property-body.sai",
                b"property \"holds\"(x: Bool) { x }".as_slice(),
                b")".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/effect-covers.sai",
                b"effect io covers { io }".as_slice(),
                b"io".as_slice(),
                b"covers".as_slice(),
            ),
            (
                "conformance/layout/match-body.sai",
                b"fn value() -> Bool { match true{true => true, false => false} }".as_slice(),
                b"true".as_slice(),
                b"{".as_slice(),
            ),
        ];

        for (path, source, left, right) in cases {
            let variants = generate_layout_variants(path, source, 16)
                .unwrap_or_else(|error| panic!("generate layouts for {path}: {error}"));
            assert!(
                variants
                    .iter()
                    .any(|variant| has_newline_between(variant, left, right)),
                "no layout broke between {:?} and {:?} in {path}",
                String::from_utf8_lossy(left),
                String::from_utf8_lossy(right),
            );
            let expected = format_source(source).expect("format baseline source");
            for variant in variants {
                assert_eq!(
                    format_source(&variant).expect("format declaration layout"),
                    expected,
                    "layout changed formatter output for {path}"
                );
            }
        }
    }

    #[test]
    fn removes_existing_optional_s4_breaks() {
        let cases = [
            (
                "conformance/layout/remove-function-body-break.sai",
                b"fn value() -> Bool\n{true}".as_slice(),
                b"Bool".as_slice(),
                b"{".as_slice(),
            ),
            (
                "conformance/layout/remove-generic-close-break.sai",
                b"type Box<T\n> {}".as_slice(),
                b"T".as_slice(),
                b">".as_slice(),
            ),
        ];

        for (path, source, left, right) in cases {
            let variants = generate_layout_variants(path, source, 16)
                .unwrap_or_else(|error| panic!("generate layouts for {path}: {error}"));
            assert!(
                variants
                    .iter()
                    .any(|variant| !has_newline_between(variant, left, right)),
                "all layouts retained the optional break in {path}"
            );
            let expected = format_source(source).expect("format baseline source");
            for variant in variants {
                assert_eq!(
                    format_source(&variant).expect("format compacted layout"),
                    expected,
                    "layout changed formatter output for {path}"
                );
            }
        }
    }

    #[test]
    fn generates_s4_break_before_match_arm_list() {
        let source = b"fn value() -> Bool { match true{true => true, false => false} }";
        let variants = generate_layout_variants("conformance/layout/match-body.sai", source, 16)
            .expect("generate match expression layouts");

        assert!(
            variants
                .iter()
                .any(|variant| has_newline_between(variant, b"true", b"{")),
            "no layout broke before the match arm list"
        );
        let expected = format_source(source).expect("format baseline match expression");
        for variant in variants {
            assert_eq!(
                format_source(&variant).expect("format match expression layout"),
                expected
            );
        }
    }
}
