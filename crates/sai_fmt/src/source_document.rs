use std::collections::{HashMap, HashSet};
use std::ops::Range;

use sai_syntax::{
    GreenElement, GreenNode, GreenTrivia, SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken,
    TriviaKind,
    ast::{self, AstNode, AstToken, Expr, Item, LiteralToken, Stmt},
};
use sai_vfs::{TextRange, TextSize};

use crate::{Doc, GroupId, ParsedSource, printer::MAX_LINE_WIDTH};

/// Syntax shapes that the source document builder does not support yet.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SourceDocumentError {
    UnsupportedShape,
}

/// Build a document for the currently supported source-formatting subset.
///
/// The private builder remains fail-closed for unsupported syntax while
/// source-formatting coverage is being implemented.
pub(super) fn source_document(source: &ParsedSource) -> Result<Doc, SourceDocumentError> {
    let file = ast::SourceFile::cast(source.root().clone())
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let module_header_node = file.module_header();
    let has_module_header = module_header_node.is_some();
    let top_level_ranges = file
        .syntax()
        .children()
        .map(|child| child.text_range())
        .collect::<Vec<_>>();
    let items = file.items().collect::<Vec<_>>();
    if source.root().children().count() != items.len() + usize::from(has_module_header) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut comments = collect_top_level_comments(source, &top_level_ranges)?;
    let mut module_header = module_header_node
        .as_ref()
        .map(|header| module_header_document(source, header))
        .transpose()?;
    if let (Some(document), Some(range)) = (module_header.as_mut(), top_level_ranges.first()) {
        *document = decorate_top_level_document(
            std::mem::replace(document, Doc::concat([])),
            &comments.leading[0],
            &comments.trailing[0],
            source.text(),
            *range,
        )?
        .0;
    }
    let mut units = Vec::with_capacity(items.len());
    let mut use_declarations = Vec::new();
    let mut detached_use_comments = Vec::new();
    let mut expression_group_ids = ExpressionGroupIds::default();
    for (item_index, item) in items.into_iter().enumerate() {
        let top_level_index = item_index + usize::from(has_module_header);
        let range = *top_level_ranges
            .get(top_level_index)
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let leading_comments = comments
            .leading
            .get_mut(top_level_index)
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let trailing_comments = comments
            .trailing
            .get_mut(top_level_index)
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let detached_comments = comments
            .detached_before
            .get_mut(top_level_index)
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let unit = match item {
            Item::Use(declaration) => {
                detached_use_comments.append(detached_comments);
                let mut declaration = use_declaration_document(source, &declaration)?;
                declaration.document = decorate_top_level_document(
                    declaration.document,
                    leading_comments,
                    trailing_comments,
                    source.text(),
                    range,
                )?
                .0;
                use_declarations.push(declaration);
                continue;
            }
            Item::Function(function) => {
                function_document(source, &function, &mut expression_group_ids)?
            }
            Item::Impl(implementation) => {
                impl_document(source, &implementation, &mut expression_group_ids)?
            }
            Item::Trait(declaration) => {
                trait_document(source, &declaration, &mut expression_group_ids)?
            }
            Item::Distinct(declaration) => {
                distinct_decl_document(source, &declaration, &mut expression_group_ids)?
            }
            Item::Enum(declaration) => enum_decl_document(source, &declaration)?,
            Item::Resource(declaration) => resource_decl_document(source, &declaration)?,
            Item::Interface(declaration) => {
                interface_document(source, &declaration, &mut expression_group_ids)?
            }
            Item::Type(declaration) => {
                type_decl_document(source, &declaration, &mut expression_group_ids)?
            }
            Item::Const(declaration) => {
                const_decl_document(source, &declaration, &mut expression_group_ids)?
            }
            Item::Invariant(declaration) => {
                invariant_decl_document(source, &declaration, &mut expression_group_ids)?
            }
            Item::Effect(declaration) => effect_decl_document(source, &declaration)?,
            Item::World(declaration) => {
                world_decl_document(source, &declaration, &mut expression_group_ids)?
            }
            Item::ExternBlock(declaration) => {
                extern_block_document(source, &declaration, &mut expression_group_ids)?
            }
            Item::Test(declaration) => {
                test_decl_document(source, &declaration, &mut expression_group_ids)?
            }
            Item::Property(declaration) => {
                property_decl_document(source, &declaration, &mut expression_group_ids)?
            }
        };
        if !detached_comments.is_empty() {
            units.push(detached_comment_item_document(
                detached_comments,
                source.text(),
            )?);
        }
        units.push(decorate_item_document(
            unit,
            leading_comments,
            trailing_comments,
            source.text(),
        )?);
    }
    use_declarations.sort_by(|left, right| {
        left.is_public
            .cmp(&right.is_public)
            .then_with(|| left.sort_key.as_bytes().cmp(right.sort_key.as_bytes()))
    });

    let mut document = Vec::with_capacity(units.len() * 2 + use_declarations.len() * 2 + 2);
    if let Some(shebang) = &comments.shebang {
        document.push(Doc::verbatim(shebang.text.clone()));
        let first_content_start = comments
            .file_header
            .first()
            .or_else(|| {
                comments
                    .leading
                    .first()
                    .and_then(|comments| comments.first())
            })
            .map(|comment| comment.range.start)
            .or_else(|| {
                top_level_ranges
                    .first()
                    .map(|range| range.start().to_usize())
            })
            .or_else(|| comments.eof.first().map(|comment| comment.range.start));
        append_comment_gap(
            &mut document,
            source.text(),
            Some(shebang.range.end),
            first_content_start,
        )?;
    }
    if !comments.file_header.is_empty() {
        append_comment_documents(&mut document, &comments.file_header, source.text(), None)?;
        if let Some(first_range) = top_level_ranges.first() {
            append_comment_gap(
                &mut document,
                source.text(),
                comments.file_header.last().map(|comment| comment.range.end),
                Some(first_range.start().to_usize()),
            )?;
        }
    }
    if let Some(module_header) = module_header {
        document.push(module_header);
        if !detached_use_comments.is_empty() || !use_declarations.is_empty() || !units.is_empty() {
            document.extend([Doc::HardLine, Doc::HardLine]);
        }
    }
    if !detached_use_comments.is_empty() {
        append_comment_documents(&mut document, &detached_use_comments, source.text(), None)?;
        if !use_declarations.is_empty() {
            document.extend([Doc::HardLine, Doc::HardLine]);
        }
    }
    for (index, declaration) in use_declarations.iter().enumerate() {
        if index > 0 {
            document.push(Doc::HardLine);
            if use_declarations[index - 1].is_public != declaration.is_public {
                document.push(Doc::HardLine);
            }
        }
        document.push(declaration.document.clone());
    }
    if !use_declarations.is_empty() && !units.is_empty() {
        document.extend([Doc::HardLine, Doc::HardLine]);
    }
    for (index, unit) in units.iter().enumerate() {
        if index > 0 {
            let previous = &units[index - 1];
            let gap = source
                .text()
                .get(previous.end..unit.start)
                .unwrap_or_default();
            let trailing_whitespace_start = gap.trim_end_matches(char::is_whitespace).len();
            let trailing_whitespace = &gap[trailing_whitespace_start..];
            let preserve_blank_line = trailing_whitespace.matches('\n').count() > 1;
            let separate_with_blank_line =
                previous.multiline || unit.multiline || preserve_blank_line;
            document.push(Doc::HardLine);
            if separate_with_blank_line {
                document.push(Doc::HardLine);
            }
        }
        document.push(unit.document.clone());
    }
    if !comments.eof.is_empty() {
        let last_index = top_level_ranges.len().saturating_sub(1);
        let last_content_end = top_level_ranges.last().map(|range| {
            comments
                .trailing
                .get(last_index)
                .and_then(|comments| comments.last())
                .map_or_else(|| range.end().to_usize(), |comment| comment.range.end)
        });
        append_comment_gap(
            &mut document,
            source.text(),
            last_content_end,
            comments.eof.first().map(|comment| comment.range.start),
        )?;
        append_comment_documents(&mut document, &comments.eof, source.text(), None)?;
    }
    Ok(Doc::concat(document))
}

#[derive(Default)]
struct TopLevelComments {
    shebang: Option<SourceComment>,
    file_header: Vec<SourceComment>,
    detached_before: Vec<Vec<SourceComment>>,
    leading: Vec<Vec<SourceComment>>,
    trailing: Vec<Vec<SourceComment>>,
    eof: Vec<SourceComment>,
}

#[derive(Clone)]
struct SourceComment {
    range: Range<usize>,
    text: String,
    kind: TriviaKind,
    is_leading: bool,
    owner_token_kind: SyntaxKind,
    owner_token_range: Range<usize>,
}

#[derive(Default)]
struct CommentCandidates {
    shebang: Option<SourceComment>,
    eof: Vec<SourceComment>,
    leading: Vec<SourceComment>,
    trailing: Vec<SourceComment>,
}

fn collect_top_level_comments(
    source: &ParsedSource,
    top_level_ranges: &[TextRange],
) -> Result<TopLevelComments, SourceDocumentError> {
    let mut comments = TopLevelComments {
        detached_before: vec![Vec::new(); top_level_ranges.len()],
        leading: vec![Vec::new(); top_level_ranges.len()],
        trailing: vec![Vec::new(); top_level_ranges.len()],
        ..TopLevelComments::default()
    };
    let candidates = collect_comment_candidates(source.root().green(), 0, source.text().len())?;
    let supported_comment_owners = supported_comment_owners(source.root());

    comments.eof = candidates.eof;
    comments.shebang = candidates.shebang;
    for comment in candidates.leading {
        if top_level_range_containing(top_level_ranges, &comment).is_some() {
            if supported_comment_owners.contains(&comment_owner_key(&comment)) {
                continue;
            }
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let node_index =
            top_level_ranges.partition_point(|range| range.start().to_usize() < comment.range.end);
        if node_index >= top_level_ranges.len() {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        if node_index > 0 && comment.range.start < top_level_ranges[node_index - 1].end().to_usize()
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        comments.leading[node_index].push(comment);
    }
    for (node_index, group) in comments.leading.iter_mut().enumerate() {
        if group.is_empty() {
            continue;
        }
        let node_start = top_level_ranges[node_index].start().to_usize();
        let last_comment_end = group
            .last()
            .map(|comment| comment.range.end)
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let gap = source
            .text()
            .get(last_comment_end..node_start)
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let mut detached_count = if line_break_count(gap) > 1 {
            group.len()
        } else {
            0
        };
        if detached_count == 0 {
            for (comment_index, pair) in group.windows(2).enumerate() {
                let gap = source
                    .text()
                    .get(pair[0].range.end..pair[1].range.start)
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                if line_break_count(gap) > 1 {
                    detached_count = comment_index + 1;
                }
            }
        }
        if detached_count > 0 {
            let detached = group.drain(..detached_count).collect::<Vec<_>>();
            if node_index == 0 {
                comments.file_header = detached;
            } else {
                comments.detached_before[node_index] = detached;
            }
        }
    }
    for comment in candidates.trailing {
        if top_level_range_containing(top_level_ranges, &comment).is_some() {
            if supported_comment_owners.contains(&comment_owner_key(&comment)) {
                continue;
            }
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let node_index = top_level_ranges
            .partition_point(|range| range.end().to_usize() <= comment.range.start)
            .checked_sub(1)
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        if top_level_ranges
            .get(node_index + 1)
            .is_some_and(|next| comment.range.end > next.start().to_usize())
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        comments.trailing[node_index].push(comment);
    }
    if top_level_ranges.is_empty() {
        comments.file_header = std::mem::take(&mut comments.eof);
    } else if let Some(last_range) = top_level_ranges.last()
        && comments
            .eof
            .first()
            .is_some_and(|comment| comment.range.start < last_range.end().to_usize())
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(comments)
}

fn top_level_range_containing(ranges: &[TextRange], comment: &SourceComment) -> Option<usize> {
    let index = ranges
        .partition_point(|range| range.start().to_usize() <= comment.range.start)
        .checked_sub(1)?;
    ranges
        .get(index)
        .and_then(|range| (comment.range.end <= range.end().to_usize()).then_some(index))
}

fn collect_comment_candidates(
    root: &GreenNode,
    start: usize,
    expected_end: usize,
) -> Result<CommentCandidates, SourceDocumentError> {
    let mut candidates = CommentCandidates::default();
    let mut cursor = start;
    let mut pending = root.children().iter().rev().collect::<Vec<_>>();
    while let Some(element) = pending.pop() {
        match element {
            GreenElement::Node(node) => pending.extend(node.children().iter().rev()),
            GreenElement::Token(token) => {
                let token_start = cursor
                    .checked_add(
                        token
                            .leading_trivia()
                            .iter()
                            .map(|trivia| trivia.text().len())
                            .sum::<usize>(),
                    )
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                let token_end = token_start
                    .checked_add(token.text().len())
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                for trivia in token.leading_trivia() {
                    scan_source_trivia(
                        trivia,
                        true,
                        token.kind(),
                        token_start..token_end,
                        &mut cursor,
                        &mut candidates,
                    )?;
                }
                cursor = cursor
                    .checked_add(token.text().len())
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                for trivia in token.trailing_trivia() {
                    scan_source_trivia(
                        trivia,
                        false,
                        token.kind(),
                        token_start..token_end,
                        &mut cursor,
                        &mut candidates,
                    )?;
                }
            }
        }
    }
    if cursor != expected_end {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(candidates)
}

#[derive(Clone, Copy)]
enum ListCommentTarget {
    OpeningLeading,
    OpeningTrailing,
    BeforeMember(usize),
    AfterKeyword(usize),
    AfterExpressionKeyword(usize),
    AfterTypeColon(usize),
    AfterOperator(usize),
    AfterMember(usize),
    Closing,
}

type CommentOwnerKey = (bool, SyntaxKind, usize, usize);

struct BraceCommentTargets {
    interior_start: usize,
    interior_end: usize,
    targets: HashMap<CommentOwnerKey, ListCommentTarget>,
    owner_tokens: Vec<SyntaxToken>,
}

#[derive(Default)]
struct ListComments {
    opening_leading: Vec<SourceComment>,
    opening_trailing: Vec<SourceComment>,
    before: Vec<Vec<SourceComment>>,
    after_keyword: Vec<Vec<SourceComment>>,
    after_expression_keyword: Vec<Vec<SourceComment>>,
    after_type_colon: Vec<Vec<SourceComment>>,
    after_operator: Vec<Vec<SourceComment>>,
    after: Vec<Vec<SourceComment>>,
    closing: Vec<SourceComment>,
}

impl ListComments {
    fn has_comments(&self) -> bool {
        !self.opening_leading.is_empty()
            || !self.opening_trailing.is_empty()
            || !self.closing.is_empty()
            || self.before.iter().any(|comments| !comments.is_empty())
            || self
                .after_keyword
                .iter()
                .any(|comments| !comments.is_empty())
            || self
                .after_expression_keyword
                .iter()
                .any(|comments| !comments.is_empty())
            || self
                .after_type_colon
                .iter()
                .any(|comments| !comments.is_empty())
            || self
                .after_operator
                .iter()
                .any(|comments| !comments.is_empty())
            || self.after.iter().any(|comments| !comments.is_empty())
    }
}

fn comment_owner_key(comment: &SourceComment) -> CommentOwnerKey {
    (
        comment.is_leading,
        comment.owner_token_kind,
        comment.owner_token_range.start,
        comment.owner_token_range.end,
    )
}

fn supported_comment_owners(root: &SyntaxNode) -> HashSet<CommentOwnerKey> {
    let mut owners = HashSet::new();
    for node in root.descendants() {
        let member_kind = match (node.kind(), node.parent().map(|parent| parent.kind())) {
            (SyntaxKind::FIELD_LIST, Some(SyntaxKind::STRUCT_DEF | SyntaxKind::RESOURCE_DECL)) => {
                Some(SyntaxKind::FIELD_DECL)
            }
            (SyntaxKind::VARIANT_LIST, Some(SyntaxKind::ENUM_DECL)) => {
                Some(SyntaxKind::VARIANT_DECL)
            }
            (SyntaxKind::BLOCK, _) => None,
            _ => continue,
        };
        let members = node
            .children()
            .filter(|child| {
                member_kind.map_or_else(
                    || ast::Stmt::can_cast(child.kind()),
                    |kind| child.kind() == kind,
                )
            })
            .collect::<Vec<_>>();
        let targets = if member_kind.is_some() {
            list_comment_targets(&node, &members)
        } else {
            block_comment_targets(&node, &members)
        };
        if let Ok(targets) = targets {
            owners.extend(targets.targets.into_keys());
        }
    }
    for node in root.descendants().filter(|node| {
        node.kind() == SyntaxKind::IF_EXPR
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == SyntaxKind::ELSE_BRANCH)
    }) {
        if let Some(token) = node
            .children_with_tokens()
            .find_map(|element| match element {
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::IF_KW => Some(token),
                _ => None,
            })
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
    }
    for node in root
        .descendants()
        .filter(|node| matches!(node.kind(), SyntaxKind::ELSE_BRANCH | SyntaxKind::LET_ELSE))
    {
        if let Some(token) = node
            .children_with_tokens()
            .find_map(|element| match element {
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::ELSE_KW => Some(token),
                _ => None,
            })
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
    }
    for node in root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::MATCH_ARM)
    {
        let pattern = ast::MatchArm::cast(node.clone()).and_then(|arm| arm.pattern());
        if let Some(token) = pattern
            .as_ref()
            .and_then(|pattern| pattern.syntax().first_token())
        {
            owners.insert((
                true,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
        if let Some(token) = ast::MatchArm::cast(node.clone())
            .and_then(|arm| arm.pattern())
            .and_then(|pattern| pattern.syntax().last_token())
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
        if let Some(token) = node
            .children_with_tokens()
            .find_map(|element| match element {
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::FAT_ARROW => Some(token),
                _ => None,
            })
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
        if let Some(token) = ast::MatchArm::cast(node.clone())
            .and_then(|arm| arm.expression())
            .and_then(|expression| expression.syntax().last_token())
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
    }
    for node in root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::AT_PAT)
    {
        if let Some(token) = node
            .children_with_tokens()
            .find_map(|element| match element {
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::AT => Some(token),
                _ => None,
            })
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
    }
    for node in root.descendants().filter(|node| {
        matches!(
            node.kind(),
            SyntaxKind::CTOR_PAT_ARGS | SyntaxKind::TUPLE_PAT | SyntaxKind::SLICE_PAT
        )
    }) {
        let (open, close) = match node.kind() {
            SyntaxKind::CTOR_PAT_ARGS | SyntaxKind::TUPLE_PAT => {
                (SyntaxKind::L_PAREN, SyntaxKind::R_PAREN)
            }
            SyntaxKind::SLICE_PAT => (SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET),
            _ => continue,
        };
        let item_count = node.children().count();
        if item_count == 0 || !comma_delimited_tokens_match(&node, open, close, item_count) {
            continue;
        }
        if let Some(token) = node
            .children_with_tokens()
            .find_map(|element| match element {
                SyntaxElement::Token(token) if token.kind() == open => Some(token),
                _ => None,
            })
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
        for (comma_index, token) in node
            .children_with_tokens()
            .filter_map(|element| match element {
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA => Some(token),
                _ => None,
            })
            .enumerate()
        {
            if comma_index + 1 < item_count {
                owners.insert((
                    false,
                    token.kind(),
                    token.text_range().start().to_usize(),
                    token.text_range().end().to_usize(),
                ));
            }
        }
    }
    for node in root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::MATCH_ARM_LIST)
    {
        let Some(list) = ast::MatchArmList::cast(node.clone()) else {
            continue;
        };
        let arms = list.arms().collect::<Vec<_>>();
        if !match_arm_list_elements_match(&list, &arms) {
            continue;
        }
        if let Some((opening, _)) = brace_list_delimiters(&node) {
            owners.insert((
                true,
                opening.kind(),
                opening.text_range().start().to_usize(),
                opening.text_range().end().to_usize(),
            ));
        }
        if let Some((_, closing)) = brace_list_delimiters(&node) {
            owners.insert((
                true,
                closing.kind(),
                closing.text_range().start().to_usize(),
                closing.text_range().end().to_usize(),
            ));
        }
        if let Some(token) =
            list.syntax()
                .children_with_tokens()
                .find_map(|element| match element {
                    SyntaxElement::Token(token) if token.kind() == SyntaxKind::L_BRACE => {
                        Some(token)
                    }
                    _ => None,
                })
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
        let mut next_arm_index = 0;
        let mut previous_arm_index = None;
        for element in list.syntax().children_with_tokens() {
            match element {
                SyntaxElement::Node(arm) if arm.kind() == SyntaxKind::MATCH_ARM => {
                    previous_arm_index = Some(next_arm_index);
                    next_arm_index += 1;
                }
                SyntaxElement::Token(token)
                    if token.kind() == SyntaxKind::COMMA
                        && previous_arm_index.is_some_and(|index| index < arms.len()) =>
                {
                    owners.insert((
                        false,
                        token.kind(),
                        token.text_range().start().to_usize(),
                        token.text_range().end().to_usize(),
                    ));
                }
                _ => {}
            }
        }
    }
    for node in root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::MATCH_EXPR)
    {
        if let Some(token) = ast::MatchExpr::cast(node.clone())
            .and_then(|match_expr| match_expr.scrutinee())
            .and_then(|scrutinee| scrutinee.syntax().last_token())
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
    }
    for node in root
        .descendants()
        .filter(|node| node.kind() == SyntaxKind::MATCH_GUARD)
    {
        if let Some(token) = ast::MatchGuard::cast(node.clone())
            .and_then(|guard| guard.expression())
            .and_then(|expression| expression.syntax().last_token())
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
        if let Some(token) = node
            .children_with_tokens()
            .find_map(|element| match element {
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::IF_KW => Some(token),
                _ => None,
            })
        {
            owners.insert((
                false,
                token.kind(),
                token.text_range().start().to_usize(),
                token.text_range().end().to_usize(),
            ));
        }
    }
    owners
}

fn list_comment_targets(
    list: &SyntaxNode,
    members: &[SyntaxNode],
) -> Result<BraceCommentTargets, SourceDocumentError> {
    let parent = list.parent().ok_or(SourceDocumentError::UnsupportedShape)?;
    let member_kind = match (list.kind(), parent.kind()) {
        (SyntaxKind::FIELD_LIST, SyntaxKind::STRUCT_DEF | SyntaxKind::RESOURCE_DECL) => {
            SyntaxKind::FIELD_DECL
        }
        (SyntaxKind::VARIANT_LIST, SyntaxKind::ENUM_DECL) => SyntaxKind::VARIANT_DECL,
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    let (opening, closing) =
        brace_list_delimiters(list).ok_or(SourceDocumentError::UnsupportedShape)?;
    let opening_end = opening.text_range().end().to_usize();
    let closing_start = closing.text_range().start().to_usize();
    let mut targets = HashMap::with_capacity(members.len() * 2 + 1);
    let mut owner_tokens = vec![opening.clone(), closing.clone()];
    targets.insert(
        (
            false,
            opening.kind(),
            opening.text_range().start().to_usize(),
            opening.text_range().end().to_usize(),
        ),
        ListCommentTarget::OpeningTrailing,
    );
    for (index, member) in members.iter().enumerate() {
        if member.kind() != member_kind {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let first_token = member
            .first_token()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        owner_tokens.push(first_token.clone());
        targets.insert(
            (
                true,
                first_token.kind(),
                first_token.text_range().start().to_usize(),
                first_token.text_range().end().to_usize(),
            ),
            ListCommentTarget::BeforeMember(index),
        );
        let last_token = member
            .last_token()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        owner_tokens.push(last_token.clone());
        targets.insert(
            (
                false,
                last_token.kind(),
                last_token.text_range().start().to_usize(),
                last_token.text_range().end().to_usize(),
            ),
            ListCommentTarget::AfterMember(index),
        );
    }
    targets.insert(
        (
            true,
            closing.kind(),
            closing.text_range().start().to_usize(),
            closing.text_range().end().to_usize(),
        ),
        ListCommentTarget::Closing,
    );

    let mut previous_member = None;
    let mut next_member = 0_usize;
    for element in list.children_with_tokens() {
        match element {
            SyntaxElement::Node(node) if node.kind() == member_kind => {
                previous_member = Some(next_member);
                next_member += 1;
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA => {
                owner_tokens.push(token.clone());
                let key = (
                    true,
                    token.kind(),
                    token.text_range().start().to_usize(),
                    token.text_range().end().to_usize(),
                );
                let target = if next_member < members.len() {
                    ListCommentTarget::BeforeMember(next_member)
                } else {
                    ListCommentTarget::Closing
                };
                targets.insert(key, target);
                if let Some(previous_member) = previous_member {
                    targets.insert(
                        (
                            false,
                            token.kind(),
                            token.text_range().start().to_usize(),
                            token.text_range().end().to_usize(),
                        ),
                        ListCommentTarget::AfterMember(previous_member),
                    );
                }
            }
            _ => {}
        }
    }
    Ok(BraceCommentTargets {
        interior_start: opening_end,
        interior_end: closing_start,
        targets,
        owner_tokens,
    })
}

fn block_comment_targets(
    block: &SyntaxNode,
    statements: &[SyntaxNode],
) -> Result<BraceCommentTargets, SourceDocumentError> {
    if block.kind() != SyntaxKind::BLOCK {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let (opening, closing) =
        brace_list_delimiters(block).ok_or(SourceDocumentError::UnsupportedShape)?;
    let opening_end = opening.text_range().end().to_usize();
    let closing_start = closing.text_range().start().to_usize();
    let mut targets = HashMap::with_capacity(statements.len() * 2 + 1);
    let mut owner_tokens = vec![opening.clone(), closing.clone()];
    targets.insert(
        (
            true,
            opening.kind(),
            opening.text_range().start().to_usize(),
            opening.text_range().end().to_usize(),
        ),
        ListCommentTarget::OpeningLeading,
    );
    targets.insert(
        (
            false,
            opening.kind(),
            opening.text_range().start().to_usize(),
            opening.text_range().end().to_usize(),
        ),
        ListCommentTarget::OpeningTrailing,
    );
    for (index, statement) in statements.iter().enumerate() {
        if !ast::Stmt::can_cast(statement.kind()) {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let first_token = statement
            .first_token()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        owner_tokens.push(first_token.clone());
        targets.insert(
            (
                true,
                first_token.kind(),
                first_token.text_range().start().to_usize(),
                first_token.text_range().end().to_usize(),
            ),
            ListCommentTarget::BeforeMember(index),
        );
        if matches!(
            first_token.kind(),
            SyntaxKind::LET_KW
                | SyntaxKind::VAR_KW
                | SyntaxKind::ASSERT_KW
                | SyntaxKind::RETURN_KW
                | SyntaxKind::BREAK_KW
                | SyntaxKind::CONTINUE_KW
                | SyntaxKind::IF_KW
                | SyntaxKind::MATCH_KW
        ) {
            targets.insert(
                (
                    false,
                    first_token.kind(),
                    first_token.text_range().start().to_usize(),
                    first_token.text_range().end().to_usize(),
                ),
                ListCommentTarget::AfterKeyword(index),
            );
        }
        if let Some(operator_token) = block_statement_operator_token(statement) {
            owner_tokens.push(operator_token.clone());
            targets.insert(
                (
                    false,
                    operator_token.kind(),
                    operator_token.text_range().start().to_usize(),
                    operator_token.text_range().end().to_usize(),
                ),
                ListCommentTarget::AfterOperator(index),
            );
        }
        if let Some(type_colon_token) = block_statement_type_colon_token(statement) {
            owner_tokens.push(type_colon_token.clone());
            targets.insert(
                (
                    false,
                    type_colon_token.kind(),
                    type_colon_token.text_range().start().to_usize(),
                    type_colon_token.text_range().end().to_usize(),
                ),
                ListCommentTarget::AfterTypeColon(index),
            );
        }
        if let Some(keyword_token) = block_statement_while_keyword_token(statement) {
            owner_tokens.push(keyword_token.clone());
            targets.insert(
                (
                    false,
                    keyword_token.kind(),
                    keyword_token.text_range().start().to_usize(),
                    keyword_token.text_range().end().to_usize(),
                ),
                ListCommentTarget::AfterKeyword(index),
            );
        }
        if let Some(keyword_token) = block_statement_for_keyword_token(statement) {
            owner_tokens.push(keyword_token.clone());
            targets.insert(
                (
                    false,
                    keyword_token.kind(),
                    keyword_token.text_range().start().to_usize(),
                    keyword_token.text_range().end().to_usize(),
                ),
                ListCommentTarget::AfterKeyword(index),
            );
        }
        if let Some(keyword_token) = block_statement_expression_keyword_token(statement) {
            owner_tokens.push(keyword_token.clone());
            targets.insert(
                (
                    false,
                    keyword_token.kind(),
                    keyword_token.text_range().start().to_usize(),
                    keyword_token.text_range().end().to_usize(),
                ),
                ListCommentTarget::AfterKeyword(index),
            );
        }
        if let Some(keyword_token) = block_statement_initializer_expression_keyword_token(statement)
        {
            owner_tokens.push(keyword_token.clone());
            targets.insert(
                (
                    false,
                    keyword_token.kind(),
                    keyword_token.text_range().start().to_usize(),
                    keyword_token.text_range().end().to_usize(),
                ),
                ListCommentTarget::AfterExpressionKeyword(index),
            );
        }
        let last_token = statement
            .last_token()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        owner_tokens.push(last_token.clone());
        targets.insert(
            (
                false,
                last_token.kind(),
                last_token.text_range().start().to_usize(),
                last_token.text_range().end().to_usize(),
            ),
            ListCommentTarget::AfterMember(index),
        );
    }
    targets.insert(
        (
            true,
            closing.kind(),
            closing.text_range().start().to_usize(),
            closing.text_range().end().to_usize(),
        ),
        ListCommentTarget::Closing,
    );
    Ok(BraceCommentTargets {
        interior_start: opening_end,
        interior_end: closing_start,
        targets,
        owner_tokens,
    })
}

fn block_statement_operator_token(statement: &SyntaxNode) -> Option<SyntaxToken> {
    match statement.kind() {
        SyntaxKind::LET_STMT | SyntaxKind::VAR_STMT => {
            statement
                .children_with_tokens()
                .find_map(|element| match element {
                    SyntaxElement::Token(token) if token.kind() == SyntaxKind::EQ => Some(token),
                    _ => None,
                })
        }
        SyntaxKind::ASSIGN_STMT => ast::AssignStmt::cast(statement.clone())
            .and_then(|statement| statement.op().map(|(operator, _)| operator)),
        _ => None,
    }
}

fn block_statement_type_colon_token(statement: &SyntaxNode) -> Option<SyntaxToken> {
    if !matches!(
        statement.kind(),
        SyntaxKind::LET_STMT | SyntaxKind::VAR_STMT
    ) {
        return None;
    }
    statement
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COLON => Some(token),
            _ => None,
        })
}

fn block_statement_while_keyword_token(statement: &SyntaxNode) -> Option<SyntaxToken> {
    let Stmt::Expr(statement) = ast::Stmt::cast(statement.clone())? else {
        return None;
    };
    let Expr::While(while_expr) = statement.expression()? else {
        return None;
    };
    while_expr
        .syntax()
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::WHILE_KW => Some(token),
            _ => None,
        })
}

fn block_statement_for_keyword_token(statement: &SyntaxNode) -> Option<SyntaxToken> {
    let Stmt::Expr(statement) = ast::Stmt::cast(statement.clone())? else {
        return None;
    };
    let Expr::For(for_expr) = statement.expression()? else {
        return None;
    };
    for_expr
        .syntax()
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::FOR_KW => Some(token),
            _ => None,
        })
}

fn expression_keyword_token(expression: &Expr) -> Option<SyntaxToken> {
    let (expression, keyword_kind) = match expression {
        Expr::Loop(loop_expr) => (loop_expr.syntax(), SyntaxKind::LOOP_KW),
        Expr::Match(match_expr) => (match_expr.syntax(), SyntaxKind::MATCH_KW),
        Expr::UnsafeBlock(unsafe_block) => (unsafe_block.syntax(), SyntaxKind::UNSAFE_KW),
        Expr::AsyncBlock(async_block) => (async_block.syntax(), SyntaxKind::ASYNC_KW),
        Expr::Closure(closure) => (closure.syntax(), SyntaxKind::FN_KW),
        Expr::Region(region_expr) => {
            return region_expr
                .syntax()
                .children_with_tokens()
                .find_map(|element| match element {
                    SyntaxElement::Token(token)
                        if token.kind() == SyntaxKind::LIDENT && token.text() == "region" =>
                    {
                        Some(token)
                    }
                    _ => None,
                });
        }
        _ => return None,
    };
    expression
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == keyword_kind => Some(token),
            _ => None,
        })
}

fn block_statement_expression_keyword_token(statement: &SyntaxNode) -> Option<SyntaxToken> {
    let Stmt::Expr(statement) = ast::Stmt::cast(statement.clone())? else {
        return None;
    };
    expression_keyword_token(&statement.expression()?)
}

fn block_statement_initializer_expression_keyword_token(
    statement: &SyntaxNode,
) -> Option<SyntaxToken> {
    let statement = ast::Stmt::cast(statement.clone())?;
    let expression = match statement {
        Stmt::Let(statement) => statement.initializer()?,
        Stmt::Var(statement) => statement.initializer()?,
        Stmt::Assign(statement) => statement.rhs()?,
        _ => return None,
    };
    expression_keyword_token(&expression)
}

fn brace_list_delimiters(list: &SyntaxNode) -> Option<(SyntaxToken, SyntaxToken)> {
    let direct_tokens = list
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(_) => None,
        })
        .collect::<Vec<_>>();
    let opening = direct_tokens
        .iter()
        .find(|token| token.kind() == SyntaxKind::L_BRACE)?;
    let closing = direct_tokens
        .iter()
        .rfind(|token| token.kind() == SyntaxKind::R_BRACE)?;
    Some(((*opening).clone(), (*closing).clone()))
}

fn collect_list_comments(
    list: &SyntaxNode,
    members: &[SyntaxNode],
) -> Result<ListComments, SourceDocumentError> {
    let targets = list_comment_targets(list, members)?;
    collect_brace_unit_comments(members.len(), targets)
}

fn collect_block_comments(
    block: &SyntaxNode,
    statements: &[SyntaxNode],
) -> Result<ListComments, SourceDocumentError> {
    let targets = block_comment_targets(block, statements)?;
    collect_brace_unit_comments(statements.len(), targets)
}

fn block_has_comments(
    block: &ast::Block,
    statements: &[Stmt],
) -> Result<bool, SourceDocumentError> {
    let statement_nodes = statements
        .iter()
        .map(|statement| statement.syntax().clone())
        .collect::<Vec<_>>();
    Ok(collect_block_comments(block.syntax(), &statement_nodes)?.has_comments())
}

fn block_has_opening_leading_comments(block: &ast::Block) -> Result<bool, SourceDocumentError> {
    let statements = block
        .statements()
        .map(|statement| statement.syntax().clone())
        .collect::<Vec<_>>();
    Ok(!collect_block_comments(block.syntax(), &statements)?
        .opening_leading
        .is_empty())
}

fn collect_brace_unit_comments(
    member_count: usize,
    targets: BraceCommentTargets,
) -> Result<ListComments, SourceDocumentError> {
    let BraceCommentTargets {
        interior_start,
        interior_end,
        targets,
        owner_tokens,
    } = targets;
    let mut comments = ListComments {
        opening_leading: Vec::new(),
        opening_trailing: Vec::new(),
        before: vec![Vec::new(); member_count],
        after_keyword: vec![Vec::new(); member_count],
        after_expression_keyword: vec![Vec::new(); member_count],
        after_type_colon: vec![Vec::new(); member_count],
        after_operator: vec![Vec::new(); member_count],
        after: vec![Vec::new(); member_count],
        closing: Vec::new(),
    };
    let mut candidates = collect_comments_from_tokens(&owner_tokens)?;
    candidates.sort_by_key(|comment| comment.range.start);
    for comment in candidates {
        let target = targets.get(&comment_owner_key(&comment)).copied();
        if matches!(target, Some(ListCommentTarget::OpeningLeading)) {
            comments.opening_leading.push(comment);
            continue;
        }
        if comment.range.start < interior_start || comment.range.end > interior_end {
            continue;
        }
        match target {
            Some(ListCommentTarget::OpeningLeading) => {}
            Some(ListCommentTarget::OpeningTrailing) => comments.opening_trailing.push(comment),
            Some(ListCommentTarget::BeforeMember(index)) => comments.before[index].push(comment),
            Some(ListCommentTarget::AfterKeyword(index)) => {
                comments.after_keyword[index].push(comment)
            }
            Some(ListCommentTarget::AfterExpressionKeyword(index)) => {
                comments.after_expression_keyword[index].push(comment)
            }
            Some(ListCommentTarget::AfterTypeColon(index)) => {
                comments.after_type_colon[index].push(comment)
            }
            Some(ListCommentTarget::AfterOperator(index)) => {
                comments.after_operator[index].push(comment)
            }
            Some(ListCommentTarget::AfterMember(index)) => comments.after[index].push(comment),
            Some(ListCommentTarget::Closing) => comments.closing.push(comment),
            None => {}
        }
    }
    Ok(comments)
}

fn collect_comments_from_tokens(
    tokens: &[SyntaxToken],
) -> Result<Vec<SourceComment>, SourceDocumentError> {
    let mut comments = Vec::new();
    let mut seen_tokens = HashSet::with_capacity(tokens.len());
    for token in tokens {
        let range = token.text_range();
        let token_key = (
            token.kind(),
            range.start().to_usize(),
            range.end().to_usize(),
        );
        if !seen_tokens.insert(token_key) {
            continue;
        }
        let green = token.green();
        let token_start = range.start().to_usize();
        let token_end = range.end().to_usize();
        let leading_len = green
            .leading_trivia()
            .iter()
            .try_fold(0_usize, |length, trivia| {
                length
                    .checked_add(trivia.text().len())
                    .ok_or(SourceDocumentError::UnsupportedShape)
            })?;
        let mut cursor = token_start
            .checked_sub(leading_len)
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let mut candidates = CommentCandidates::default();
        for trivia in green.leading_trivia() {
            scan_source_trivia(
                trivia,
                true,
                token.kind(),
                token_start..token_end,
                &mut cursor,
                &mut candidates,
            )?;
        }
        cursor = token_end;
        for trivia in green.trailing_trivia() {
            scan_source_trivia(
                trivia,
                false,
                token.kind(),
                token_start..token_end,
                &mut cursor,
                &mut candidates,
            )?;
        }
        comments.extend(candidates.leading);
        comments.extend(candidates.trailing);
        comments.extend(candidates.eof);
    }
    Ok(comments)
}

fn scan_source_trivia(
    trivia: &GreenTrivia,
    is_leading: bool,
    token_kind: SyntaxKind,
    owner_token_range: Range<usize>,
    cursor: &mut usize,
    candidates: &mut CommentCandidates,
) -> Result<(), SourceDocumentError> {
    let start = *cursor;
    *cursor = cursor
        .checked_add(trivia.text().len())
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    match trivia.kind() {
        TriviaKind::LineComment | TriviaKind::BlockComment => {
            let comment = SourceComment {
                range: start..*cursor,
                text: normalize_comment_text(trivia.text()),
                kind: trivia.kind(),
                is_leading,
                owner_token_kind: token_kind,
                owner_token_range,
            };
            if is_leading && token_kind == SyntaxKind::EOF {
                candidates.eof.push(comment);
            } else if is_leading {
                candidates.leading.push(comment);
            } else {
                candidates.trailing.push(comment);
            }
        }
        TriviaKind::Shebang => {
            if !is_leading || start != 0 || candidates.shebang.is_some() {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            candidates.shebang = Some(SourceComment {
                range: start..*cursor,
                text: normalize_comment_text(trivia.text()),
                kind: TriviaKind::Shebang,
                is_leading,
                owner_token_kind: token_kind,
                owner_token_range,
            });
        }
        TriviaKind::Whitespace | TriviaKind::LineBreak | TriviaKind::Skipped => {}
    }
    Ok(())
}

fn normalize_comment_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| line.trim_end_matches([' ', '\t']))
        .collect::<Vec<_>>()
        .join("\n")
}

fn append_comment_documents(
    document: &mut Vec<Doc>,
    comments: &[SourceComment],
    source: &str,
    previous_end: Option<usize>,
) -> Result<(), SourceDocumentError> {
    let mut previous_end = previous_end;
    for comment in comments {
        if let Some(previous_end) = previous_end {
            append_comment_gap(
                document,
                source,
                Some(previous_end),
                Some(comment.range.start),
            )?;
        }
        document.push(Doc::verbatim(comment.text.clone()));
        previous_end = Some(comment.range.end);
    }
    Ok(())
}

fn append_comment_gap(
    document: &mut Vec<Doc>,
    source: &str,
    start: Option<usize>,
    end: Option<usize>,
) -> Result<(), SourceDocumentError> {
    let (Some(start), Some(end)) = (start, end) else {
        return Ok(());
    };
    let gap = source
        .get(start..end)
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    document.push(Doc::HardLine);
    if line_break_count(gap) > 1 {
        document.push(Doc::HardLine);
    }
    Ok(())
}

fn line_break_count(text: &str) -> usize {
    text.bytes().filter(|byte| *byte == b'\n').count()
}

fn decorate_top_level_document(
    document: Doc,
    leading_comments: &[SourceComment],
    trailing_comments: &[SourceComment],
    source: &str,
    range: TextRange,
) -> Result<(Doc, bool), SourceDocumentError> {
    let mut documents = Vec::new();
    if !leading_comments.is_empty() {
        append_comment_documents(&mut documents, leading_comments, source, None)?;
        append_comment_gap(
            &mut documents,
            source,
            leading_comments.last().map(|comment| comment.range.end),
            Some(range.start().to_usize()),
        )?;
    }
    documents.push(document);
    let mut multiline = !leading_comments.is_empty();
    for comment in trailing_comments {
        if comment.kind == TriviaKind::LineComment {
            documents.push(Doc::line_suffix(Doc::text(format!(" {}", comment.text))));
        } else {
            documents.push(Doc::text(" "));
            documents.push(Doc::verbatim(comment.text.clone()));
            multiline |= comment.text.contains('\n');
        }
    }
    Ok((Doc::concat(documents), multiline))
}

fn decorate_item_document(
    mut unit: ItemDocument,
    leading_comments: &[SourceComment],
    trailing_comments: &[SourceComment],
    source: &str,
) -> Result<ItemDocument, SourceDocumentError> {
    let range = TextRange::new(
        TextSize::from_usize(unit.start).ok_or(SourceDocumentError::UnsupportedShape)?,
        TextSize::from_usize(unit.end).ok_or(SourceDocumentError::UnsupportedShape)?,
    )
    .ok_or(SourceDocumentError::UnsupportedShape)?;
    let (document, comments_multiline) = decorate_top_level_document(
        unit.document,
        leading_comments,
        trailing_comments,
        source,
        range,
    )?;
    unit.document = document;
    if let Some(comment) = leading_comments.first() {
        unit.start = comment.range.start;
    }
    if let Some(comment) = trailing_comments.last() {
        unit.end = comment.range.end;
    }
    unit.multiline |= comments_multiline;
    Ok(unit)
}

struct ItemDocument {
    document: Doc,
    start: usize,
    end: usize,
    multiline: bool,
}

fn detached_comment_item_document(
    comments: &[SourceComment],
    source: &str,
) -> Result<ItemDocument, SourceDocumentError> {
    let first = comments
        .first()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let last = comments
        .last()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let mut documents = Vec::new();
    append_comment_documents(&mut documents, comments, source, None)?;
    Ok(ItemDocument {
        document: Doc::concat(documents),
        start: first.range.start,
        end: last.range.end,
        multiline: false,
    })
}

struct UseDeclarationDocument {
    document: Doc,
    sort_key: String,
    is_public: bool,
}

#[derive(Clone, Copy)]
enum ExpressionContext {
    Unprotected,
    Protected,
    ParenthesizedWhole,
}

#[derive(Default)]
struct ExpressionGroupIds {
    next_id: u32,
}

impl ExpressionGroupIds {
    fn next(&mut self) -> GroupId {
        // Source-offset groups stay below the 16 MiB input limit; expression
        // groups use this disjoint namespace and a per-document sequence.
        const EXPRESSION_GROUP_START: u32 = 1 << 31;
        debug_assert!(self.next_id < EXPRESSION_GROUP_START);
        let group = GroupId(EXPRESSION_GROUP_START | self.next_id);
        self.next_id += 1;
        group
    }
}

fn function_document(
    source: &ParsedSource,
    function: &ast::FnDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let prefix = item_prefix_document(source, function.docs(), function.attrs())?;
    let visibility_prefix = visibility_prefix(function.visibility())?;
    let is_unsafe = function.unsafe_token().is_some();
    let is_async = function.async_token().is_some();
    let mut expected_tokens = Vec::new();
    if is_unsafe {
        expected_tokens.push(SyntaxKind::UNSAFE_KW);
    }
    if is_async {
        expected_tokens.push(SyntaxKind::ASYNC_KW);
    }
    expected_tokens.push(SyntaxKind::FN_KW);
    if direct_token_kinds(function.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>()
        != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut clauses = Vec::with_capacity(7);
    if let Some(clause) = function.where_clause() {
        clauses.push(where_clause_document(source, &clause)?);
    }
    if let Some(clause) = function.borrows_clause() {
        clauses.push(borrows_clause_document(source, &clause)?);
    }
    if let Some(clause) = function.effects_clause() {
        clauses.push(effects_clause_document(source, &clause)?);
    }
    clauses.extend(
        function
            .requires_clauses()
            .map(|clause| requires_clause_document(source, &clause, expression_group_ids))
            .collect::<Result<Vec<_>, _>>()?,
    );
    clauses.extend(
        function
            .ensures_clauses()
            .map(|clause| ensures_clause_document(source, &clause, expression_group_ids))
            .collect::<Result<Vec<_>, _>>()?,
    );
    let params = function
        .params()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let parameters = parameter_list_document(source, &params)?;
    let return_type = function
        .ret_type()
        .map(|return_type| {
            let ty = return_type
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            type_document(source, &ty)
        })
        .transpose()?
        .map(|type_doc| Doc::concat([Doc::text(" -> "), type_doc]))
        .unwrap_or_else(|| Doc::text(""));
    let body = function.body();
    let name = function
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let name = node_text(source, name.syntax());
    let head_prefix = format!(
        "{visibility_prefix}{}{}",
        if is_unsafe { "unsafe " } else { "" },
        if is_async { "async " } else { "" },
    );
    let head_prefix = format!("{head_prefix}fn {name}");
    let generic_params = function
        .generic_params()
        .map(|params| {
            let document = generic_params_document(source, &params)?;
            let prefix_width = flat_document_width(&document)
                .and_then(|generic_width| {
                    head_prefix
                        .chars()
                        .count()
                        .checked_add(generic_width)?
                        .checked_add(1)
                })
                .unwrap_or(usize::MAX);
            Ok(if prefix_width <= MAX_LINE_WIDTH {
                Doc::flat(document)
            } else {
                document
            })
        })
        .transpose()?
        .unwrap_or_else(|| Doc::text(""));
    let body_has_opening_leading_comments = body
        .as_ref()
        .map(block_has_opening_leading_comments)
        .transpose()?
        .unwrap_or(false);
    let body_document = body
        .as_ref()
        .map(|body| block_document(source, body, expression_group_ids))
        .transpose()?;
    let head = Doc::concat([
        Doc::text(head_prefix),
        generic_params,
        parameters,
        return_type,
    ]);
    let document = if clauses.is_empty() {
        if let Some(body) = body_document {
            Doc::concat([head, Doc::text(" "), body])
        } else {
            head
        }
    } else {
        let mut clause_lines = vec![Doc::HardLine];
        for (index, clause) in clauses.into_iter().enumerate() {
            if index > 0 {
                clause_lines.push(Doc::HardLine);
            }
            clause_lines.push(clause);
        }
        let mut document = vec![head, Doc::indent(Doc::concat(clause_lines))];
        if let Some(body) = body_document {
            if body_has_opening_leading_comments {
                document.push(body);
            } else {
                document.extend([Doc::HardLine, body]);
            }
        }
        Doc::concat(document)
    };
    let document = Doc::concat([prefix, document]);
    let range = function.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn visibility_prefix(
    visibility: Option<ast::Visibility>,
) -> Result<&'static str, SourceDocumentError> {
    visibility
        .map(|visibility| {
            match direct_token_kinds(visibility.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>()
                .as_slice()
            {
                [SyntaxKind::PUB_KW] => Ok("pub "),
                [SyntaxKind::EXPORT_KW] => Ok("export "),
                _ => Err(SourceDocumentError::UnsupportedShape),
            }
        })
        .transpose()
        .map(|prefix| prefix.unwrap_or(""))
}

fn item_prefix_child_ranges(docs: Option<&ast::Docs>, attrs: &[ast::Attribute]) -> Vec<TextRange> {
    docs.into_iter()
        .map(|docs| docs.syntax().text_range())
        .chain(
            attrs
                .iter()
                .map(|attribute| attribute.syntax().text_range()),
        )
        .collect()
}

fn item_prefix_document(
    source: &ParsedSource,
    docs: Option<ast::Docs>,
    attrs: impl IntoIterator<Item = ast::Attribute>,
) -> Result<Doc, SourceDocumentError> {
    let mut documents = Vec::new();
    if let Some(docs) = docs {
        documents.extend([docs_document(source, &docs)?, Doc::HardLine]);
    }
    for attribute in attrs {
        documents.extend([attribute_document(source, &attribute)?, Doc::HardLine]);
    }
    Ok(Doc::concat(documents))
}

fn docs_document(source: &ParsedSource, docs: &ast::Docs) -> Result<Doc, SourceDocumentError> {
    docs_document_with_kind(source, docs, SyntaxKind::DOC_OUTER)
}

fn docs_document_with_kind(
    source: &ParsedSource,
    docs: &ast::Docs,
    expected_kind: SyntaxKind,
) -> Result<Doc, SourceDocumentError> {
    let mut documents = Vec::new();
    for element in docs.syntax().children_with_tokens() {
        let SyntaxElement::Token(token) = element else {
            return Err(SourceDocumentError::UnsupportedShape);
        };
        if token.kind() != expected_kind {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        if !documents.is_empty() {
            documents.push(Doc::HardLine);
        }
        documents.push(Doc::text(
            token_text(source, &token)?
                .trim_end_matches([' ', '\t'])
                .to_owned(),
        ));
    }
    if documents.is_empty() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat(documents))
}

fn module_header_document(
    source: &ParsedSource,
    header: &ast::ModuleHeader,
) -> Result<Doc, SourceDocumentError> {
    let docs = header.docs();
    let has_docs = docs.is_some();
    if header.syntax().children().count() != usize::from(has_docs) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let tokens = header
        .syntax()
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(_) => None,
        })
        .collect::<Vec<_>>();
    let unsafe_token = header.unsafe_token();
    let expected_kinds = if unsafe_token.is_some() {
        vec![
            SyntaxKind::UNSAFE_KW,
            SyntaxKind::LIDENT,
            SyntaxKind::NEWLINE,
        ]
    } else {
        Vec::new()
    };
    if tokens.iter().map(SyntaxToken::kind).collect::<Vec<_>>() != expected_kinds {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(3);
    if let Some(docs) = docs {
        documents.push(docs_document_with_kind(
            source,
            &docs,
            SyntaxKind::DOC_INNER,
        )?);
    }
    if unsafe_token.is_some() {
        if has_docs {
            documents.extend([Doc::HardLine, Doc::HardLine]);
        }
        let module_token = tokens.get(1).ok_or(SourceDocumentError::UnsupportedShape)?;
        if token_text(source, module_token)? != "module" {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        documents.push(Doc::text("unsafe module"));
    }
    if documents.is_empty() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat(documents))
}

fn use_declaration_document(
    source: &ParsedSource,
    declaration: &ast::UseDecl,
) -> Result<UseDeclarationDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attributes = declaration.attrs().collect::<Vec<_>>();
    let visibility = declaration.visibility();
    let alias = declaration.alias();
    let items = declaration.items();
    let mut expected_child_ranges = Vec::new();
    expected_child_ranges.extend(docs.as_ref().map(|docs| docs.syntax().text_range()));
    expected_child_ranges.extend(
        attributes
            .iter()
            .map(|attribute| attribute.syntax().text_range()),
    );
    expected_child_ranges.extend(
        visibility
            .as_ref()
            .map(|visibility| visibility.syntax().text_range()),
    );
    expected_child_ranges.extend(alias.as_ref().map(|alias| alias.syntax().text_range()));
    expected_child_ranges.extend(items.as_ref().map(|items| items.syntax().text_range()));
    let direct_tokens = direct_token_kinds(declaration.syntax());
    if declaration
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(expected_child_ranges)
        || direct_tokens
            .iter()
            .copied()
            .skip_while(|kind| *kind == SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::USE_KW, SyntaxKind::PKG_PATH]
        || direct_tokens
            .iter()
            .skip_while(|kind| **kind == SyntaxKind::NEWLINE)
            .any(|kind| *kind == SyntaxKind::NEWLINE)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let is_public = visibility
        .as_ref()
        .is_some_and(|visibility| visibility.pub_token().is_some());
    let prefix = visibility_prefix(visibility)?;
    if prefix == "export " {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let package = declaration
        .package()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if package.syntax().kind() != SyntaxKind::PKG_PATH {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let package = token_text(source, package.syntax())?;
    let alias = alias
        .as_ref()
        .map(|alias| use_alias_document(source, alias))
        .transpose()?;
    let items = items
        .as_ref()
        .map(|items| use_items_document(source, items))
        .transpose()?;
    let suffix = format!(
        "{}{}",
        alias.as_ref().map_or("", |(_, text)| text.as_str()),
        items.as_ref().map_or("", |(_, text)| text.as_str())
    );

    let mut documents = vec![item_prefix_document(source, docs, attributes)?];
    documents.extend([
        Doc::text(format!("{prefix}use ")),
        Doc::text(package.to_owned()),
    ]);
    if let Some((document, _)) = alias {
        documents.push(document);
    }
    if let Some((document, _)) = items {
        documents.push(document);
    }
    Ok(UseDeclarationDocument {
        document: Doc::concat(documents),
        sort_key: format!("use {package}{suffix}"),
        is_public,
    })
}

fn use_alias_document(
    source: &ParsedSource,
    alias: &ast::UseAlias,
) -> Result<(Doc, String), SourceDocumentError> {
    let name = alias.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    if alias.syntax().children().count() != 1
        || alias
            .syntax()
            .children()
            .next()
            .map(|child| child.text_range())
            != Some(name.syntax().text_range())
        || direct_token_kinds(alias.syntax()) != [SyntaxKind::AS_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let name = name_node_text(source, name.syntax())?;
    Ok((
        Doc::concat([Doc::text(" as "), Doc::text(name.clone())]),
        format!(" as {name}"),
    ))
}

fn use_items_document(
    source: &ParsedSource,
    items: &ast::UseItems,
) -> Result<(Doc, String), SourceDocumentError> {
    let mut use_items = items.items().collect::<Vec<_>>();
    if items.syntax().children().count() != use_items.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let raw_token_kinds = direct_token_kinds(items.syntax());
    let token_kinds = raw_token_kinds
        .iter()
        .copied()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let separator_count = token_kinds
        .iter()
        .filter(|kind| **kind == SyntaxKind::COMMA)
        .count()
        + raw_token_kinds
            .iter()
            .filter(|kind| **kind == SyntaxKind::NEWLINE)
            .count();
    if token_kinds.len() < 3
        || token_kinds[0..2] != [SyntaxKind::DOT, SyntaxKind::L_BRACE]
        || token_kinds.last() != Some(&SyntaxKind::R_BRACE)
        || token_kinds[2..token_kinds.len() - 1]
            .iter()
            .any(|kind| *kind != SyntaxKind::COMMA)
        || token_kinds[2..token_kinds.len() - 1].len() > use_items.len()
        || separator_count < use_items.len().saturating_sub(1)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut named_items = use_items
        .drain(..)
        .map(|item| {
            let name = item.name().ok_or(SourceDocumentError::UnsupportedShape)?;
            let alias = item.alias();
            let mut expected_child_ranges = vec![name.syntax().text_range()];
            expected_child_ranges.extend(alias.as_ref().map(|alias| alias.syntax().text_range()));
            if item
                .syntax()
                .children()
                .map(|child| child.text_range())
                .ne(expected_child_ranges)
                || direct_token_kinds(item.syntax())
                    != if alias.is_some() {
                        vec![SyntaxKind::AS_KW]
                    } else {
                        Vec::new()
                    }
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let name = name_ref_text(source, &name, &[SyntaxKind::LIDENT, SyntaxKind::UIDENT])?;
            let alias = alias
                .as_ref()
                .map(|alias| name_node_text(source, alias.syntax()))
                .transpose()?;
            Ok((name, alias))
        })
        .collect::<Result<Vec<_>, SourceDocumentError>>()?;
    named_items.sort_by(|left, right| {
        left.0.as_bytes().cmp(right.0.as_bytes()).then_with(|| {
            left.1
                .as_deref()
                .map(str::as_bytes)
                .cmp(&right.1.as_deref().map(str::as_bytes))
        })
    });

    let flat_items = named_items
        .iter()
        .map(|(name, alias)| match alias {
            Some(alias) => format!("{name} as {alias}"),
            None => name.clone(),
        })
        .collect::<Vec<_>>();
    let flat_text = format!(".{{{}}}", flat_items.join(", "));
    if flat_items.is_empty() {
        return Ok((Doc::text(".{}"), flat_text));
    }

    let group = GroupId(items.syntax().text_range().start().raw());
    let mut contents = Vec::with_capacity(flat_items.len() * 2);
    contents.push(Doc::SoftLine);
    for (index, item) in flat_items.iter().enumerate() {
        if index > 0 {
            contents.extend([
                Doc::if_broken(group, Doc::text(""), Doc::text(",")),
                Doc::Line,
            ]);
        }
        contents.push(Doc::text(item.clone()));
    }
    Ok((
        Doc::group(
            group,
            Doc::concat([
                Doc::text(".{"),
                Doc::indent(Doc::concat(contents)),
                Doc::SoftLine,
                Doc::text("}"),
            ]),
        ),
        flat_text,
    ))
}

fn name_node_text(
    source: &ParsedSource,
    name: &sai_syntax::SyntaxNode,
) -> Result<String, SourceDocumentError> {
    let tokens = name
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(_) => None,
        })
        .collect::<Vec<_>>();
    if name.children().next().is_some()
        || tokens.len() != 1
        || !matches!(tokens[0].kind(), SyntaxKind::LIDENT | SyntaxKind::UIDENT)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(token_text(source, &tokens[0])?.to_owned())
}

fn name_ref_text(
    source: &ParsedSource,
    name: &ast::NameRef,
    allowed_kinds: &[SyntaxKind],
) -> Result<String, SourceDocumentError> {
    let tokens = name
        .syntax()
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(_) => None,
        })
        .collect::<Vec<_>>();
    if name.syntax().children().next().is_some()
        || tokens.len() != 1
        || !allowed_kinds.contains(&tokens[0].kind())
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(token_text(source, &tokens[0])?.to_owned())
}

fn attribute_document(
    source: &ParsedSource,
    attribute: &ast::Attribute,
) -> Result<Doc, SourceDocumentError> {
    let name = attribute
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let args = attribute.args();
    if attribute
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(std::iter::once(name.syntax().text_range())
            .chain(args.as_ref().map(|args| args.syntax().text_range())))
        || direct_token_kinds(attribute.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::AT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let name = name_ref_text(source, &name, &[SyntaxKind::LIDENT])?;
    let mut documents = vec![Doc::text(format!("@{name}"))];
    if let Some(args) = args {
        documents.push(attribute_args_document(source, &args)?);
    }
    Ok(Doc::concat(documents))
}

fn attribute_args_document(
    source: &ParsedSource,
    args: &ast::AttrArgs,
) -> Result<Doc, SourceDocumentError> {
    let items = args.items().collect::<Vec<_>>();
    if args
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(items.iter().map(|item| item.syntax().text_range()))
        || !comma_delimited_tokens_match(
            args.syntax(),
            SyntaxKind::L_PAREN,
            SyntaxKind::R_PAREN,
            items.len(),
        )
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = items
        .iter()
        .map(|item| attribute_arg_document(source, item))
        .collect::<Result<Vec<_>, _>>()?;
    let range = args.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "(",
        ")",
        documents,
        false,
    ))
}

fn attribute_arg_document(
    source: &ParsedSource,
    argument: &ast::AttrArg,
) -> Result<Doc, SourceDocumentError> {
    let name = argument.name();
    let args = argument.args();
    let value = argument.value();
    let string = argument.string();
    let integer = argument.integer();
    let direct_tokens = direct_token_kinds(argument.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();

    if let Some(name) = name {
        let name_text = name_ref_text(source, &name, &[SyntaxKind::LIDENT, SyntaxKind::UIDENT])?;
        if let Some(args) = args {
            if value.is_some()
                || string.is_some()
                || integer.is_some()
                || !direct_tokens.is_empty()
                || argument
                    .syntax()
                    .children()
                    .map(|child| child.text_range())
                    .ne([name.syntax().text_range(), args.syntax().text_range()])
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            return Ok(Doc::concat([
                Doc::text(name_text),
                attribute_args_document(source, &args)?,
            ]));
        }
        if let Some(value) = value {
            if string.is_some()
                || integer.is_some()
                || direct_tokens.as_slice() != [SyntaxKind::EQ]
                || argument
                    .syntax()
                    .children()
                    .map(|child| child.text_range())
                    .ne([name.syntax().text_range(), value.syntax().text_range()])
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let value_text =
                name_ref_text(source, &value, &[SyntaxKind::LIDENT, SyntaxKind::UIDENT])?;
            return Ok(Doc::concat([
                Doc::text(name_text),
                Doc::text(" = "),
                Doc::text(value_text),
            ]));
        }
        if let Some(string) = string {
            if integer.is_some()
                || direct_tokens.as_slice() != [SyntaxKind::EQ, SyntaxKind::STRING]
                || argument
                    .syntax()
                    .children()
                    .map(|child| child.text_range())
                    .ne([name.syntax().text_range()])
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            return Ok(Doc::concat([
                Doc::text(name_text),
                Doc::text(" = "),
                literal_document(source, LiteralToken::String(string))?,
            ]));
        }
        if let Some(integer) = integer {
            if direct_tokens.as_slice() != [SyntaxKind::EQ, SyntaxKind::INT]
                || argument
                    .syntax()
                    .children()
                    .map(|child| child.text_range())
                    .ne([name.syntax().text_range()])
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            return Ok(Doc::concat([
                Doc::text(name_text),
                Doc::text(" = "),
                Doc::text(normalize_integer_literal(token_text(
                    source,
                    integer.syntax(),
                )?)),
            ]));
        }
        if !direct_tokens.is_empty()
            || argument
                .syntax()
                .children()
                .map(|child| child.text_range())
                .ne([name.syntax().text_range()])
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        return Ok(Doc::text(name_text));
    }

    if let Some(string) = string {
        if args.is_some()
            || value.is_some()
            || integer.is_some()
            || direct_tokens.as_slice() != [SyntaxKind::STRING]
            || argument.syntax().children().next().is_some()
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        return literal_document(source, LiteralToken::String(string));
    }
    if let Some(integer) = integer {
        if args.is_some()
            || value.is_some()
            || direct_tokens.as_slice() != [SyntaxKind::INT]
            || argument.syntax().children().next().is_some()
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        return Ok(Doc::text(normalize_integer_literal(token_text(
            source,
            integer.syntax(),
        )?)));
    }
    Err(SourceDocumentError::UnsupportedShape)
}

fn impl_document(
    source: &ParsedSource,
    implementation: &ast::ImplDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = implementation.docs();
    let attrs = implementation.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;

    let is_unsafe = implementation.unsafe_token().is_some();
    let generic_params = implementation.generic_params();
    let trait_ref = implementation.trait_ref();
    let for_type = implementation.for_type();
    let inherent_type = implementation.inherent_type();
    let interface_path = implementation.interface_path();
    let where_clause = implementation.where_clause();
    let items = implementation
        .items()
        .ok_or(SourceDocumentError::UnsupportedShape)?;

    let target_document = match (
        trait_ref.as_ref(),
        for_type.as_ref(),
        inherent_type.as_ref(),
        interface_path.as_ref(),
    ) {
        (Some(trait_ref), Some(for_type), None, None) => {
            let direct_types = implementation
                .syntax()
                .children()
                .filter_map(ast::Type::cast)
                .collect::<Vec<_>>();
            if direct_types.len() != 2
                || direct_types[0].syntax().text_range() != trait_ref.syntax().text_range()
                || direct_types[1].syntax().text_range() != for_type.syntax().text_range()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Doc::concat([
                type_document(source, trait_ref)?,
                Doc::text(" for "),
                type_document(source, for_type)?,
            ])
        }
        (None, None, Some(inherent_type), None) => {
            let direct_types = implementation
                .syntax()
                .children()
                .filter_map(ast::Type::cast)
                .collect::<Vec<_>>();
            if direct_types.len() != 1
                || direct_types[0].syntax().text_range() != inherent_type.syntax().text_range()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            type_document(source, inherent_type)?
        }
        (None, None, None, Some(interface_path)) => {
            if implementation
                .syntax()
                .children()
                .filter_map(ast::Type::cast)
                .next()
                .is_some()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            mod_path_document(source, interface_path)?
        }
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };

    let expected_direct_tokens = [
        is_unsafe.then_some(SyntaxKind::UNSAFE_KW),
        Some(SyntaxKind::IMPL_KW),
        trait_ref.as_ref().map(|_| SyntaxKind::FOR_KW),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if direct_token_kinds(implementation.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>()
        != expected_direct_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut expected_children = item_prefix_child_ranges(docs.as_ref(), &attrs);
    if let Some(generic_params) = generic_params.as_ref() {
        expected_children.push(generic_params.syntax().text_range());
    }
    match (
        trait_ref.as_ref(),
        for_type.as_ref(),
        inherent_type.as_ref(),
        interface_path.as_ref(),
    ) {
        (Some(trait_ref), Some(for_type), None, None) => {
            expected_children.extend([
                trait_ref.syntax().text_range(),
                for_type.syntax().text_range(),
            ]);
        }
        (None, None, Some(inherent_type), None) => {
            expected_children.push(inherent_type.syntax().text_range());
        }
        (None, None, None, Some(interface_path)) => {
            expected_children.push(interface_path.syntax().text_range());
        }
        _ => unreachable!("the target shape was checked above"),
    }
    if let Some(where_clause) = where_clause.as_ref() {
        expected_children.push(where_clause.syntax().text_range());
    }
    expected_children.push(items.syntax().text_range());
    if implementation
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(expected_children)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let item_body = item_list_body_document(
        source,
        &items,
        expression_group_ids,
        |child, expression_group_ids| match child.kind() {
            SyntaxKind::FN_DECL => function_document(
                source,
                &ast::FnDecl::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
                expression_group_ids,
            ),
            SyntaxKind::ASSOC_TYPE_DEF => assoc_type_def_document(
                source,
                &ast::AssocTypeDef::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
            ),
            _ => Err(SourceDocumentError::UnsupportedShape),
        },
    )?;

    let mut head = vec![Doc::text(if is_unsafe { "unsafe impl" } else { "impl" })];
    if let Some(generic_params) = generic_params.as_ref() {
        head.extend([
            generic_params_document(source, generic_params)?,
            Doc::text(" "),
        ]);
    } else {
        head.push(Doc::text(" "));
    }
    head.push(target_document);
    let head = Doc::concat(head);
    let document = if let Some(where_clause) = where_clause {
        Doc::concat([
            head,
            Doc::indent(Doc::concat([
                Doc::HardLine,
                where_clause_document(source, &where_clause)?,
            ])),
            Doc::HardLine,
            item_body,
        ])
    } else {
        Doc::concat([head, Doc::text(" "), item_body])
    };
    let document = Doc::concat([prefix, document]);
    let range = implementation.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn trait_document(
    source: &ParsedSource,
    declaration: &ast::TraitDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;

    let visibility_prefix = visibility_prefix(declaration.visibility())?;
    let is_unsafe = declaration.unsafe_token().is_some();
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let generic_params = declaration.generic_params();
    let bounds = declaration.bounds();
    let where_clause = declaration.where_clause();
    let items = declaration
        .items()
        .ok_or(SourceDocumentError::UnsupportedShape)?;

    let expected_direct_tokens = [
        is_unsafe.then_some(SyntaxKind::UNSAFE_KW),
        Some(SyntaxKind::TRAIT_KW),
        bounds.as_ref().map(|_| SyntaxKind::COLON),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    if direct_token_kinds(declaration.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>()
        != expected_direct_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut expected_children = item_prefix_child_ranges(docs.as_ref(), &attrs);
    if let Some(visibility) = declaration.visibility() {
        expected_children.push(visibility.syntax().text_range());
    }
    expected_children.push(name.syntax().text_range());
    if let Some(generic_params) = generic_params.as_ref() {
        expected_children.push(generic_params.syntax().text_range());
    }
    if let Some(bounds) = bounds.as_ref() {
        expected_children.push(bounds.syntax().text_range());
    }
    if let Some(where_clause) = where_clause.as_ref() {
        expected_children.push(where_clause.syntax().text_range());
    }
    expected_children.push(items.syntax().text_range());
    if declaration
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(expected_children)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let item_body = item_list_body_document(
        source,
        &items,
        expression_group_ids,
        |child, expression_group_ids| match child.kind() {
            SyntaxKind::FN_DECL => function_document(
                source,
                &ast::FnDecl::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
                expression_group_ids,
            ),
            SyntaxKind::ASSOC_TYPE_DECL => assoc_type_decl_document(
                source,
                &ast::AssocTypeDecl::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
            ),
            _ => Err(SourceDocumentError::UnsupportedShape),
        },
    )?;

    let name = node_text(source, name.syntax());
    let mut head = vec![Doc::text(format!(
        "{visibility_prefix}{}trait {name}",
        if is_unsafe { "unsafe " } else { "" }
    ))];
    if let Some(generic_params) = generic_params.as_ref() {
        head.push(generic_params_document(source, generic_params)?);
    }
    if let Some(bounds) = bounds.as_ref() {
        head.extend([Doc::text(": "), bound_list_document(source, bounds)?]);
    }
    let head = Doc::concat(head);
    let document = if let Some(where_clause) = where_clause {
        Doc::concat([
            head,
            Doc::indent(Doc::concat([
                Doc::HardLine,
                where_clause_document(source, &where_clause)?,
            ])),
            Doc::HardLine,
            item_body,
        ])
    } else {
        Doc::concat([head, Doc::text(" "), item_body])
    };
    let document = Doc::concat([prefix, document]);
    let range = declaration.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn interface_document(
    source: &ParsedSource,
    declaration: &ast::InterfaceDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let visibility_prefix = visibility_prefix(declaration.visibility())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let items = declaration
        .items()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if !has_contextual_token(source, declaration.syntax(), "interface")?
        || declaration
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain(
                    declaration
                        .visibility()
                        .map(|visibility| visibility.syntax().text_range()),
                )
                .chain([name.syntax().text_range(), items.syntax().text_range()]))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let item_body = item_list_body_document(
        source,
        &items,
        expression_group_ids,
        |child, expression_group_ids| match child.kind() {
            SyntaxKind::FN_DECL => function_document(
                source,
                &ast::FnDecl::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
                expression_group_ids,
            ),
            SyntaxKind::IFACE_RESOURCE => iface_resource_document(
                source,
                &ast::IfaceResource::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
                expression_group_ids,
            ),
            SyntaxKind::TYPE_DECL => type_decl_document(
                source,
                &ast::TypeDecl::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
                expression_group_ids,
            ),
            SyntaxKind::ENUM_DECL => enum_decl_document(
                source,
                &ast::EnumDecl::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
            ),
            _ => Err(SourceDocumentError::UnsupportedShape),
        },
    )?;
    let document = Doc::concat([
        Doc::text(format!(
            "{visibility_prefix}interface {} ",
            node_text(source, name.syntax())
        )),
        item_body,
    ]);
    let document = Doc::concat([prefix, document]);
    let range = declaration.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn iface_resource_document(
    source: &ParsedSource,
    resource: &ast::IfaceResource,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = resource.docs();
    let attrs = resource.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let name = resource
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let items = resource.items();
    if !has_contextual_token(source, resource.syntax(), "resource")?
        || resource
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain(std::iter::once(name.syntax().text_range()))
                .chain(items.as_ref().map(|items| items.syntax().text_range())))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut document = vec![
        Doc::text("resource "),
        Doc::text(node_text(source, name.syntax())),
    ];
    if let Some(items) = items {
        document.extend([
            Doc::text(" "),
            item_list_body_document(
                source,
                &items,
                expression_group_ids,
                |child, expression_group_ids| match child.kind() {
                    SyntaxKind::FN_DECL => function_document(
                        source,
                        &ast::FnDecl::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
                        expression_group_ids,
                    ),
                    _ => Err(SourceDocumentError::UnsupportedShape),
                },
            )?,
        ]);
    }
    let document = Doc::concat(document);
    let document = Doc::concat([prefix, document]);
    let range = resource.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn type_decl_document(
    source: &ParsedSource,
    declaration: &ast::TypeDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let visibility_prefix = visibility_prefix(declaration.visibility())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let generic_params = declaration.generic_params();
    let struct_def = declaration.struct_def();
    let alias_def = declaration.alias_def();
    let Some(definition) = struct_def
        .as_ref()
        .map(|definition| definition.syntax())
        .or_else(|| alias_def.as_ref().map(|definition| definition.syntax()))
    else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    if struct_def.is_some() == alias_def.is_some()
        || direct_token_kinds(declaration.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::TYPE_KW]
        || declaration
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain(
                    declaration
                        .visibility()
                        .map(|visibility| visibility.syntax().text_range()),
                )
                .chain([name.syntax().text_range()])
                .chain(
                    generic_params
                        .as_ref()
                        .map(|params| params.syntax().text_range()),
                )
                .chain([definition.text_range()]))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = vec![Doc::text(format!(
        "{visibility_prefix}type {}",
        node_text(source, name.syntax())
    ))];
    if let Some(generic_params) = generic_params {
        documents.push(generic_params_document(source, &generic_params)?);
    }

    if let Some(alias) = alias_def {
        let ty = alias.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
        let refinement = alias.refinement();
        let expected_alias_children = std::iter::once(ty.syntax().text_range()).chain(
            refinement
                .as_ref()
                .map(|clause| clause.syntax().text_range()),
        );
        if alias
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(expected_alias_children)
            || direct_token_kinds(alias.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>()
                != [SyntaxKind::EQ]
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let rhs = type_document(source, &ty)?;
        let rhs = if let Some(refinement) = refinement {
            refined_right_hand_side_document(
                rhs,
                refinement_clause_document(source, &refinement, expression_group_ids)?,
            )
        } else {
            movable_right_hand_side_document(rhs, true, None)
        };
        documents.extend([Doc::text(" ="), rhs]);
    } else if let Some(struct_def) = struct_def {
        let where_clause = struct_def.where_clause();
        let fields = struct_def
            .fields()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let expected_struct_children = where_clause
            .as_ref()
            .map(|clause| clause.syntax().text_range())
            .into_iter()
            .chain([fields.syntax().text_range()]);
        if struct_def
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(expected_struct_children)
            || direct_token_kinds(struct_def.syntax())
                .into_iter()
                .any(|kind| kind != SyntaxKind::NEWLINE)
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        if let Some(where_clause) = where_clause {
            documents.push(Doc::indent(Doc::concat([
                Doc::HardLine,
                where_clause_document(source, &where_clause)?,
            ])));
            documents.push(Doc::HardLine);
        } else {
            documents.push(Doc::text(" "));
        }
        documents.push(record_fields_document(source, &fields)?);
    } else {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let document = Doc::concat([prefix, Doc::concat(documents)]);
    let range = declaration.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn distinct_decl_document(
    source: &ParsedSource,
    declaration: &ast::DistinctDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let visibility_prefix = visibility_prefix(declaration.visibility())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let generic_params = declaration.generic_params();
    let base_type = declaration
        .base_type()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let refinement = declaration.refinement();
    let inner_pub = declaration.inner_pub_token();
    let expected_children = item_prefix_child_ranges(docs.as_ref(), &attrs)
        .into_iter()
        .chain(
            declaration
                .visibility()
                .map(|visibility| visibility.syntax().text_range()),
        )
        .chain([name.syntax().text_range()])
        .chain(
            generic_params
                .as_ref()
                .map(|params| params.syntax().text_range()),
        )
        .chain([base_type.syntax().text_range()])
        .chain(
            refinement
                .as_ref()
                .map(|clause| clause.syntax().text_range()),
        );
    let direct_tokens = declaration
        .syntax()
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() != SyntaxKind::NEWLINE => Some(token),
            SyntaxElement::Token(_) | SyntaxElement::Node(_) => None,
        })
        .collect::<Vec<_>>();
    let Some(keyword) = direct_tokens.first() else {
        return Err(SourceDocumentError::UnsupportedShape);
    };
    if keyword.kind() != SyntaxKind::LIDENT || token_text(source, keyword)? != "distinct" {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut expected_tokens = vec![SyntaxKind::LIDENT, SyntaxKind::EQ];
    if inner_pub.is_some() {
        expected_tokens.push(SyntaxKind::PUB_KW);
    }
    if declaration
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(expected_children)
        || direct_tokens
            .iter()
            .map(|token| token.kind())
            .collect::<Vec<_>>()
            != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = vec![Doc::text(format!(
        "{visibility_prefix}distinct {}",
        node_text(source, name.syntax())
    ))];
    if let Some(generic_params) = generic_params {
        documents.push(generic_params_document(source, &generic_params)?);
    }
    let mut rhs = Vec::new();
    if inner_pub.is_some() {
        rhs.push(Doc::text("pub "));
    }
    rhs.push(type_document(source, &base_type)?);
    let rhs = Doc::concat(rhs);
    let rhs = if let Some(refinement) = refinement {
        refined_right_hand_side_document(
            rhs,
            refinement_clause_document(source, &refinement, expression_group_ids)?,
        )
    } else {
        movable_right_hand_side_document(rhs, true, None)
    };
    documents.extend([Doc::text(" ="), rhs]);

    let document = Doc::concat([prefix, Doc::concat(documents)]);
    let range = declaration.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn resource_decl_document(
    source: &ParsedSource,
    resource: &ast::ResourceDecl,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = resource.docs();
    let attrs = resource.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let visibility_prefix = visibility_prefix(resource.visibility())?;
    let name = resource
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let fields = resource.fields();
    if !has_contextual_token(source, resource.syntax(), "resource")?
        || resource
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain(
                    resource
                        .visibility()
                        .map(|visibility| visibility.syntax().text_range()),
                )
                .chain([name.syntax().text_range()])
                .chain(fields.as_ref().map(|fields| fields.syntax().text_range())))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = vec![Doc::text(format!(
        "{visibility_prefix}resource {}",
        node_text(source, name.syntax())
    ))];
    if let Some(fields) = fields {
        documents.extend([Doc::text(" "), record_fields_document(source, &fields)?]);
    }
    let document = Doc::concat([prefix, Doc::concat(documents)]);
    let range = resource.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn refinement_clause_document(
    source: &ParsedSource,
    refinement: &ast::RefinementClause,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let expression = refinement
        .expression()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if refinement
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne([expression.syntax().text_range()])
        || direct_token_kinds(refinement.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::WHERE_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text("where "),
        expression_document(
            source,
            &expression,
            ExpressionContext::Unprotected,
            expression_group_ids,
        )?,
    ]))
}

fn record_fields_document(
    source: &ParsedSource,
    fields: &ast::FieldList,
) -> Result<Doc, SourceDocumentError> {
    let declarations = fields.fields().collect::<Vec<_>>();
    let declaration_nodes = declarations
        .iter()
        .map(|declaration| declaration.syntax().clone())
        .collect::<Vec<_>>();
    let tokens = direct_token_kinds(fields.syntax());
    if tokens.len() < 2
        || tokens.first() != Some(&SyntaxKind::L_BRACE)
        || tokens.last() != Some(&SyntaxKind::R_BRACE)
        || tokens[1..tokens.len() - 1]
            .iter()
            .any(|kind| !matches!(kind, SyntaxKind::NEWLINE | SyntaxKind::COMMA))
        || fields
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(declarations
                .iter()
                .map(|declaration| declaration.syntax().text_range()))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let comments = collect_list_comments(fields.syntax(), &declaration_nodes)?;
    let documents = declarations
        .iter()
        .map(|declaration| record_field_document(source, declaration))
        .collect::<Result<Vec<_>, _>>()?;
    let members = declarations
        .iter()
        .zip(documents)
        .enumerate()
        .map(|(index, (declaration, document))| {
            let range = declaration.syntax().text_range();
            decorate_list_member_document(
                source,
                &comments.before[index],
                &comments.after[index],
                range.start().to_usize(),
                range.end().to_usize(),
                document,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    broken_brace_list_document(
        source,
        members,
        &comments.opening_leading,
        &comments.opening_trailing,
        &comments.closing,
    )
}

fn broken_brace_list_document(
    source: &ParsedSource,
    members: Vec<(usize, usize, Doc)>,
    opening_leading_comments: &[SourceComment],
    opening_trailing_comments: &[SourceComment],
    closing_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let mut document = Vec::new();
    if !opening_leading_comments.is_empty() {
        document.push(Doc::HardLine);
        append_comment_documents(&mut document, opening_leading_comments, source.text(), None)?;
        let opening_start = opening_leading_comments[0].owner_token_range.start;
        append_comment_gap(
            &mut document,
            source.text(),
            opening_leading_comments
                .last()
                .map(|comment| comment.range.end),
            Some(opening_start),
        )?;
    }
    if members.is_empty() && opening_trailing_comments.is_empty() && closing_comments.is_empty() {
        document.push(Doc::text("{}"));
        return Ok(Doc::concat(document));
    }
    let mut body = Vec::new();
    append_trailing_comment_documents(&mut body, opening_trailing_comments);
    if !members.is_empty() || !closing_comments.is_empty() {
        body.push(Doc::HardLine);
    }
    for (index, (start, _, document)) in members.iter().enumerate() {
        if index > 0 {
            let (_, previous_end, _) = &members[index - 1];
            let gap = source
                .text()
                .get(*previous_end..*start)
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            body.push(Doc::HardLine);
            if gap.matches('\n').count() > 1 {
                body.push(Doc::HardLine);
            }
        }
        body.push(document.clone());
    }
    if !closing_comments.is_empty() {
        if let Some((_, previous_end, _)) = members.last() {
            let gap = source
                .text()
                .get(*previous_end..closing_comments[0].range.start)
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            body.push(Doc::HardLine);
            if gap.matches('\n').count() > 1 {
                body.push(Doc::HardLine);
            }
        }
        append_comment_documents(&mut body, closing_comments, source.text(), None)?;
    }
    document.extend([
        Doc::text("{"),
        Doc::indent(Doc::concat(body)),
        Doc::HardLine,
        Doc::text("}"),
    ]);
    Ok(Doc::concat(document))
}

fn decorate_list_member_document(
    source: &ParsedSource,
    leading_comments: &[SourceComment],
    trailing_comments: &[SourceComment],
    start: usize,
    end: usize,
    document: Doc,
) -> Result<(usize, usize, Doc), SourceDocumentError> {
    if leading_comments.is_empty() && trailing_comments.is_empty() {
        return Ok((start, end, document));
    }
    let mut documents = Vec::new();
    let mut output_start = start;
    if !leading_comments.is_empty() {
        append_comment_documents(&mut documents, leading_comments, source.text(), None)?;
        append_comment_gap(
            &mut documents,
            source.text(),
            leading_comments.last().map(|comment| comment.range.end),
            Some(start),
        )?;
        output_start = leading_comments[0].range.start;
    }
    documents.push(document);
    append_trailing_comment_documents(&mut documents, trailing_comments);
    let output_end = trailing_comments
        .last()
        .map_or(end, |comment| comment.range.end);
    Ok((output_start, output_end, Doc::concat(documents)))
}

fn append_trailing_comment_documents(document: &mut Vec<Doc>, comments: &[SourceComment]) {
    let mut trailing_comment_requires_line_break = false;
    for comment in comments {
        if trailing_comment_requires_line_break {
            document.push(Doc::HardLine);
        }
        if comment.kind == TriviaKind::LineComment && !trailing_comment_requires_line_break {
            document.push(Doc::line_suffix(Doc::text(format!(" {}", comment.text))));
        } else {
            if !trailing_comment_requires_line_break {
                document.push(Doc::text(" "));
            }
            document.push(Doc::verbatim(comment.text.clone()));
        }
        trailing_comment_requires_line_break = comment.kind == TriviaKind::LineComment
            || (comment.kind == TriviaKind::BlockComment && comment.text.contains('\n'));
    }
}

fn append_comments_between_tokens(document: &mut Vec<Doc>, comments: &[SourceComment]) -> bool {
    let mut previous_comment_requires_break = false;
    for comment in comments {
        if previous_comment_requires_break {
            document.push(Doc::HardLine);
        }
        if comment.kind == TriviaKind::LineComment {
            document.push(Doc::line_suffix(Doc::text(format!(" {}", comment.text))));
        } else {
            if !previous_comment_requires_break {
                document.push(Doc::text(" "));
            }
            document.push(Doc::verbatim(comment.text.clone()));
        }
        previous_comment_requires_break = comment.kind == TriviaKind::LineComment
            || (comment.kind == TriviaKind::BlockComment && comment.text.contains('\n'));
    }
    if previous_comment_requires_break {
        document.push(Doc::HardLine);
    }
    previous_comment_requires_break
}

fn record_field_document(
    source: &ParsedSource,
    field: &ast::FieldDecl,
) -> Result<Doc, SourceDocumentError> {
    let docs = field.docs();
    let attrs = field.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let name = field.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let ty = field.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
    let tokens = direct_token_kinds(field.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if field
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
            .into_iter()
            .chain([name.syntax().text_range(), ty.syntax().text_range()]))
        || (tokens != [SyntaxKind::COLON] && tokens != [SyntaxKind::PUB_KW, SyntaxKind::COLON])
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(4);
    if tokens.first() == Some(&SyntaxKind::PUB_KW) {
        documents.push(Doc::text("pub "));
    }
    documents.extend([
        Doc::text(node_text(source, name.syntax())),
        Doc::text(": "),
        type_document(source, &ty)?,
    ]);
    Ok(Doc::concat([prefix, Doc::concat(documents)]))
}

fn enum_decl_document(
    source: &ParsedSource,
    declaration: &ast::EnumDecl,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let visibility_prefix = visibility_prefix(declaration.visibility())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let generic_params = declaration.generic_params();
    let where_clause = declaration.where_clause();
    let variants = declaration
        .variants()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let expected_children = item_prefix_child_ranges(docs.as_ref(), &attrs)
        .into_iter()
        .chain(
            declaration
                .visibility()
                .map(|visibility| visibility.syntax().text_range()),
        )
        .chain([name.syntax().text_range()])
        .chain(
            generic_params
                .as_ref()
                .map(|params| params.syntax().text_range()),
        )
        .chain(
            where_clause
                .as_ref()
                .map(|clause| clause.syntax().text_range()),
        )
        .chain([variants.syntax().text_range()]);
    if declaration
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(expected_children)
        || direct_token_kinds(declaration.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::ENUM_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let declarations = variants.variants().collect::<Vec<_>>();
    let tokens = direct_token_kinds(variants.syntax());
    if tokens.len() < 2
        || tokens.first() != Some(&SyntaxKind::L_BRACE)
        || tokens.last() != Some(&SyntaxKind::R_BRACE)
        || tokens[1..tokens.len() - 1]
            .iter()
            .any(|kind| !matches!(kind, SyntaxKind::NEWLINE | SyntaxKind::COMMA))
        || variants
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(declarations
                .iter()
                .map(|variant| variant.syntax().text_range()))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let declaration_nodes = declarations
        .iter()
        .map(|declaration| declaration.syntax().clone())
        .collect::<Vec<_>>();
    let comments = collect_list_comments(variants.syntax(), &declaration_nodes)?;
    let documents = declarations
        .iter()
        .map(|variant| enum_variant_document(source, variant))
        .collect::<Result<Vec<_>, _>>()?;
    let members = declarations
        .iter()
        .zip(documents)
        .enumerate()
        .map(|(index, (variant, document))| {
            let range = variant.syntax().text_range();
            decorate_list_member_document(
                source,
                &comments.before[index],
                &comments.after[index],
                range.start().to_usize(),
                range.end().to_usize(),
                document,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let variants = broken_brace_list_document(
        source,
        members,
        &comments.opening_leading,
        &comments.opening_trailing,
        &comments.closing,
    )?;

    let mut documents = vec![Doc::text(format!(
        "{visibility_prefix}enum {}",
        node_text(source, name.syntax())
    ))];
    if let Some(generic_params) = generic_params {
        documents.push(generic_params_document(source, &generic_params)?);
    }
    if let Some(where_clause) = where_clause {
        documents.push(Doc::indent(Doc::concat([
            Doc::HardLine,
            where_clause_document(source, &where_clause)?,
        ])));
        documents.push(Doc::HardLine);
    } else {
        documents.push(Doc::text(" "));
    }
    documents.push(variants);
    let document = Doc::concat([prefix, Doc::concat(documents)]);
    let range = declaration.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn enum_variant_document(
    source: &ParsedSource,
    variant: &ast::VariantDecl,
) -> Result<Doc, SourceDocumentError> {
    let docs = variant.docs();
    let attrs = variant.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let name = variant
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let tuple_shape = variant.tuple_shape();
    let record_shape = variant.record_shape();
    let discriminant = variant.discriminant();
    let shape = match (&tuple_shape, &record_shape) {
        (Some(tuple), None) => Some(tuple.syntax().text_range()),
        (None, Some(record)) => Some(record.syntax().text_range()),
        (None, None) => None,
        (Some(_), Some(_)) => return Err(SourceDocumentError::UnsupportedShape),
    };
    let expected_children = item_prefix_child_ranges(docs.as_ref(), &attrs)
        .into_iter()
        .chain(std::iter::once(name.syntax().text_range()))
        .chain(shape)
        .chain(
            discriminant
                .as_ref()
                .map(|value| value.syntax().text_range()),
        );
    let expected_tokens = Vec::new();
    if variant
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(expected_children)
        || direct_token_kinds(variant.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = vec![Doc::text(node_text(source, name.syntax()))];
    if let Some(tuple_shape) = tuple_shape {
        documents.push(tuple_variant_shape_document(source, &tuple_shape)?);
    }
    if let Some(record_shape) = record_shape {
        documents.push(record_variant_shape_document(source, &record_shape)?);
    }
    if let Some(discriminant) = discriminant {
        documents.push(discriminant_document(source, &discriminant)?);
    }
    Ok(Doc::concat([prefix, Doc::concat(documents)]))
}

fn tuple_variant_shape_document(
    source: &ParsedSource,
    shape: &ast::TupleShape,
) -> Result<Doc, SourceDocumentError> {
    let types = shape.types().collect::<Vec<_>>();
    if types.is_empty()
        || shape.syntax().children().count() != types.len()
        || !comma_delimited_tokens_match(
            shape.syntax(),
            SyntaxKind::L_PAREN,
            SyntaxKind::R_PAREN,
            types.len(),
        )
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = types
        .iter()
        .map(|ty| type_document(source, ty))
        .collect::<Result<Vec<_>, _>>()?;
    let range = shape.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "(",
        ")",
        documents,
        false,
    ))
}

fn record_variant_shape_document(
    source: &ParsedSource,
    shape: &ast::RecordShape,
) -> Result<Doc, SourceDocumentError> {
    let fields = shape.fields().collect::<Vec<_>>();
    if fields.is_empty()
        || shape.syntax().children().count() != fields.len()
        || !comma_delimited_tokens_match(
            shape.syntax(),
            SyntaxKind::L_PAREN,
            SyntaxKind::R_PAREN,
            fields.len(),
        )
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = fields
        .iter()
        .map(|field| variant_field_document(source, field))
        .collect::<Result<Vec<_>, _>>()?;
    let range = shape.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "(",
        ")",
        documents,
        false,
    ))
}

fn variant_field_document(
    source: &ParsedSource,
    field: &ast::VariantField,
) -> Result<Doc, SourceDocumentError> {
    if field.attrs().next().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let name = field.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let ty = field.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
    if field
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne([name.syntax().text_range(), ty.syntax().text_range()])
        || direct_token_kinds(field.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::COLON]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text(node_text(source, name.syntax())),
        Doc::text(": "),
        type_document(source, &ty)?,
    ]))
}

fn discriminant_document(
    source: &ParsedSource,
    discriminant: &ast::Discriminant,
) -> Result<Doc, SourceDocumentError> {
    let integer = discriminant
        .integer()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let minus = discriminant.minus_token();
    let mut expected_tokens = vec![SyntaxKind::EQ];
    if minus.is_some() {
        expected_tokens.push(SyntaxKind::MINUS);
    }
    expected_tokens.push(SyntaxKind::INT);
    if discriminant.syntax().children().next().is_some()
        || direct_token_kinds(discriminant.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let integer = normalize_integer_literal(token_text(source, integer.syntax())?);
    Ok(Doc::concat([
        Doc::text(" = "),
        Doc::text(if minus.is_some() { "-" } else { "" }),
        Doc::text(integer),
    ]))
}

fn item_list_body_document(
    source: &ParsedSource,
    items: &ast::ItemList,
    expression_group_ids: &mut ExpressionGroupIds,
    mut render_member: impl FnMut(
        sai_syntax::SyntaxNode,
        &mut ExpressionGroupIds,
    ) -> Result<ItemDocument, SourceDocumentError>,
) -> Result<Doc, SourceDocumentError> {
    let mut members = Vec::with_capacity(items.syntax().children().count());
    for child in items.syntax().children() {
        members.push(render_member(child, expression_group_ids)?);
    }
    let item_list_tokens = direct_token_kinds(items.syntax());
    if item_list_tokens.len() < 2
        || item_list_tokens.first() != Some(&SyntaxKind::L_BRACE)
        || item_list_tokens.last() != Some(&SyntaxKind::R_BRACE)
        || item_list_tokens[1..item_list_tokens.len() - 1]
            .iter()
            .any(|kind| *kind != SyntaxKind::NEWLINE)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if members.is_empty() {
        return Ok(Doc::text("{}"));
    }

    let mut body = vec![Doc::HardLine];
    for (index, member) in members.iter().enumerate() {
        if index > 0 {
            let previous = &members[index - 1];
            let gap = source
                .text()
                .get(previous.end..member.start)
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            body.push(Doc::HardLine);
            if gap.matches('\n').count() > 1 {
                body.push(Doc::HardLine);
            }
        }
        body.push(member.document.clone());
    }
    Ok(Doc::concat([
        Doc::text("{"),
        Doc::indent(Doc::concat(body)),
        Doc::HardLine,
        Doc::text("}"),
    ]))
}

fn significant_token_kinds(node: &sai_syntax::SyntaxNode) -> Vec<SyntaxKind> {
    direct_token_kinds(node)
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect()
}

fn item_document_for_node(
    document: Doc,
    node: &sai_syntax::SyntaxNode,
) -> Result<ItemDocument, SourceDocumentError> {
    let range = node.text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn const_decl_document(
    source: &ParsedSource,
    declaration: &ast::ConstDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let visibility_prefix = visibility_prefix(declaration.visibility())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let ty = declaration
        .ty()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let initializer = declaration
        .initializer()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if significant_token_kinds(declaration.syntax())
        != [SyntaxKind::CONST_KW, SyntaxKind::COLON, SyntaxKind::EQ]
        || declaration
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain(
                    declaration
                        .visibility()
                        .map(|visibility| visibility.syntax().text_range()),
                )
                .chain([
                    name.syntax().text_range(),
                    ty.syntax().text_range(),
                    initializer.syntax().text_range(),
                ]))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let value = right_hand_side_document(
        source,
        &initializer,
        ExpressionContext::Unprotected,
        expression_group_ids,
        true,
    )?;
    let document = Doc::concat([
        prefix,
        Doc::text(format!(
            "{visibility_prefix}const {}:",
            node_text(source, name.syntax())
        )),
        Doc::text(" "),
        type_document(source, &ty)?,
        Doc::text(" ="),
        value,
    ]);
    item_document_for_node(document, declaration.syntax())
}

fn invariant_decl_document(
    source: &ParsedSource,
    declaration: &ast::InvariantDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let predicates = declaration
        .predicates()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let expressions = predicates.expressions().collect::<Vec<_>>();
    let list_tokens = significant_token_kinds(predicates.syntax());
    let separators_only = list_tokens
        .get(1..list_tokens.len().saturating_sub(1))
        .is_some_and(|middle| middle.iter().all(|kind| *kind == SyntaxKind::COMMA));
    if !has_contextual_token(source, declaration.syntax(), "invariant")?
        || declaration
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain([name.syntax().text_range(), predicates.syntax().text_range()]))
        || expressions.is_empty()
        || predicates.syntax().children().count() != expressions.len()
        || list_tokens.first() != Some(&SyntaxKind::L_BRACE)
        || list_tokens.last() != Some(&SyntaxKind::R_BRACE)
        || !separators_only
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let group = GroupId(predicates.syntax().text_range().start().raw());
    let last_index = expressions.len() - 1;
    let mut predicate_documents = Vec::with_capacity(expressions.len());
    for (index, expression) in expressions.iter().enumerate() {
        let document = expression_document(
            source,
            expression,
            ExpressionContext::Unprotected,
            expression_group_ids,
        )?;
        // In a broken list, an open-ended range would not be ended by its line break
        // (§14.5.4 rule 5), so it is parenthesized only then.
        predicate_documents.push(
            if index < last_index && is_open_ended_range_expression(expression) {
                Doc::if_broken(
                    group,
                    Doc::concat([Doc::text("("), document.clone(), Doc::text(")")]),
                    document,
                )
            } else {
                document
            },
        );
    }
    let document = Doc::concat([
        prefix,
        Doc::text(format!("invariant {} ", node_text(source, name.syntax()))),
        brace_list_document(group, predicate_documents),
    ]);
    item_document_for_node(document, declaration.syntax())
}

fn effect_decl_document(
    source: &ParsedSource,
    declaration: &ast::EffectDecl,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let visibility_prefix = visibility_prefix(declaration.visibility())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let covers = declaration.covers();
    if !has_contextual_token(source, declaration.syntax(), "effect")?
        || declaration
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain(
                    declaration
                        .visibility()
                        .map(|visibility| visibility.syntax().text_range()),
                )
                .chain([name.syntax().text_range()])
                .chain(covers.as_ref().map(|covers| covers.syntax().text_range())))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text(format!(
        "{visibility_prefix}effect {}",
        node_text(source, name.syntax())
    ))];
    if let Some(covers) = covers {
        let set = covers.set().ok_or(SourceDocumentError::UnsupportedShape)?;
        if covers.syntax().children().count() != 1
            || !has_contextual_token(source, covers.syntax(), "covers")?
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        documents.extend([Doc::text(" covers "), effect_set_document(source, &set)?]);
    }
    let document = Doc::concat([prefix, Doc::concat(documents)]);
    item_document_for_node(document, declaration.syntax())
}

fn world_decl_document(
    source: &ParsedSource,
    declaration: &ast::WorldDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let visibility_prefix = visibility_prefix(declaration.visibility())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let items = declaration
        .items()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if !has_contextual_token(source, declaration.syntax(), "world")?
        || declaration
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain(
                    declaration
                        .visibility()
                        .map(|visibility| visibility.syntax().text_range()),
                )
                .chain([name.syntax().text_range(), items.syntax().text_range()]))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let item_body = item_list_body_document(
        source,
        &items,
        expression_group_ids,
        |child, expression_group_ids| world_item_document(source, child, expression_group_ids),
    )?;
    let document = Doc::concat([
        prefix,
        Doc::text(format!(
            "{visibility_prefix}world {} ",
            node_text(source, name.syntax())
        )),
        item_body,
    ]);
    item_document_for_node(document, declaration.syntax())
}

fn world_item_document(
    source: &ParsedSource,
    item: sai_syntax::SyntaxNode,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let (keyword, allows_function) = match item.kind() {
        SyntaxKind::WORLD_IMPORT => ("import", true),
        SyntaxKind::WORLD_EXPORT => ("export", true),
        SyntaxKind::WORLD_INCLUDE => ("include", false),
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    let keyword_matches = match keyword {
        "export" => significant_token_kinds(&item) == [SyntaxKind::EXPORT_KW],
        _ => has_contextual_token(source, &item, keyword)?,
    };
    let docs = item.children().find_map(ast::Docs::cast);
    let attrs = item
        .children()
        .filter_map(ast::Attribute::cast)
        .collect::<Vec<_>>();
    let function = item.children().find_map(ast::FnDecl::cast);
    let reference = item.children().find_map(ast::WorldRef::cast);
    let (body_range, body_document) = match (function, reference) {
        (Some(function), None) if allows_function => {
            let range = function.syntax().text_range();
            (
                range,
                function_document(source, &function, expression_group_ids)?.document,
            )
        }
        (None, Some(reference)) => (
            reference.syntax().text_range(),
            world_ref_document(source, &reference)?,
        ),
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    if !keyword_matches
        || item
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain([body_range]))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let prefix = item_prefix_document(source, docs, attrs)?;
    let document = Doc::concat([prefix, Doc::text(format!("{keyword} ")), body_document]);
    item_document_for_node(document, &item)
}

fn extern_block_document(
    source: &ParsedSource,
    declaration: &ast::ExternBlock,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let items = declaration
        .items()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let abi = declaration
        .abi()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let module = declaration.module();
    let unsafe_prefix = match significant_token_kinds(declaration.syntax()).as_slice() {
        [
            SyntaxKind::UNSAFE_KW,
            SyntaxKind::EXTERN_KW,
            SyntaxKind::STRING,
        ] => "unsafe ",
        [SyntaxKind::EXTERN_KW, SyntaxKind::STRING] => "",
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    if declaration
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
            .into_iter()
            .chain(module.as_ref().map(|module| module.syntax().text_range()))
            .chain([items.syntax().text_range()]))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let module_document = module
        .as_ref()
        .map(|module| extern_module_document(source, module))
        .transpose()?
        .unwrap_or_else(|| Doc::text(""));
    let item_body = item_list_body_document(
        source,
        &items,
        expression_group_ids,
        |child, expression_group_ids| match child.kind() {
            SyntaxKind::FN_DECL => function_document(
                source,
                &ast::FnDecl::cast(child).ok_or(SourceDocumentError::UnsupportedShape)?,
                expression_group_ids,
            ),
            _ => Err(SourceDocumentError::UnsupportedShape),
        },
    )?;
    let document = Doc::concat([
        prefix,
        Doc::text(format!("{unsafe_prefix}extern ")),
        literal_document(source, LiteralToken::String(abi))?,
        module_document,
        Doc::text(" "),
        item_body,
    ]);
    item_document_for_node(document, declaration.syntax())
}

fn extern_module_document(
    source: &ParsedSource,
    module: &ast::ExternModule,
) -> Result<Doc, SourceDocumentError> {
    let value = module
        .value()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if significant_token_kinds(module.syntax()) != [SyntaxKind::LIDENT, SyntaxKind::STRING]
        || module.syntax().children().count() != 0
        || !module.syntax().children_with_tokens().any(|element| {
            matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::LIDENT && token.text() == "module")
        })
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text(" module "),
        literal_document(source, LiteralToken::String(value))?,
    ]))
}

fn test_decl_document(
    source: &ParsedSource,
    declaration: &ast::TestDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let body = declaration
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if significant_token_kinds(declaration.syntax()) != [SyntaxKind::LIDENT, SyntaxKind::STRING]
        || !has_leading_contextual_token(declaration.syntax(), "test")
        || declaration
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain([body.syntax().text_range()]))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let name_document = literal_document(source, LiteralToken::String(name))?;
    let body_document = block_document(source, &body, expression_group_ids)?;
    let document = Doc::concat([
        prefix,
        Doc::text("test "),
        name_document,
        Doc::text(" "),
        body_document,
    ]);
    item_document_for_node(document, declaration.syntax())
}

fn property_decl_document(
    source: &ParsedSource,
    declaration: &ast::PropertyDecl,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let params = declaration
        .params()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let body = declaration
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if significant_token_kinds(declaration.syntax()) != [SyntaxKind::LIDENT, SyntaxKind::STRING]
        || !has_leading_contextual_token(declaration.syntax(), "property")
        || declaration
            .syntax()
            .children()
            .map(|child| child.text_range())
            .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
                .into_iter()
                .chain([params.syntax().text_range(), body.syntax().text_range()]))
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let name_document = literal_document(source, LiteralToken::String(name))?;
    let params_document = parameter_list_document(source, &params)?;
    let body_document = block_document(source, &body, expression_group_ids)?;
    let document = Doc::concat([
        prefix,
        Doc::text("property "),
        name_document,
        Doc::text(" "),
        params_document,
        Doc::text(" "),
        body_document,
    ]);
    item_document_for_node(document, declaration.syntax())
}

fn has_leading_contextual_token(node: &sai_syntax::SyntaxNode, spelling: &str) -> bool {
    node.children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() != SyntaxKind::NEWLINE => Some(token),
            SyntaxElement::Token(_) | SyntaxElement::Node(_) => None,
        })
        .is_some_and(|token| token.kind() == SyntaxKind::LIDENT && token.text() == spelling)
}

fn world_ref_document(
    source: &ParsedSource,
    reference: &ast::WorldRef,
) -> Result<Doc, SourceDocumentError> {
    let child_count = reference.syntax().children().count();
    match (
        significant_token_kinds(reference.syntax()).as_slice(),
        reference.mod_path(),
    ) {
        ([SyntaxKind::PKG_PATH], None) if child_count == 0 => {
            Ok(Doc::text(node_text(source, reference.syntax())))
        }
        ([], Some(path)) if child_count == 1 => mod_path_document(source, &path),
        _ => Err(SourceDocumentError::UnsupportedShape),
    }
}

fn assoc_type_decl_document(
    source: &ParsedSource,
    declaration: &ast::AssocTypeDecl,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = declaration.docs();
    let attrs = declaration.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let name = declaration
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let bounds = declaration.bounds();
    if declaration
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
            .into_iter()
            .chain(std::iter::once(name.syntax().text_range()))
            .chain(bounds.as_ref().map(|bounds| bounds.syntax().text_range())))
        || direct_token_kinds(declaration.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [
                Some(SyntaxKind::TYPE_KW),
                bounds.as_ref().map(|_| SyntaxKind::COLON),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut document = vec![
        Doc::text("type "),
        Doc::text(node_text(source, name.syntax())),
    ];
    if let Some(bounds) = bounds {
        document.extend([Doc::text(": "), bound_list_document(source, &bounds)?]);
    }
    let document = Doc::concat([prefix, Doc::concat(document)]);
    let range = declaration.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn assoc_type_def_document(
    source: &ParsedSource,
    definition: &ast::AssocTypeDef,
) -> Result<ItemDocument, SourceDocumentError> {
    let docs = definition.docs();
    let attrs = definition.attrs().collect::<Vec<_>>();
    let prefix = item_prefix_document(source, docs.clone(), attrs.iter().cloned())?;
    let name = definition
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let ty = definition
        .ty()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if definition
        .syntax()
        .children()
        .map(|child| child.text_range())
        .ne(item_prefix_child_ranges(docs.as_ref(), &attrs)
            .into_iter()
            .chain([name.syntax().text_range(), ty.syntax().text_range()]))
        || direct_token_kinds(definition.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::TYPE_KW, SyntaxKind::EQ]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let document = Doc::concat([
        prefix,
        Doc::text("type "),
        Doc::text(node_text(source, name.syntax())),
        Doc::text(" = "),
        type_document(source, &ty)?,
    ]);
    let range = definition.syntax().text_range();
    let multiline = crate::print(&document)
        .map_err(|_| SourceDocumentError::UnsupportedShape)?
        .trim_end_matches('\n')
        .contains('\n');
    Ok(ItemDocument {
        document,
        start: range.start().to_usize(),
        end: range.end().to_usize(),
        multiline,
    })
}

fn mod_path_document(
    source: &ParsedSource,
    path: &ast::ModPath,
) -> Result<Doc, SourceDocumentError> {
    let segments = path.segments().collect::<Vec<_>>();
    let elements = path
        .syntax()
        .children_with_tokens()
        .filter(|element| {
            !matches!(
                element,
                SyntaxElement::Token(token) if token.kind() == SyntaxKind::NEWLINE
            )
        })
        .collect::<Vec<_>>();
    if segments.is_empty()
        || path.syntax().children().count() != segments.len()
        || elements.len() != segments.len() * 2 - 1
        || segments.iter().enumerate().any(|(index, segment)| {
            !matches!(
                elements.get(index * 2),
                Some(SyntaxElement::Node(node))
                    if node.text_range() == segment.syntax().text_range()
            ) || (index + 1 < segments.len()
                && !matches!(
                    elements.get(index * 2 + 1),
                    Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::DOT
                ))
        })
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = Vec::with_capacity(segments.len() * 2 - 1);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            documents.push(Doc::text("."));
        }
        documents.push(Doc::text(node_text(source, segment.syntax())));
    }
    Ok(Doc::concat(documents))
}

fn block_document(
    source: &ParsedSource,
    block: &ast::Block,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let statements = block.statements().collect::<Vec<_>>();
    if block.syntax().children().count() != statements.len()
        || direct_token_kinds(block.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::L_BRACE, SyntaxKind::R_BRACE]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let statement_nodes = statements
        .iter()
        .map(|statement| statement.syntax().clone())
        .collect::<Vec<_>>();
    let comments = collect_block_comments(block.syntax(), &statement_nodes)?;
    if statements.is_empty() && !comments.has_comments() {
        return Ok(Doc::text("{}"));
    }

    let documents = statements
        .iter()
        .enumerate()
        .map(|(index, statement)| {
            statement_document(
                source,
                statement,
                &comments.after_keyword[index],
                &comments.after_expression_keyword[index],
                &comments.after_type_colon[index],
                &comments.after_operator[index],
                expression_group_ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let members = statements
        .iter()
        .zip(documents)
        .enumerate()
        .map(|(index, (statement, document))| {
            let range = statement.syntax().text_range();
            decorate_list_member_document(
                source,
                &comments.before[index],
                &comments.after[index],
                range.start().to_usize(),
                range.end().to_usize(),
                document,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    broken_brace_list_document(
        source,
        members,
        &comments.opening_leading,
        &comments.opening_trailing,
        &comments.closing,
    )
}

fn block_expression_document(
    source: &ParsedSource,
    block_expr: &ast::BlockExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let block = block_expr
        .block()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if block_expr.syntax().children().count() != 1
        || !direct_token_kinds(block_expr.syntax()).is_empty()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    block_document(source, &block, expression_group_ids)
}

fn if_expression_document(
    source: &ParsedSource,
    if_expr: &ast::IfExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    if_expression_document_with_keyword_comments(source, if_expr, expression_group_ids, &[])
}

fn if_expression_document_with_keyword_comments(
    source: &ParsedSource,
    if_expr: &ast::IfExpr,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let condition = if_expr
        .condition()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let then_branch = if_expr
        .then_branch()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let else_branch = if_expr.else_branch();
    let if_tokens = direct_token_kinds(if_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if if_expr.syntax().children().count() != 2 + usize::from(else_branch.is_some())
        || if_tokens != [SyntaxKind::IF_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = vec![Doc::text("if")];
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    documents.extend([
        Doc::text(" "),
        condition_document(source, &condition, expression_group_ids)?,
        Doc::text(" "),
        block_document(source, &then_branch, expression_group_ids)?,
    ]);
    if let Some(else_branch) = else_branch {
        if else_branch.syntax().children().count() != 1
            || direct_token_kinds(else_branch.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>()
                != [SyntaxKind::ELSE_KW]
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let block = else_branch.block();
        let else_if = else_branch.else_if();
        if block.is_some() == else_if.is_some() {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        documents.push(Doc::text(" else"));
        let else_keyword_comments =
            trailing_comments_after_direct_token(else_branch.syntax(), SyntaxKind::ELSE_KW)?;
        append_single_line_block_comments(&mut documents, &else_keyword_comments)?;
        documents.push(Doc::text(" "));
        if let Some(block) = block {
            documents.push(block_document(source, &block, expression_group_ids)?);
        } else if let Some(else_if) = else_if {
            let keyword_comments = else_if_keyword_comments(&else_if)?;
            documents.push(if_expression_document_with_keyword_comments(
                source,
                &else_if,
                expression_group_ids,
                &keyword_comments,
            )?);
        }
    }
    Ok(Doc::concat(documents))
}

fn else_if_keyword_comments(
    if_expr: &ast::IfExpr,
) -> Result<Vec<SourceComment>, SourceDocumentError> {
    trailing_comments_after_direct_token(if_expr.syntax(), SyntaxKind::IF_KW)
}

fn trailing_comments_after_direct_token(
    node: &SyntaxNode,
    kind: SyntaxKind,
) -> Result<Vec<SourceComment>, SourceDocumentError> {
    let keyword = node
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == kind => Some(token),
            _ => None,
        })
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    trailing_comments_after_token(&keyword)
}

fn trailing_comments_after_token(
    token: &SyntaxToken,
) -> Result<Vec<SourceComment>, SourceDocumentError> {
    let comments = collect_comments_from_tokens(std::slice::from_ref(token))?;
    Ok(comments
        .into_iter()
        .filter(|comment| !comment.is_leading)
        .collect())
}

fn leading_comments_before_token(
    token: &SyntaxToken,
) -> Result<Vec<SourceComment>, SourceDocumentError> {
    let comments = collect_comments_from_tokens(std::slice::from_ref(token))?;
    Ok(comments
        .into_iter()
        .filter(|comment| comment.is_leading)
        .collect())
}

fn match_expression_document(
    source: &ParsedSource,
    match_expr: &ast::MatchExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    match_expression_document_with_keyword_comments(source, match_expr, expression_group_ids, &[])
}

fn match_expression_document_with_keyword_comments(
    source: &ParsedSource,
    match_expr: &ast::MatchExpr,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let scrutinee = match_expr
        .scrutinee()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let arms = match_expr
        .arms()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let match_children = match_expr.syntax().children().collect::<Vec<_>>();
    if match_expr.syntax().children().count() != 2
        || match_children[0].text_range() != scrutinee.syntax().text_range()
        || match_children[1].text_range() != arms.syntax().text_range()
        || direct_token_kinds(match_expr.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::MATCH_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let match_arms = arms.arms().collect::<Vec<_>>();
    if arms.syntax().children().count() != match_arms.len()
        || !match_arm_list_elements_match(&arms, &match_arms)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let list_group = GroupId(arms.syntax().text_range().start().raw());
    let scrutinee_comments = trailing_comments_after_token(
        &scrutinee
            .syntax()
            .last_token()
            .ok_or(SourceDocumentError::UnsupportedShape)?,
    )?;
    let opening_leading_comment_documents =
        match_arm_list_opening_leading_comment_documents(source, &arms)?;
    let mut arm_documents = Vec::with_capacity(match_arms.len());
    for (index, arm) in match_arms.iter().enumerate() {
        arm_documents.push(match_arm_document(
            source,
            arm,
            list_group,
            index + 1 < match_arms.len(),
            expression_group_ids,
        )?);
    }
    let (separator_comment_documents, separator_comments_require_line_break) =
        match_arm_separator_comment_documents(&arms, match_arms.len())?;
    let opening_comments = match_arm_list_opening_comments(&arms)?;
    let closing_comment_documents =
        match_arm_list_closing_comment_documents(source, &arms, &match_arms, &opening_comments)?;
    let mut force_break = separator_comments_require_line_break
        || arm_documents
            .iter()
            .any(document_contains_unavoidable_forced_break);
    for arm in &match_arms {
        let result_last_token = arm
            .expression()
            .and_then(|expression| expression.syntax().last_token())
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        if source_comments_require_line_break(&trailing_comments_after_token(&result_last_token)?) {
            force_break = true;
            break;
        }
    }
    if !opening_comments.is_empty() && !match_arms.is_empty() {
        let mut first_arm_prefix = Vec::new();
        if source_comments_require_line_break(&opening_comments) {
            append_comment_documents(
                &mut first_arm_prefix,
                &opening_comments,
                source.text(),
                None,
            )?;
            let last_comment_end = opening_comments.last().map(|comment| comment.range.end);
            let first_pattern_token = match_arms
                .first()
                .and_then(ast::MatchArm::pattern)
                .and_then(|pattern| pattern.syntax().first_token())
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let first_pattern_leading_comments =
                leading_comments_before_token(&first_pattern_token)?;
            let next_comment_or_pattern = first_pattern_leading_comments
                .first()
                .map(|comment| comment.range.start)
                .unwrap_or_else(|| first_pattern_token.text_range().start().to_usize());
            append_comment_gap(
                &mut first_arm_prefix,
                source.text(),
                last_comment_end,
                Some(next_comment_or_pattern),
            )?;
            force_break = true;
        } else {
            append_single_line_block_comments_without_leading_space(
                &mut first_arm_prefix,
                &opening_comments,
            )?;
            let first_pattern_token = match_arms
                .first()
                .and_then(ast::MatchArm::pattern)
                .and_then(|pattern| pattern.syntax().first_token())
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let first_pattern_leading_comments =
                leading_comments_before_token(&first_pattern_token)?;
            if source_comments_require_line_break(&first_pattern_leading_comments) {
                append_comment_gap(
                    &mut first_arm_prefix,
                    source.text(),
                    opening_comments.last().map(|comment| comment.range.end),
                    first_pattern_leading_comments
                        .first()
                        .map(|comment| comment.range.start),
                )?;
                force_break = true;
            } else {
                first_arm_prefix.push(Doc::text(" "));
            }
        }
        let first_arm = arm_documents
            .first_mut()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        first_arm_prefix.push(std::mem::replace(first_arm, Doc::concat([])));
        *first_arm = Doc::concat(first_arm_prefix);
    }

    let mut documents = vec![Doc::text("match")];
    append_trailing_comment_documents(&mut documents, keyword_comments);
    if keyword_comments.last().is_some_and(|comment| {
        comment.kind == TriviaKind::LineComment
            || (comment.kind == TriviaKind::BlockComment && comment.text.contains('\n'))
    }) {
        documents.push(Doc::HardLine);
    } else {
        documents.push(Doc::text(" "));
    }
    documents.push(expression_document(
        source,
        &scrutinee,
        ExpressionContext::Unprotected,
        expression_group_ids,
    )?);
    append_trailing_comment_documents(&mut documents, &scrutinee_comments);
    if opening_leading_comment_documents.is_empty() {
        if scrutinee_comments.last().is_some_and(|comment| {
            comment.kind == TriviaKind::LineComment
                || (comment.kind == TriviaKind::BlockComment && comment.text.contains('\n'))
        }) {
            documents.push(Doc::HardLine);
        } else {
            documents.push(Doc::text(" "));
        }
    } else {
        documents.extend(opening_leading_comment_documents);
    }
    documents.push(brace_list_document_with_separator_suffixes(
        list_group,
        arm_documents,
        force_break,
        &separator_comment_documents,
        &closing_comment_documents,
    ));
    Ok(Doc::concat(documents))
}

fn match_arm_list_elements_match(list: &ast::MatchArmList, arms: &[ast::MatchArm]) -> bool {
    let elements = list.syntax().children_with_tokens().collect::<Vec<_>>();
    let mut cursor = 0;
    if !matches!(
        elements.get(cursor),
        Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::L_BRACE
    ) {
        return false;
    }
    cursor += 1;
    while matches!(
        elements.get(cursor),
        Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::NEWLINE
    ) {
        cursor += 1;
    }

    for (index, arm) in arms.iter().enumerate() {
        let arm_syntax = arm.syntax();
        if !matches!(
            elements.get(cursor),
            Some(SyntaxElement::Node(node))
                if node.kind() == SyntaxKind::MATCH_ARM
                    && node.text_range() == arm_syntax.text_range()
        ) {
            return false;
        }
        cursor += 1;

        let mut has_separator = false;
        while matches!(
            elements.get(cursor),
            Some(SyntaxElement::Token(token))
                if matches!(token.kind(), SyntaxKind::COMMA | SyntaxKind::NEWLINE)
        ) {
            has_separator = true;
            cursor += 1;
        }
        if index + 1 < arms.len() && !has_separator {
            return false;
        }
        if index + 1 == arms.len() {
            break;
        }
    }

    while matches!(
        elements.get(cursor),
        Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::NEWLINE
    ) {
        cursor += 1;
    }
    if !matches!(
        elements.get(cursor),
        Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::R_BRACE
    ) {
        return false;
    }
    cursor += 1;
    cursor == elements.len()
}

fn match_arm_separator_comment_documents(
    list: &ast::MatchArmList,
    arm_count: usize,
) -> Result<(Vec<Vec<Doc>>, bool), SourceDocumentError> {
    let mut separator_documents = vec![Vec::new(); arm_count];
    let mut comments_require_line_break = false;
    let mut next_arm_index = 0;
    let mut previous_arm_index = None;
    for element in list.syntax().children_with_tokens() {
        match element {
            SyntaxElement::Node(arm) if arm.kind() == SyntaxKind::MATCH_ARM => {
                previous_arm_index = Some(next_arm_index);
                next_arm_index += 1;
            }
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA => {
                let comments = trailing_comments_after_token(&token)?;
                if comments.is_empty() {
                    continue;
                }
                let separator_index = previous_arm_index
                    .filter(|index| *index < arm_count)
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                if !separator_documents[separator_index].is_empty() {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                let documents = &mut separator_documents[separator_index];
                if source_comments_require_line_break(&comments) {
                    append_match_arm_comment_run(documents, &comments);
                    comments_require_line_break = true;
                } else {
                    append_single_line_block_comments(documents, &comments)?;
                }
            }
            _ => {}
        }
    }
    Ok((separator_documents, comments_require_line_break))
}

fn match_arm_list_opening_comments(
    list: &ast::MatchArmList,
) -> Result<Vec<SourceComment>, SourceDocumentError> {
    let opening = list
        .syntax()
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::L_BRACE => Some(token),
            _ => None,
        })
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    trailing_comments_after_token(&opening)
}

fn match_arm_list_opening_leading_comment_documents(
    source: &ParsedSource,
    list: &ast::MatchArmList,
) -> Result<Vec<Doc>, SourceDocumentError> {
    let (opening, _) =
        brace_list_delimiters(list.syntax()).ok_or(SourceDocumentError::UnsupportedShape)?;
    let comments = leading_comments_before_token(&opening)?;
    if comments.is_empty() {
        return Ok(Vec::new());
    }

    let mut documents = vec![Doc::HardLine];
    append_comment_documents(&mut documents, &comments, source.text(), None)?;
    append_comment_gap(
        &mut documents,
        source.text(),
        comments.last().map(|comment| comment.range.end),
        Some(opening.text_range().start().to_usize()),
    )?;
    Ok(documents)
}

fn match_arm_list_closing_comment_documents(
    source: &ParsedSource,
    list: &ast::MatchArmList,
    arms: &[ast::MatchArm],
    opening_comments: &[SourceComment],
) -> Result<Vec<Doc>, SourceDocumentError> {
    let (_, closing) =
        brace_list_delimiters(list.syntax()).ok_or(SourceDocumentError::UnsupportedShape)?;
    let comments = leading_comments_before_token(&closing)?;
    if comments.is_empty() && !(arms.is_empty() && !opening_comments.is_empty()) {
        return Ok(Vec::new());
    }

    if arms.is_empty() {
        let mut documents = vec![Doc::HardLine];
        append_comment_documents(&mut documents, opening_comments, source.text(), None)?;
        if !comments.is_empty() {
            append_comment_documents(
                &mut documents,
                &comments,
                source.text(),
                opening_comments.last().map(|comment| comment.range.end),
            )?;
        }
        return Ok(documents);
    }

    let previous_end = if let Some(last_arm) = arms.last() {
        let final_comma = list
            .syntax()
            .children_with_tokens()
            .filter_map(|element| match element {
                SyntaxElement::Token(token)
                    if token.kind() == SyntaxKind::COMMA
                        && token.text_range().start() >= last_arm.syntax().text_range().end() =>
                {
                    Some(token)
                }
                _ => None,
            })
            .last();
        let trailing_comma_comments = final_comma
            .as_ref()
            .map(trailing_comments_after_token)
            .transpose()?
            .unwrap_or_default();
        if let Some(comment) = trailing_comma_comments.last() {
            Some(comment.range.end)
        } else {
            let last_token = last_arm
                .expression()
                .and_then(|expression| expression.syntax().last_token())
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let trailing_result_comments = trailing_comments_after_token(&last_token)?;
            trailing_result_comments
                .last()
                .map(|comment| comment.range.end)
                .or_else(|| Some(last_token.text_range().end().to_usize()))
        }
    } else {
        None
    };

    let mut documents = Vec::new();
    if let Some(previous_end) = previous_end {
        append_comment_documents(&mut documents, &comments, source.text(), Some(previous_end))?;
    } else {
        documents.push(Doc::HardLine);
        append_comment_documents(&mut documents, &comments, source.text(), None)?;
    }
    Ok(documents)
}

fn match_arm_document(
    source: &ParsedSource,
    arm: &ast::MatchArm,
    list_group: GroupId,
    has_following_arm: bool,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let pattern = arm.pattern().ok_or(SourceDocumentError::UnsupportedShape)?;
    let guard = arm.guard();
    let expression = arm
        .expression()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let arm_children = arm.syntax().children().collect::<Vec<_>>();
    let expected_child_ranges = std::iter::once(pattern.syntax().text_range())
        .chain(guard.as_ref().map(|guard| guard.syntax().text_range()))
        .chain(std::iter::once(expression.syntax().text_range()))
        .collect::<Vec<_>>();
    if arm.syntax().children().count() != 2 + usize::from(guard.is_some())
        || arm_children
            .iter()
            .map(|child| child.text_range())
            .ne(expected_child_ranges)
        || direct_token_kinds(arm.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::FAT_ARROW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let pattern_first_token = pattern
        .syntax()
        .first_token()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let leading_pattern_comments = leading_comments_before_token(&pattern_first_token)?;
    let mut documents = Vec::new();
    if source_comments_require_line_break(&leading_pattern_comments) {
        append_comment_documents(
            &mut documents,
            &leading_pattern_comments,
            source.text(),
            None,
        )?;
        let last_comment_end = leading_pattern_comments
            .last()
            .map(|comment| comment.range.end);
        append_comment_gap(
            &mut documents,
            source.text(),
            last_comment_end,
            Some(pattern_first_token.text_range().start().to_usize()),
        )?;
    } else {
        append_single_line_block_comments_without_leading_space(
            &mut documents,
            &leading_pattern_comments,
        )?;
        if !leading_pattern_comments.is_empty() {
            documents.push(Doc::text(" "));
        }
    }
    documents.push(pattern_document(source, &pattern, expression_group_ids)?);
    let pattern_last_token = pattern
        .syntax()
        .last_token()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let pattern_comments = trailing_comments_after_token(&pattern_last_token)?;
    append_single_line_block_comments(&mut documents, &pattern_comments)?;
    if let Some(guard) = guard {
        let guard_expression = guard
            .expression()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        if guard.syntax().children().count() != 1
            || direct_token_kinds(guard.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>()
                != [SyntaxKind::IF_KW]
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        documents.push(Doc::text(" if"));
        let guard_keyword_comments =
            trailing_comments_after_direct_token(guard.syntax(), SyntaxKind::IF_KW)?;
        append_match_arm_comment_run(&mut documents, &guard_keyword_comments);
        let guard_expression_document = expression_document(
            source,
            &guard_expression,
            ExpressionContext::Unprotected,
            expression_group_ids,
        )?;
        if source_comments_require_line_break(&guard_keyword_comments) {
            documents.push(Doc::indent(Doc::concat([
                Doc::HardLine,
                guard_expression_document,
            ])));
        } else {
            documents.extend([Doc::text(" "), guard_expression_document]);
        }
        let guard_last_token = guard_expression
            .syntax()
            .last_token()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let guard_expression_comments = trailing_comments_after_token(&guard_last_token)?;
        append_single_line_block_comments(&mut documents, &guard_expression_comments)?;
    }
    documents.push(Doc::text(" =>"));
    let arrow_comments = trailing_comments_after_direct_token(arm.syntax(), SyntaxKind::FAT_ARROW)?;
    append_match_arm_comment_run(&mut documents, &arrow_comments);
    let arrow_comments_require_line_break = source_comments_require_line_break(&arrow_comments);

    let body_document = expression_document(
        source,
        &expression,
        ExpressionContext::Unprotected,
        expression_group_ids,
    )?;
    let body = if arrow_comments_require_line_break {
        let body_document = if has_following_arm && is_open_ended_range_expression(&expression) {
            Doc::concat([Doc::text("("), body_document, Doc::text(")")])
        } else {
            body_document
        };
        Doc::indent(Doc::concat([Doc::HardLine, body_document]))
    } else if has_following_arm && is_open_ended_range_expression(&expression) {
        let parenthesized = right_hand_side_document_from_document(
            &expression,
            Doc::concat([Doc::text("("), body_document.clone(), Doc::text(")")]),
            true,
        );
        let plain = right_hand_side_document_from_document(&expression, body_document, true);
        Doc::if_broken(list_group, parenthesized, plain)
    } else {
        right_hand_side_document_from_document(&expression, body_document, true)
    };
    documents.push(body);
    let expression_last_token = expression
        .syntax()
        .last_token()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let expression_comments = trailing_comments_after_token(&expression_last_token)?;
    if source_comments_require_line_break(&expression_comments) {
        append_match_arm_comment_run(&mut documents, &expression_comments);
    } else {
        append_single_line_block_comments(&mut documents, &expression_comments)?;
    }
    Ok(Doc::concat(documents))
}

fn append_match_arm_comment_run(document: &mut Vec<Doc>, comments: &[SourceComment]) {
    let mut previous_comment_requires_line_break = false;
    for comment in comments {
        if previous_comment_requires_line_break {
            document.push(Doc::HardLine);
        } else {
            document.push(Doc::text(" "));
        }
        if comment.kind == TriviaKind::LineComment {
            document.push(Doc::line_suffix(Doc::text(comment.text.clone())));
        } else {
            document.push(Doc::verbatim(comment.text.clone()));
        }
        // Keep a trailing comment run together after block comments. Breaking
        // there can reattach the next comment as leading trivia and break a
        // second formatting pass.
        previous_comment_requires_line_break = comment.kind == TriviaKind::LineComment;
    }
}

fn source_comments_require_line_break(comments: &[SourceComment]) -> bool {
    comments.iter().any(|comment| {
        comment.kind == TriviaKind::LineComment
            || (comment.kind == TriviaKind::BlockComment && comment.text.contains('\n'))
    })
}

fn is_open_ended_range_expression(expression: &Expr) -> bool {
    matches!(expression, Expr::Range(range) if range.end().is_none())
}

fn condition_document(
    source: &ParsedSource,
    condition: &ast::Condition,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let condition_tokens = direct_token_kinds(condition.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if condition.syntax().children().count() != 1 || !condition_tokens.is_empty() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if let Some(expression) = condition.expression() {
        if condition.let_condition().is_some() {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        return expression_document(
            source,
            &expression,
            ExpressionContext::Unprotected,
            expression_group_ids,
        );
    }

    let let_condition = condition
        .let_condition()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let pattern = let_condition
        .pattern()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let initializer = let_condition
        .initializer()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let let_tokens = direct_token_kinds(let_condition.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if let_condition.syntax().children().count() != 2
        || let_tokens != [SyntaxKind::LET_KW, SyntaxKind::EQ]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text("let "),
        pattern_document(source, &pattern, expression_group_ids)?,
        Doc::text(" = "),
        expression_document(
            source,
            &initializer,
            ExpressionContext::Unprotected,
            expression_group_ids,
        )?,
    ]))
}

fn loop_label_document(label: &ast::LoopLabel) -> Result<Doc, SourceDocumentError> {
    let token = label.label().ok_or(SourceDocumentError::UnsupportedShape)?;
    let label_tokens = direct_token_kinds(label.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if label.syntax().children().count() != 0
        || label_tokens != [SyntaxKind::LABEL, SyntaxKind::COLON]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text(token.syntax().text().to_string()),
        Doc::text(": "),
    ]))
}

fn loop_expression_document(
    source: &ParsedSource,
    loop_expr: &ast::LoopExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    loop_expression_document_with_keyword_comments(source, loop_expr, expression_group_ids, &[])
}

fn loop_expression_document_with_keyword_comments(
    source: &ParsedSource,
    loop_expr: &ast::LoopExpr,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let label = loop_expr.label();
    let body = loop_expr
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let loop_tokens = direct_token_kinds(loop_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if loop_expr.syntax().children().count() != 1 + usize::from(label.is_some())
        || loop_tokens != [SyntaxKind::LOOP_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(3);
    if let Some(label) = label {
        documents.push(loop_label_document(&label)?);
    }
    documents.push(Doc::text("loop"));
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    documents.extend([
        Doc::text(" "),
        block_document(source, &body, expression_group_ids)?,
    ]);
    Ok(Doc::concat(documents))
}

fn while_expression_document(
    source: &ParsedSource,
    while_expr: &ast::WhileExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    while_expression_document_with_keyword_comments(source, while_expr, expression_group_ids, &[])
}

fn while_expression_document_with_keyword_comments(
    source: &ParsedSource,
    while_expr: &ast::WhileExpr,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let label = while_expr.label();
    let condition = while_expr
        .condition()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let body = while_expr
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let while_tokens = direct_token_kinds(while_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if while_expr.syntax().children().count() != 2 + usize::from(label.is_some())
        || while_tokens != [SyntaxKind::WHILE_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(5);
    if let Some(label) = label {
        documents.push(loop_label_document(&label)?);
    }
    documents.push(Doc::text("while"));
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    documents.extend([
        Doc::text(" "),
        condition_document(source, &condition, expression_group_ids)?,
        Doc::text(" "),
        block_document(source, &body, expression_group_ids)?,
    ]);
    Ok(Doc::concat(documents))
}

fn pattern_document(
    source: &ParsedSource,
    pattern: &ast::Pattern,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    match pattern {
        ast::Pattern::Binding(_) | ast::Pattern::Wildcard(_) => {
            simple_binding_pattern_document(source, pattern)
        }
        ast::Pattern::Or(or_pattern) => {
            or_pattern_document(source, or_pattern, expression_group_ids)
        }
        ast::Pattern::At(at_pattern) => {
            let binding = at_pattern
                .binding()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let pattern = at_pattern
                .pattern()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            if at_pattern.syntax().children().count() != 2
                || direct_token_kinds(at_pattern.syntax())
                    .into_iter()
                    .filter(|kind| *kind != SyntaxKind::NEWLINE)
                    .collect::<Vec<_>>()
                    != [SyntaxKind::AT]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let at_token = at_pattern
                .syntax()
                .children_with_tokens()
                .find_map(|element| match element {
                    SyntaxElement::Token(token) if token.kind() == SyntaxKind::AT => Some(token),
                    _ => None,
                })
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let at_comments = trailing_comments_after_token(&at_token)?;
            let nested_pattern = pattern_document(source, &pattern, expression_group_ids)?;
            let mut documents = vec![binding_pattern_document(source, &binding)?, Doc::text(" @")];
            append_match_arm_comment_run(&mut documents, &at_comments);
            if at_comments
                .last()
                .is_some_and(|comment| comment.kind == TriviaKind::LineComment)
            {
                documents.push(Doc::indent(Doc::concat([Doc::HardLine, nested_pattern])));
            } else {
                documents.extend([Doc::text(" "), nested_pattern]);
            }
            Ok(Doc::concat(documents))
        }
        ast::Pattern::Literal(literal) => literal_pattern_document(source, literal),
        ast::Pattern::Range(range) => range_pattern_document(source, range),
        ast::Pattern::Tuple(tuple) => tuple_pattern_document(source, tuple, expression_group_ids),
        ast::Pattern::Parenthesized(paren) => {
            let pattern = paren
                .pattern()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            if paren.syntax().children().count() != 1
                || direct_token_kinds(paren.syntax())
                    .into_iter()
                    .filter(|kind| *kind != SyntaxKind::NEWLINE)
                    .collect::<Vec<_>>()
                    != [SyntaxKind::L_PAREN, SyntaxKind::R_PAREN]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::concat([
                Doc::text("("),
                pattern_document(source, &pattern, expression_group_ids)?,
                Doc::text(")"),
            ]))
        }
        ast::Pattern::Slice(slice) => slice_pattern_document(source, slice, expression_group_ids),
        ast::Pattern::Constructor(constructor) => {
            constructor_pattern_document(source, constructor, expression_group_ids)
        }
        ast::Pattern::ImplicitMember(member) => {
            implicit_member_pattern_document(source, member, expression_group_ids)
        }
    }
}

fn simple_binding_pattern_document(
    source: &ParsedSource,
    pattern: &ast::Pattern,
) -> Result<Doc, SourceDocumentError> {
    match pattern {
        ast::Pattern::Binding(binding) => binding_pattern_document(source, binding),
        ast::Pattern::Wildcard(wildcard) => {
            if wildcard.syntax().children().count() != 0
                || direct_token_kinds(wildcard.syntax()) != [SyntaxKind::UNDERSCORE]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::text("_"))
        }
        _ => Err(SourceDocumentError::UnsupportedShape),
    }
}

fn binding_pattern_document(
    source: &ParsedSource,
    binding: &ast::BindingPat,
) -> Result<Doc, SourceDocumentError> {
    let mode = binding.mode();
    let name = binding
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let mut documents = Vec::with_capacity(2);
    if binding.syntax().children().count() != 1 + usize::from(mode.is_some())
        || !direct_token_kinds(binding.syntax()).is_empty()
        || name.syntax().children().count() != 0
        || direct_token_kinds(name.syntax()) != [SyntaxKind::LIDENT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if let Some(mode) = mode {
        let mode_tokens = direct_token_kinds(mode.syntax());
        let mode_text = match mode_tokens.as_slice() {
            [SyntaxKind::VAR_KW] => "var ",
            [SyntaxKind::BORROW_KW] => "borrow ",
            [SyntaxKind::BORROW_KW, SyntaxKind::MUT_KW] => "borrow mut ",
            _ => return Err(SourceDocumentError::UnsupportedShape),
        };
        if mode.syntax().children().count() != 0 {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        documents.push(Doc::text(mode_text));
    }
    documents.push(Doc::text(node_text(source, name.syntax())));
    Ok(Doc::concat(documents))
}

fn or_pattern_document(
    source: &ParsedSource,
    or_pattern: &ast::OrPat,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let alternatives = or_pattern.alternatives().collect::<Vec<_>>();
    let direct_tokens = direct_token_kinds(or_pattern.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let first_element = or_pattern.syntax().children_with_tokens().find(|element| {
        !matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::NEWLINE)
    });
    let leading_pipe = matches!(
        first_element,
        Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::PIPE
    );
    let expected_pipe_count = alternatives.len().saturating_sub(1) + usize::from(leading_pipe);
    if or_pattern.syntax().children().count() != alternatives.len()
        || direct_tokens.len() != expected_pipe_count
        || direct_tokens.iter().any(|kind| *kind != SyntaxKind::PIPE)
        || alternatives.is_empty()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = alternatives
        .into_iter()
        .map(|alternative| pattern_document(source, &alternative, expression_group_ids));
    let first = documents
        .next()
        .ok_or(SourceDocumentError::UnsupportedShape)??;
    let mut alternatives = vec![first];
    for alternative in documents {
        alternatives.extend([
            Doc::text(" |"),
            Doc::indent(Doc::concat([Doc::Line, alternative?])),
        ]);
    }
    if alternatives.len() == 1 {
        Ok(alternatives
            .pop()
            .ok_or(SourceDocumentError::UnsupportedShape)?)
    } else {
        Ok(Doc::group(
            expression_group_ids.next(),
            Doc::concat(alternatives),
        ))
    }
}

fn literal_pattern_document(
    source: &ParsedSource,
    literal: &ast::LiteralPat,
) -> Result<Doc, SourceDocumentError> {
    if literal.syntax().children().count() != 0 {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let tokens = literal
        .syntax()
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() != SyntaxKind::NEWLINE => Some(token),
            SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
        })
        .collect::<Vec<_>>();
    match tokens.as_slice() {
        [token] => {
            let literal = LiteralToken::cast((*token).clone())
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            literal_document(source, literal)
        }
        [minus, integer]
            if minus.kind() == SyntaxKind::MINUS && integer.kind() == SyntaxKind::INT =>
        {
            let integer = LiteralToken::cast((*integer).clone())
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                Doc::text("-"),
                literal_document(source, integer)?,
            ]))
        }
        _ => Err(SourceDocumentError::UnsupportedShape),
    }
}

fn range_pattern_endpoint_document(
    source: &ParsedSource,
    endpoint: &ast::RangePatEndpoint,
) -> Result<Doc, SourceDocumentError> {
    match endpoint {
        ast::RangePatEndpoint::Literal(literal) => literal_pattern_document(source, literal),
        ast::RangePatEndpoint::ConstantPath(path) => const_path_pattern_document(source, path),
    }
}

fn range_pattern_document(
    source: &ParsedSource,
    range: &ast::RangePat,
) -> Result<Doc, SourceDocumentError> {
    let start = range.start();
    let end = range.end();
    let tokens = direct_token_kinds(range.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if range.syntax().children().count()
        != usize::from(start.is_some()) + usize::from(end.is_some())
        || tokens.len() != 1
        || !matches!(tokens[0], SyntaxKind::DOT2 | SyntaxKind::DOT2EQ)
        || (tokens[0] == SyntaxKind::DOT2EQ && end.is_none())
        || (start.is_none() && tokens[0] == SyntaxKind::DOT2)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = Vec::with_capacity(3);
    if let Some(start) = start {
        documents.push(range_pattern_endpoint_document(source, &start)?);
    }
    documents.push(Doc::text(if tokens[0] == SyntaxKind::DOT2EQ {
        "..="
    } else {
        ".."
    }));
    if let Some(end) = end {
        documents.push(range_pattern_endpoint_document(source, &end)?);
    }
    Ok(Doc::concat(documents))
}

fn const_path_pattern_document(
    source: &ParsedSource,
    path: &ast::ConstPath,
) -> Result<Doc, SourceDocumentError> {
    let segments = path.segments().collect::<Vec<_>>();
    let tokens = direct_token_kinds(path.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if segments.is_empty()
        || path.syntax().children().count() != segments.len()
        || tokens.len() != segments.len().saturating_sub(1)
        || tokens.iter().any(|kind| *kind != SyntaxKind::DOT)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = Vec::with_capacity(segments.len() * 2 - 1);
    for (index, segment) in segments.iter().enumerate() {
        if segment.syntax().children().count() != 0 {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let token = segment
            .syntax()
            .children_with_tokens()
            .find_map(|element| match element {
                SyntaxElement::Token(token) => Some(token),
                SyntaxElement::Node(_) => None,
            })
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        if (index + 1 == segments.len() && token.kind() != SyntaxKind::UIDENT)
            || (index + 1 < segments.len()
                && !matches!(token.kind(), SyntaxKind::LIDENT | SyntaxKind::UIDENT))
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        if index > 0 {
            documents.push(Doc::text("."));
        }
        documents.push(Doc::text(node_text(source, segment.syntax())));
    }
    Ok(Doc::concat(documents))
}

fn tuple_pattern_document(
    source: &ParsedSource,
    tuple: &ast::TuplePat,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let elements = tuple.elements().collect::<Vec<_>>();
    if tuple.syntax().children().count() != elements.len()
        || !comma_delimited_tokens_match(
            tuple.syntax(),
            SyntaxKind::L_PAREN,
            SyntaxKind::R_PAREN,
            elements.len(),
        )
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = elements
        .into_iter()
        .map(|element| pattern_document(source, &element, expression_group_ids))
        .collect::<Result<Vec<_>, _>>()?;
    let opening_suffix = delimited_pattern_opening_suffix(tuple.syntax(), SyntaxKind::L_PAREN)?;
    let separator_suffixes = delimited_pattern_separator_suffixes(tuple.syntax(), documents.len())?;
    Ok(delimited_list_document_with_options_and_suffixes(
        expression_group_ids.next(),
        "(",
        ")",
        documents,
        true,
        true,
        DelimitedListSuffixes {
            opening: opening_suffix,
            separators: separator_suffixes,
        },
    ))
}

fn slice_pattern_document(
    source: &ParsedSource,
    slice: &ast::SlicePat,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let children = slice.syntax().children().collect::<Vec<_>>();
    if !comma_delimited_tokens_match(
        slice.syntax(),
        SyntaxKind::L_BRACKET,
        SyntaxKind::R_BRACKET,
        children.len(),
    ) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = children
        .into_iter()
        .map(|child| {
            if let Some(pattern) = ast::Pattern::cast(child.clone()) {
                pattern_document(source, &pattern, expression_group_ids)
            } else if let Some(rest) = ast::RestPat::cast(child) {
                rest_pattern_document(source, &rest, true)
            } else {
                Err(SourceDocumentError::UnsupportedShape)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    if documents.len() != slice.elements().count() + slice.rest_patterns().count()
        || slice.syntax().children().count() != documents.len()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let opening_suffix = delimited_pattern_opening_suffix(slice.syntax(), SyntaxKind::L_BRACKET)?;
    let separator_suffixes = delimited_pattern_separator_suffixes(slice.syntax(), documents.len())?;
    Ok(delimited_list_document_with_options_and_suffixes(
        expression_group_ids.next(),
        "[",
        "]",
        documents,
        true,
        false,
        DelimitedListSuffixes {
            opening: opening_suffix,
            separators: separator_suffixes,
        },
    ))
}

fn delimited_pattern_opening_suffix(
    node: &SyntaxNode,
    opening_kind: SyntaxKind,
) -> Result<Vec<Doc>, SourceDocumentError> {
    let opening = node
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == opening_kind => Some(token),
            _ => None,
        })
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let comments = trailing_comments_after_token(&opening)?;
    let mut suffix = Vec::new();
    append_single_line_block_comments_without_leading_space(&mut suffix, &comments)?;
    Ok(suffix)
}

fn delimited_pattern_separator_suffixes(
    node: &SyntaxNode,
    item_count: usize,
) -> Result<Vec<Vec<Doc>>, SourceDocumentError> {
    let separator_count = item_count.saturating_sub(1);
    let commas = node
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA => Some(token),
            _ => None,
        })
        .take(separator_count)
        .collect::<Vec<_>>();
    if commas.len() != separator_count {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    commas
        .into_iter()
        .map(|comma| {
            let comments = trailing_comments_after_token(&comma)?;
            let mut suffix = Vec::new();
            append_single_line_block_comments(&mut suffix, &comments)?;
            Ok(suffix)
        })
        .collect()
}

fn rest_pattern_document(
    source: &ParsedSource,
    rest: &ast::RestPat,
    allow_binding: bool,
) -> Result<Doc, SourceDocumentError> {
    let binding = rest.binding();
    if rest.syntax().children().count() != usize::from(binding.is_some())
        || direct_token_kinds(rest.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::DOT2]
        || (binding.is_some() && !allow_binding)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("..")];
    if let Some(binding) = binding {
        documents.push(binding_pattern_document(source, &binding)?);
    }
    Ok(Doc::concat(documents))
}

fn named_pattern_argument_document(
    source: &ParsedSource,
    argument: &ast::NamedPatArg,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let name = argument
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let pattern = argument
        .pattern()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if argument.syntax().children().count() != 2
        || direct_token_kinds(argument.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::COLON]
        || name.syntax().children().count() != 0
        || direct_token_kinds(name.syntax()) != [SyntaxKind::LIDENT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text(node_text(source, name.syntax())),
        Doc::text(": "),
        pattern_document(source, &pattern, expression_group_ids)?,
    ]))
}

fn constructor_pattern_arguments_document(
    source: &ParsedSource,
    arguments: &ast::CtorPatArgs,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let children = arguments.syntax().children().collect::<Vec<_>>();
    if !comma_delimited_tokens_match(
        arguments.syntax(),
        SyntaxKind::L_PAREN,
        SyntaxKind::R_PAREN,
        children.len(),
    ) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = Vec::with_capacity(children.len());
    for child in &children {
        if let Some(pattern) = ast::Pattern::cast(child.clone()) {
            documents.push(pattern_document(source, &pattern, expression_group_ids)?);
        } else if let Some(named) = ast::NamedPatArg::cast(child.clone()) {
            documents.push(named_pattern_argument_document(
                source,
                &named,
                expression_group_ids,
            )?);
        } else if let Some(rest) = ast::RestPat::cast(child.clone()) {
            documents.push(rest_pattern_document(source, &rest, false)?);
        } else {
            return Err(SourceDocumentError::UnsupportedShape);
        }
    }
    if documents.len()
        != arguments.positional_patterns().count()
            + arguments.named_args().count()
            + arguments.rest_patterns().count()
        || arguments.syntax().children().count() != documents.len()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let final_is_rest = children
        .last()
        .is_some_and(|child| ast::RestPat::can_cast(child.kind()));
    let opening_suffix = delimited_pattern_opening_suffix(arguments.syntax(), SyntaxKind::L_PAREN)?;
    let separator_suffixes =
        delimited_pattern_separator_suffixes(arguments.syntax(), documents.len())?;
    Ok(delimited_list_document_with_options_and_suffixes(
        expression_group_ids.next(),
        "(",
        ")",
        documents,
        !final_is_rest,
        false,
        DelimitedListSuffixes {
            opening: opening_suffix,
            separators: separator_suffixes,
        },
    ))
}

fn constructor_pattern_document(
    source: &ParsedSource,
    constructor: &ast::CtorPat,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let path = constructor
        .path()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let arguments = constructor.args();
    if constructor.syntax().children().count() != 1 + usize::from(arguments.is_some())
        || !direct_token_kinds(constructor.syntax()).is_empty()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![const_path_pattern_document(source, &path)?];
    if let Some(arguments) = arguments {
        documents.push(constructor_pattern_arguments_document(
            source,
            &arguments,
            expression_group_ids,
        )?);
    }
    Ok(Doc::concat(documents))
}

fn implicit_member_pattern_document(
    source: &ParsedSource,
    member: &ast::ImplicitMemberPat,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let name = member.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let arguments = member.args();
    if member.syntax().children().count() != 1 + usize::from(arguments.is_some())
        || direct_token_kinds(member.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>()
            != [SyntaxKind::DOT]
        || name.syntax().children().count() != 0
        || direct_token_kinds(name.syntax()) != [SyntaxKind::UIDENT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("."), Doc::text(node_text(source, name.syntax()))];
    if let Some(arguments) = arguments {
        documents.push(constructor_pattern_arguments_document(
            source,
            &arguments,
            expression_group_ids,
        )?);
    }
    Ok(Doc::concat(documents))
}

fn for_expression_document(
    source: &ParsedSource,
    for_expr: &ast::ForExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    for_expression_document_with_keyword_comments(source, for_expr, expression_group_ids, &[])
}

fn for_expression_document_with_keyword_comments(
    source: &ParsedSource,
    for_expr: &ast::ForExpr,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let label = for_expr.label();
    let pattern = for_expr
        .pattern()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let iterable = for_expr
        .iterable()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let body = for_expr
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let for_tokens = direct_token_kinds(for_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let is_await = for_tokens.contains(&SyntaxKind::AWAIT_KW);
    let mut expected_tokens = vec![SyntaxKind::FOR_KW];
    if is_await {
        expected_tokens.push(SyntaxKind::AWAIT_KW);
    }
    expected_tokens.push(SyntaxKind::IN_KW);
    if for_expr.syntax().children().count() != 3 + usize::from(label.is_some())
        || for_tokens != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(6);
    if let Some(label) = label {
        documents.push(loop_label_document(&label)?);
    }
    documents.push(Doc::text("for"));
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    documents.push(Doc::text(if is_await { " await " } else { " " }));
    documents.push(pattern_document(source, &pattern, expression_group_ids)?);
    documents.push(Doc::text(" in "));
    documents.push(expression_document(
        source,
        &iterable,
        ExpressionContext::Unprotected,
        expression_group_ids,
    )?);
    documents.push(Doc::text(" "));
    documents.push(block_document(source, &body, expression_group_ids)?);
    Ok(Doc::concat(documents))
}

fn closure_parameter_document(
    source: &ParsedSource,
    parameter: ast::ClosureParam,
) -> Result<Doc, SourceDocumentError> {
    let name = parameter
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let ty = parameter.ty();
    let mutable = direct_token_kinds(parameter.syntax()).contains(&SyntaxKind::VAR_KW);
    let mut expected_tokens = Vec::with_capacity(2);
    if mutable {
        expected_tokens.push(SyntaxKind::VAR_KW);
    }
    if ty.is_some() {
        expected_tokens.push(SyntaxKind::COLON);
    }
    if parameter.syntax().children().count() != 1 + usize::from(ty.is_some())
        || direct_token_kinds(parameter.syntax()) != expected_tokens
        || name.syntax().children().count() != 0
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let token = name.token().ok_or(SourceDocumentError::UnsupportedShape)?;
    if direct_token_kinds(name.syntax()) != [token.kind()] {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = Vec::with_capacity(4);
    match token.kind() {
        // `var _` stays fail-closed (`keeps_malformed_closure_parameters_fail_closed`);
        // `_` with a type follows the rule of function parameters (§14.7.4 rule 2).
        SyntaxKind::UNDERSCORE if !mutable => {}
        SyntaxKind::LIDENT => {
            if mutable {
                documents.push(Doc::text("var "));
            }
        }
        _ => return Err(SourceDocumentError::UnsupportedShape),
    }
    documents.push(Doc::text(node_text(source, name.syntax())));
    if let Some(ty) = ty {
        documents.extend([Doc::text(": "), type_document(source, &ty)?]);
    }
    Ok(Doc::concat(documents))
}

fn closure_parameter_list_document(
    source: &ParsedSource,
    parameters: &ast::ClosureParamList,
) -> Result<Doc, SourceDocumentError> {
    let params = parameters.params().collect::<Vec<_>>();
    if parameters.syntax().children().count() != params.len()
        || !comma_delimited_tokens_match(
            parameters.syntax(),
            SyntaxKind::L_PAREN,
            SyntaxKind::R_PAREN,
            params.len(),
        )
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = params
        .into_iter()
        .map(|parameter| closure_parameter_document(source, parameter))
        .collect::<Result<Vec<_>, _>>()?;
    let group = GroupId(parameters.syntax().text_range().start().raw());
    Ok(delimited_list_document(group, "(", ")", documents, false))
}

fn closure_header_document(
    source: &ParsedSource,
    closure: &ast::ClosureExpr,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let parameters = closure
        .params()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let return_type = closure.ret_type();
    let body = closure
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let tokens = direct_token_kinds(closure.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let move_prefix = match tokens.as_slice() {
        [SyntaxKind::FN_KW] => "",
        [SyntaxKind::MOVE_KW, SyntaxKind::FN_KW] => "move ",
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    if closure.syntax().children().count() != 2 + usize::from(return_type.is_some()) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text(move_prefix), Doc::text("fn")];
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    documents.push(closure_parameter_list_document(source, &parameters)?);
    if let Some(return_type) = return_type {
        let ty = return_type
            .ty()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        if return_type.syntax().children().count() != 1
            || direct_token_kinds(return_type.syntax()) != [SyntaxKind::THIN_ARROW]
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        documents.extend([Doc::text(" -> "), type_document(source, &ty)?]);
    }
    if body.syntax().kind() != SyntaxKind::BLOCK {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat(documents))
}

fn closure_expression_document(
    source: &ParsedSource,
    closure: &ast::ClosureExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    closure_expression_document_with_keyword_comments(source, closure, expression_group_ids, &[])
}

fn closure_expression_document_with_keyword_comments(
    source: &ParsedSource,
    closure: &ast::ClosureExpr,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let body = closure
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let header = closure_header_document(source, closure, keyword_comments)?;
    let broken_block = block_document(source, &body, expression_group_ids)?;
    let statements = body.statements().collect::<Vec<_>>();
    if block_has_comments(&body, &statements)? {
        return Ok(Doc::concat([header, Doc::text(" "), broken_block]));
    }
    if let [Stmt::Expr(statement)] = statements.as_slice() {
        let flat_statement = statement_document(
            source,
            &Stmt::Expr(statement.clone()),
            &[],
            &[],
            &[],
            &[],
            expression_group_ids,
        )?;
        if flat_document_width(&header).is_some() && flat_document_width(&flat_statement).is_some()
        {
            return Ok(Doc::choice([
                Doc::concat([
                    Doc::flat(first_choice_layout(&header)),
                    Doc::text(" { "),
                    Doc::flat(first_choice_layout(&flat_statement)),
                    Doc::text(" }"),
                ]),
                Doc::concat([header, Doc::text(" "), broken_block]),
            ]));
        }
    }
    Ok(Doc::concat([header, Doc::text(" "), broken_block]))
}

fn closure_body_forces_break(
    source: &ParsedSource,
    closure: &ast::ClosureExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<bool, SourceDocumentError> {
    let body = closure
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let statements = body.statements().collect::<Vec<_>>();
    if block_has_comments(&body, &statements)? {
        return Ok(true);
    }
    if statements.is_empty() {
        return Ok(false);
    }
    if let [Stmt::Expr(statement)] = statements.as_slice() {
        let document = statement_document(
            source,
            &Stmt::Expr(statement.clone()),
            &[],
            &[],
            &[],
            &[],
            expression_group_ids,
        )?;
        return Ok(flat_document_width(&document).is_none());
    }
    Ok(true)
}

fn call_arguments_force_member_chain_break(
    source: &ParsedSource,
    arguments: &ast::ArgList,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<bool, SourceDocumentError> {
    for closure in arguments
        .syntax()
        .descendants()
        .filter_map(ast::ClosureExpr::cast)
    {
        if closure_body_forces_break(source, &closure, expression_group_ids)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn async_block_expression_document(
    source: &ParsedSource,
    async_block: &ast::AsyncBlockExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    async_block_expression_document_with_keyword_comments(
        source,
        async_block,
        expression_group_ids,
        &[],
    )
}

fn async_block_expression_document_with_keyword_comments(
    source: &ParsedSource,
    async_block: &ast::AsyncBlockExpr,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let body = async_block
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let tokens = direct_token_kinds(async_block.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let move_prefix = match tokens.as_slice() {
        [SyntaxKind::ASYNC_KW] => "",
        [SyntaxKind::ASYNC_KW, SyntaxKind::MOVE_KW] => "move ",
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    if async_block.syntax().children().count() != 1 {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("async")];
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    documents.extend([
        Doc::text(" "),
        Doc::text(move_prefix),
        block_document(source, &body, expression_group_ids)?,
    ]);
    Ok(Doc::concat(documents))
}

fn region_expression_document(
    source: &ParsedSource,
    region_expr: &ast::RegionExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    region_expression_document_with_keyword_comments(source, region_expr, expression_group_ids, &[])
}

fn region_expression_document_with_keyword_comments(
    source: &ParsedSource,
    region_expr: &ast::RegionExpr,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let name = region_expr
        .name()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let body = region_expr
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let prefix_token = region_expr
        .syntax()
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == SyntaxKind::LIDENT => Some(token),
            SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
        })
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if region_expr.syntax().children().count() != 2
        || direct_token_kinds(region_expr.syntax()) != [SyntaxKind::LIDENT]
        || prefix_token.text() != "region"
        || name.syntax().children().count() != 0
        || direct_token_kinds(name.syntax()) != [SyntaxKind::LIDENT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("region")];
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    documents.extend([
        Doc::text(" "),
        Doc::text(node_text(source, name.syntax())),
        Doc::text(" "),
        block_document(source, &body, expression_group_ids)?,
    ]);
    Ok(Doc::concat(documents))
}

fn first_choice_layout(document: &Doc) -> Doc {
    match document {
        Doc::Text(text) => Doc::text(text.clone()),
        Doc::Line => Doc::Line,
        Doc::SoftLine => Doc::SoftLine,
        Doc::HardLine => Doc::HardLine,
        Doc::HardLineCrlf => Doc::HardLineCrlf,
        Doc::Indent(child) => Doc::indent(first_choice_layout(child)),
        Doc::Group(id, child) => Doc::group(*id, first_choice_layout(child)),
        Doc::IfBroken(id, broken, flat) => {
            Doc::if_broken(*id, first_choice_layout(broken), first_choice_layout(flat))
        }
        Doc::LineSuffix(child) => Doc::line_suffix(first_choice_layout(child)),
        Doc::Choice(alternatives) => alternatives
            .first()
            .map(first_choice_layout)
            .unwrap_or_else(|| Doc::choice([])),
        Doc::Flat(child) => Doc::flat(first_choice_layout(child)),
        Doc::Verbatim(text) => Doc::verbatim(text.clone()),
        Doc::VerbatimExact(text) => Doc::VerbatimExact(text.clone()),
        Doc::Concat(documents) => Doc::concat(documents.iter().map(first_choice_layout)),
    }
}

fn unsafe_block_expression_document(
    source: &ParsedSource,
    unsafe_block: &ast::UnsafeBlockExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    unsafe_block_expression_document_with_keyword_comments(
        source,
        unsafe_block,
        expression_group_ids,
        &[],
    )
}

fn unsafe_block_expression_document_with_keyword_comments(
    source: &ParsedSource,
    unsafe_block: &ast::UnsafeBlockExpr,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let body = unsafe_block
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if unsafe_block.syntax().children().count() != 1
        || direct_token_kinds(unsafe_block.syntax()) != [SyntaxKind::UNSAFE_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut prefix = vec![Doc::text("unsafe")];
    append_single_line_block_comments(&mut prefix, keyword_comments)?;
    prefix.push(Doc::text(" "));
    let prefix = Doc::concat(prefix);
    let statements = body.statements().collect::<Vec<_>>();
    let broken_block = block_document(source, &body, expression_group_ids)?;
    if block_has_comments(&body, &statements)? {
        return Ok(Doc::concat([prefix, broken_block]));
    }
    if let [Stmt::Expr(statement)] = statements.as_slice() {
        let flat_statement = statement_document(
            source,
            &Stmt::Expr(statement.clone()),
            &[],
            &[],
            &[],
            &[],
            expression_group_ids,
        )?;
        let flat_statement = first_choice_layout(&flat_statement);
        return Ok(Doc::choice([
            Doc::concat([
                prefix.clone(),
                Doc::text("{ "),
                Doc::flat(flat_statement),
                Doc::text(" }"),
            ]),
            Doc::concat([prefix, broken_block]),
        ]));
    }
    Ok(Doc::concat([prefix, broken_block]))
}

fn statement_document(
    source: &ParsedSource,
    statement: &Stmt,
    keyword_comments: &[SourceComment],
    expression_keyword_comments: &[SourceComment],
    type_colon_comments: &[SourceComment],
    operator_comments: &[SourceComment],
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    match statement {
        Stmt::Let(statement) => {
            let pattern = statement
                .pattern()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let ty = statement.ty();
            let initializer = statement
                .initializer()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let else_clause = statement.else_clause();
            let statement_children = statement.syntax().children().collect::<Vec<_>>();
            let expected_child_ranges = std::iter::once(pattern.syntax().text_range())
                .chain(ty.as_ref().map(|ty| ty.syntax().text_range()))
                .chain(std::iter::once(initializer.syntax().text_range()))
                .chain(
                    else_clause
                        .as_ref()
                        .map(|else_clause| else_clause.syntax().text_range()),
                )
                .collect::<Vec<_>>();
            let type_tokens = direct_token_kinds(statement.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            let mut expected_tokens = vec![SyntaxKind::LET_KW];
            if ty.is_some() {
                expected_tokens.push(SyntaxKind::COLON);
            }
            expected_tokens.push(SyntaxKind::EQ);
            if statement_children.len() != expected_child_ranges.len()
                || statement_children
                    .iter()
                    .map(|child| child.text_range())
                    .ne(expected_child_ranges)
                || type_tokens != expected_tokens
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let else_suffix = if let Some(else_clause) = else_clause.as_ref() {
                let block = else_clause
                    .block()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                let else_children = else_clause.syntax().children().collect::<Vec<_>>();
                if else_children.len() != 1
                    || else_children[0].text_range() != block.syntax().text_range()
                    || direct_token_kinds(else_clause.syntax())
                        .into_iter()
                        .filter(|kind| *kind != SyntaxKind::NEWLINE)
                        .collect::<Vec<_>>()
                        != [SyntaxKind::ELSE_KW]
                {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                let keyword_comments = trailing_comments_after_direct_token(
                    else_clause.syntax(),
                    SyntaxKind::ELSE_KW,
                )?;
                let mut documents = vec![Doc::text(" else")];
                append_single_line_block_comments(&mut documents, &keyword_comments)?;
                documents.extend([
                    Doc::text(" "),
                    block_document(source, &block, expression_group_ids)?,
                ]);
                Some(Doc::concat(documents))
            } else {
                None
            };
            let mut binding = vec![Doc::text("let")];
            let keyword_comment_break =
                append_comments_between_tokens(&mut binding, keyword_comments);
            if !keyword_comment_break {
                binding.push(Doc::text(" "));
            }
            binding.push(pattern_document(source, &pattern, expression_group_ids)?);
            if let Some(ty) = ty {
                binding.push(Doc::text(":"));
                append_single_line_block_comments(&mut binding, type_colon_comments)?;
                binding.push(Doc::text(" "));
                binding.push(type_document(source, &ty)?);
            } else if !type_colon_comments.is_empty() {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            binding.push(Doc::text(" ="));
            binding.push(right_hand_side_after_operator_document(
                source,
                &initializer,
                ExpressionContext::Unprotected,
                expression_group_ids,
                true,
                else_suffix,
                RightHandSideComments {
                    operator: operator_comments,
                    expression_keyword: expression_keyword_comments,
                },
            )?);
            Ok(Doc::concat(binding))
        }
        Stmt::Var(statement) => {
            let name = statement
                .name()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let ty = statement.ty();
            let initializer = statement
                .initializer()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let type_tokens = direct_token_kinds(statement.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            let mut expected_tokens = vec![SyntaxKind::VAR_KW];
            if ty.is_some() {
                expected_tokens.push(SyntaxKind::COLON);
            }
            expected_tokens.push(SyntaxKind::EQ);
            if name.syntax().children().count() != 0
                || !matches!(
                    direct_token_kinds(name.syntax()).as_slice(),
                    [SyntaxKind::LIDENT]
                )
                || statement.syntax().children().count() != 2 + usize::from(ty.is_some())
                || type_tokens != expected_tokens
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut binding = vec![Doc::text("var")];
            let keyword_comment_break =
                append_comments_between_tokens(&mut binding, keyword_comments);
            if !keyword_comment_break {
                binding.push(Doc::text(" "));
            }
            binding.push(Doc::text(node_text(source, name.syntax())));
            if let Some(ty) = ty {
                binding.push(Doc::text(":"));
                append_single_line_block_comments(&mut binding, type_colon_comments)?;
                binding.push(Doc::text(" "));
                binding.push(type_document(source, &ty)?);
            } else if !type_colon_comments.is_empty() {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            binding.push(Doc::text(" ="));
            binding.push(right_hand_side_after_operator_document(
                source,
                &initializer,
                ExpressionContext::Unprotected,
                expression_group_ids,
                true,
                None,
                RightHandSideComments {
                    operator: operator_comments,
                    expression_keyword: expression_keyword_comments,
                },
            )?);
            Ok(Doc::concat(binding))
        }
        Stmt::Assign(statement) => {
            let lhs = statement
                .lhs()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let rhs = statement
                .rhs()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let (operator, _) = statement
                .op()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let operator_tokens = direct_token_kinds(statement.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            if statement.syntax().children().count() != 2 || operator_tokens != [operator.kind()] {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![
                expression_document(
                    source,
                    &lhs,
                    ExpressionContext::Protected,
                    expression_group_ids,
                )?,
                Doc::text(" "),
                Doc::text(operator.text()),
            ];
            documents.push(right_hand_side_after_operator_document(
                source,
                &rhs,
                ExpressionContext::Unprotected,
                expression_group_ids,
                true,
                None,
                RightHandSideComments {
                    operator: operator_comments,
                    expression_keyword: expression_keyword_comments,
                },
            )?);
            Ok(Doc::concat(documents))
        }
        Stmt::Assert(statement) => {
            let condition = statement
                .condition()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let message = statement.message();
            let mut expected_tokens = vec![SyntaxKind::ASSERT_KW];
            if message.is_some() {
                expected_tokens.push(SyntaxKind::COMMA);
            }
            if statement.syntax().children().count() != 1 + usize::from(message.is_some())
                || direct_token_kinds(statement.syntax()) != expected_tokens
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![Doc::text("assert")];
            let keyword_comment_break =
                append_comments_between_tokens(&mut documents, keyword_comments);
            if !keyword_comment_break {
                documents.push(Doc::text(" "));
            }
            documents.push(expression_document(
                source,
                &condition,
                ExpressionContext::Unprotected,
                expression_group_ids,
            )?);
            if let Some(message) = message {
                documents.extend([
                    Doc::text(", "),
                    expression_document(
                        source,
                        &message,
                        ExpressionContext::Unprotected,
                        expression_group_ids,
                    )?,
                ]);
            }
            Ok(Doc::concat(documents))
        }
        Stmt::Expr(statement) => {
            let expression = statement
                .expression()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            if statement.syntax().children().count() != 1
                || !direct_token_kinds(statement.syntax()).is_empty()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            if !keyword_comments.is_empty() {
                return match &expression {
                    Expr::Return(return_expr) => return_expression_document(
                        source,
                        return_expr,
                        ExpressionContext::Unprotected,
                        expression_group_ids,
                        keyword_comments,
                    ),
                    Expr::Break(break_expr) => break_expression_document(
                        source,
                        break_expr,
                        ExpressionContext::Unprotected,
                        expression_group_ids,
                        keyword_comments,
                    ),
                    Expr::Continue(continue_expr) => {
                        continue_expression_document(continue_expr, keyword_comments)
                    }
                    Expr::If(if_expr) => if_expression_document_with_keyword_comments(
                        source,
                        if_expr,
                        expression_group_ids,
                        keyword_comments,
                    ),
                    Expr::While(while_expr) => while_expression_document_with_keyword_comments(
                        source,
                        while_expr,
                        expression_group_ids,
                        keyword_comments,
                    ),
                    Expr::For(for_expr) => for_expression_document_with_keyword_comments(
                        source,
                        for_expr,
                        expression_group_ids,
                        keyword_comments,
                    ),
                    Expr::Loop(loop_expr) => loop_expression_document_with_keyword_comments(
                        source,
                        loop_expr,
                        expression_group_ids,
                        keyword_comments,
                    ),
                    Expr::Match(match_expr) => match_expression_document_with_keyword_comments(
                        source,
                        match_expr,
                        expression_group_ids,
                        keyword_comments,
                    ),
                    Expr::UnsafeBlock(unsafe_block) => {
                        unsafe_block_expression_document_with_keyword_comments(
                            source,
                            unsafe_block,
                            expression_group_ids,
                            keyword_comments,
                        )
                    }
                    Expr::AsyncBlock(async_block) => {
                        async_block_expression_document_with_keyword_comments(
                            source,
                            async_block,
                            expression_group_ids,
                            keyword_comments,
                        )
                    }
                    Expr::Region(region_expr) => region_expression_document_with_keyword_comments(
                        source,
                        region_expr,
                        expression_group_ids,
                        keyword_comments,
                    ),
                    Expr::Closure(closure) => closure_expression_document_with_keyword_comments(
                        source,
                        closure,
                        expression_group_ids,
                        keyword_comments,
                    ),
                    _ => Err(SourceDocumentError::UnsupportedShape),
                };
            }
            expression_document(
                source,
                &expression,
                ExpressionContext::Unprotected,
                expression_group_ids,
            )
        }
    }
}

fn right_hand_side_document(
    source: &ParsedSource,
    expression: &Expr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
    allow_move: bool,
) -> Result<Doc, SourceDocumentError> {
    let document = expression_document(source, expression, context, expression_group_ids)?;
    Ok(right_hand_side_document_from_document(
        expression, document, allow_move,
    ))
}

fn right_hand_side_after_operator_document(
    source: &ParsedSource,
    expression: &Expr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
    allow_move: bool,
    suffix: Option<Doc>,
    comments: RightHandSideComments<'_>,
) -> Result<Doc, SourceDocumentError> {
    let mut documents = Vec::new();
    append_single_line_block_comments(&mut documents, comments.operator)?;
    let expression_document = if comments.expression_keyword.is_empty() {
        expression_document(source, expression, context, expression_group_ids)?
    } else {
        match expression {
            Expr::Loop(loop_expr) => loop_expression_document_with_keyword_comments(
                source,
                loop_expr,
                expression_group_ids,
                comments.expression_keyword,
            )?,
            Expr::Match(match_expr) => match_expression_document_with_keyword_comments(
                source,
                match_expr,
                expression_group_ids,
                comments.expression_keyword,
            )?,
            Expr::UnsafeBlock(unsafe_block) => {
                unsafe_block_expression_document_with_keyword_comments(
                    source,
                    unsafe_block,
                    expression_group_ids,
                    comments.expression_keyword,
                )?
            }
            Expr::AsyncBlock(async_block) => async_block_expression_document_with_keyword_comments(
                source,
                async_block,
                expression_group_ids,
                comments.expression_keyword,
            )?,
            Expr::Region(region_expr) => region_expression_document_with_keyword_comments(
                source,
                region_expr,
                expression_group_ids,
                comments.expression_keyword,
            )?,
            Expr::Closure(closure) => closure_expression_document_with_keyword_comments(
                source,
                closure,
                expression_group_ids,
                comments.expression_keyword,
            )?,
            _ => return Err(SourceDocumentError::UnsupportedShape),
        }
    };
    documents.push(right_hand_side_document_from_document_with_optional_suffix(
        expression,
        expression_document,
        allow_move,
        suffix,
    ));
    Ok(Doc::concat(documents))
}

struct RightHandSideComments<'a> {
    operator: &'a [SourceComment],
    expression_keyword: &'a [SourceComment],
}

fn append_single_line_block_comments(
    documents: &mut Vec<Doc>,
    comments: &[SourceComment],
) -> Result<(), SourceDocumentError> {
    append_single_line_block_comments_with_spacing(documents, comments, true)
}

fn append_single_line_block_comments_without_leading_space(
    documents: &mut Vec<Doc>,
    comments: &[SourceComment],
) -> Result<(), SourceDocumentError> {
    append_single_line_block_comments_with_spacing(documents, comments, false)
}

fn append_single_line_block_comments_with_spacing(
    documents: &mut Vec<Doc>,
    comments: &[SourceComment],
    first_comment_has_leading_space: bool,
) -> Result<(), SourceDocumentError> {
    for (index, comment) in comments.iter().enumerate() {
        if comment.kind != TriviaKind::BlockComment || comment.text.contains('\n') {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        if first_comment_has_leading_space || index > 0 {
            documents.push(Doc::text(" "));
        }
        documents.push(Doc::verbatim(comment.text.clone()));
    }
    Ok(())
}

fn return_expression_document(
    source: &ParsedSource,
    return_expr: &ast::ReturnExpr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let value = return_expr.value();
    let tokens = direct_token_kinds(return_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if return_expr.syntax().children().count() != usize::from(value.is_some())
        || tokens != [SyntaxKind::RETURN_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("return")];
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    if let Some(value) = value {
        documents.push(right_hand_side_document(
            source,
            &value,
            context,
            expression_group_ids,
            false,
        )?);
    }
    Ok(Doc::concat(documents))
}

fn break_expression_document(
    source: &ParsedSource,
    break_expr: &ast::BreakExpr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let label = break_expr.label();
    let value = break_expr.value();
    let tokens = direct_token_kinds(break_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let mut expected_tokens = vec![SyntaxKind::BREAK_KW];
    if label.is_some() {
        expected_tokens.push(SyntaxKind::LABEL);
    }
    if break_expr.syntax().children().count() != usize::from(value.is_some())
        || tokens != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("break")];
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    if let Some(label) = label {
        documents.push(Doc::text(" "));
        documents.push(Doc::text(label.syntax().text().to_string()));
    }
    if let Some(value) = value {
        documents.push(right_hand_side_document(
            source,
            &value,
            context,
            expression_group_ids,
            false,
        )?);
    }
    Ok(Doc::concat(documents))
}

fn continue_expression_document(
    continue_expr: &ast::ContinueExpr,
    keyword_comments: &[SourceComment],
) -> Result<Doc, SourceDocumentError> {
    let label = continue_expr.label();
    let tokens = direct_token_kinds(continue_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let mut expected_tokens = vec![SyntaxKind::CONTINUE_KW];
    if label.is_some() {
        expected_tokens.push(SyntaxKind::LABEL);
    }
    if continue_expr.syntax().children().count() != 0 || tokens != expected_tokens {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("continue")];
    append_single_line_block_comments(&mut documents, keyword_comments)?;
    if let Some(label) = label {
        documents.push(Doc::text(" "));
        documents.push(Doc::text(label.syntax().text().to_string()));
    }
    Ok(Doc::concat(documents))
}

fn right_hand_side_document_from_document(
    expression: &Expr,
    document: Doc,
    allow_move: bool,
) -> Doc {
    right_hand_side_document_from_document_with_optional_suffix(
        expression, document, allow_move, None,
    )
}

fn right_hand_side_document_from_document_with_optional_suffix(
    expression: &Expr,
    document: Doc,
    allow_move: bool,
    suffix: Option<Doc>,
) -> Doc {
    let block_like = matches!(
        expression.syntax().kind(),
        SyntaxKind::BLOCK_EXPR
            | SyntaxKind::UNSAFE_BLOCK_EXPR
            | SyntaxKind::ASYNC_BLOCK_EXPR
            | SyntaxKind::IF_EXPR
            | SyntaxKind::MATCH_EXPR
            | SyntaxKind::LOOP_EXPR
            | SyntaxKind::WHILE_EXPR
            | SyntaxKind::FOR_EXPR
            | SyntaxKind::CLOSURE_EXPR
            | SyntaxKind::REGION_EXPR
    );
    let with_suffix = |mut documents: Vec<Doc>| {
        if let Some(suffix) = suffix.as_ref() {
            documents.push(suffix.clone());
        }
        Doc::concat(documents)
    };
    if block_like {
        return with_suffix(vec![Doc::text(" "), document]);
    }

    movable_right_hand_side_document(document, allow_move, suffix)
}

fn movable_right_hand_side_document(document: Doc, allow_move: bool, suffix: Option<Doc>) -> Doc {
    let with_suffix = |mut documents: Vec<Doc>| {
        if let Some(suffix) = suffix.as_ref() {
            documents.push(suffix.clone());
        }
        Doc::concat(documents)
    };
    let inline = with_suffix(vec![Doc::text(" "), Doc::flat(document.clone())]);
    let mut alternatives = vec![inline];
    if allow_move {
        alternatives.push(Doc::indent(with_suffix(vec![
            Doc::HardLine,
            Doc::flat(document.clone()),
        ])));
    }
    alternatives.push(with_suffix(vec![Doc::text(" "), document.clone()]));
    if allow_move {
        alternatives.push(Doc::indent(with_suffix(vec![
            Doc::HardLine,
            document.clone(),
        ])));
    }
    alternatives.push(with_suffix(vec![Doc::text(" "), document]));
    Doc::choice(alternatives)
}

fn refined_right_hand_side_document(document: Doc, refinement: Doc) -> Doc {
    let same_line = Doc::concat([
        Doc::text(" "),
        Doc::flat(document.clone()),
        Doc::text(" "),
        Doc::flat(refinement.clone()),
    ]);
    let same_type_moved_refinement = Doc::concat([
        Doc::text(" "),
        Doc::flat(document.clone()),
        Doc::indent(Doc::concat([Doc::HardLine, refinement.clone()])),
    ]);
    let moved_type_moved_refinement = Doc::indent(Doc::concat([
        Doc::HardLine,
        Doc::flat(document.clone()),
        Doc::HardLine,
        refinement.clone(),
    ]));
    let same_line_broken = Doc::concat([
        Doc::text(" "),
        document.clone(),
        Doc::indent(Doc::concat([Doc::HardLine, refinement.clone()])),
    ]);
    let moved_broken = Doc::indent(Doc::concat([
        Doc::HardLine,
        document.clone(),
        Doc::HardLine,
        refinement.clone(),
    ]));
    Doc::choice([
        same_line,
        same_type_moved_refinement,
        moved_type_moved_refinement,
        same_line_broken.clone(),
        moved_broken,
        same_line_broken,
    ])
}

fn where_clause_document(
    source: &ParsedSource,
    clause: &ast::WhereClause,
) -> Result<Doc, SourceDocumentError> {
    let predicates = clause.predicates().collect::<Vec<_>>();
    let token_kinds = direct_token_kinds(clause.syntax());
    let significant_tokens = token_kinds
        .iter()
        .copied()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let expected_tokens = std::iter::once(SyntaxKind::WHERE_KW)
        .chain(std::iter::repeat_n(
            SyntaxKind::COMMA,
            predicates.len().saturating_sub(1),
        ))
        .collect::<Vec<_>>();
    if predicates.is_empty()
        || clause.syntax().children().count() != predicates.len()
        || significant_tokens != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let documents = predicates
        .iter()
        .map(|predicate| where_predicate_document(source, predicate))
        .collect::<Result<Vec<_>, _>>()?;
    let range = clause.syntax().text_range();
    let mut contents = Vec::with_capacity(documents.len() * 2);
    for (index, predicate) in documents.into_iter().enumerate() {
        if index > 0 {
            contents.extend([Doc::text(","), Doc::Line]);
        }
        contents.push(predicate);
    }
    Ok(Doc::group(
        GroupId(range.start().raw()),
        Doc::concat([
            Doc::text("where"),
            Doc::indent(Doc::concat([Doc::Line, Doc::concat(contents)])),
        ]),
    ))
}

fn where_predicate_document(
    source: &ParsedSource,
    predicate: &ast::WherePred,
) -> Result<Doc, SourceDocumentError> {
    let ty = predicate
        .ty()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let bounds = predicate.bounds().collect::<Vec<_>>();
    let elements = predicate
        .syntax()
        .children_with_tokens()
        .collect::<Vec<_>>();
    let children = predicate.syntax().children().collect::<Vec<_>>();
    let ty_syntax = ty.syntax();
    if bounds.is_empty()
        || children.len() != bounds.len() + 1
        || children.first().is_none_or(|child| {
            child.kind() != ty_syntax.kind() || child.text_range() != ty_syntax.text_range()
        })
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut cursor = 0;
    if !matches!(
        elements.get(cursor),
        Some(SyntaxElement::Node(node))
            if node.kind() == ty_syntax.kind() && node.text_range() == ty_syntax.text_range()
    ) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    cursor += 1;
    if !matches!(elements.get(cursor), Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::COLON)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    cursor += 1;

    let mut bound_documents = Vec::with_capacity(bounds.len());
    for (index, bound) in bounds.iter().enumerate() {
        let parenthesized = matches!(
            elements.get(cursor),
            Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::L_PAREN
        );
        if parenthesized {
            cursor += 1;
        }
        let bound_syntax = bound.syntax();
        if !matches!(
            elements.get(cursor),
            Some(SyntaxElement::Node(node))
                if node.kind() == bound_syntax.kind() && node.text_range() == bound_syntax.text_range()
        ) {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        cursor += 1;
        if parenthesized {
            if !matches!(elements.get(cursor), Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::R_PAREN)
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            cursor += 1;
        }
        let document = bound_document(source, bound.clone())?;
        bound_documents.push(if parenthesized {
            Doc::concat([Doc::text("("), document, Doc::text(")")])
        } else {
            document
        });
        if index + 1 < bounds.len() {
            if !matches!(elements.get(cursor), Some(SyntaxElement::Token(token)) if token.kind() == SyntaxKind::PLUS)
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            cursor += 1;
        }
    }
    if cursor != elements.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let range = predicate.syntax().text_range();
    let mut contents = vec![type_document(source, &ty)?, Doc::text(": ")];
    for (index, bound) in bound_documents.into_iter().enumerate() {
        if index > 0 {
            contents.extend([
                Doc::text(" "),
                Doc::text("+"),
                Doc::indent(Doc::concat([Doc::Line, bound])),
            ]);
        } else {
            contents.push(bound);
        }
    }
    Ok(Doc::group(
        GroupId(range.start().raw()),
        Doc::concat(contents),
    ))
}

fn requires_clause_document(
    source: &ParsedSource,
    clause: &ast::RequiresClause,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    contract_clause_document(source, clause.syntax(), "requires", expression_group_ids)
}

fn ensures_clause_document(
    source: &ParsedSource,
    clause: &ast::EnsuresClause,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    contract_clause_document(source, clause.syntax(), "ensures", expression_group_ids)
}

fn contract_clause_document(
    source: &ParsedSource,
    clause: &sai_syntax::SyntaxNode,
    keyword: &str,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let expression = clause
        .children()
        .filter_map(Expr::cast)
        .next()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let token = clause
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(_) => None,
        })
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if clause.children().count() != 1
        || direct_token_kinds(clause) != [SyntaxKind::LIDENT]
        || token_text(source, &token)? != keyword
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text(format!("{keyword} ")),
        expression_document(
            source,
            &expression,
            ExpressionContext::Unprotected,
            expression_group_ids,
        )?,
    ]))
}

fn cast_expression_document(
    source: &ParsedSource,
    cast: &ast::CastExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let operand = cast
        .expression()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let ty = cast.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
    if cast.syntax().children().count() != 2
        || significant_token_kinds(cast.syntax()) != [SyntaxKind::AS_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    // The parser binds `as` tighter than every binary operator, so the operand is
    // never an unparenthesized binary chain that could need parentheses.
    Ok(Doc::concat([
        expression_document(
            source,
            &operand,
            ExpressionContext::Unprotected,
            expression_group_ids,
        )?,
        Doc::text(" as "),
        type_document(source, &ty)?,
    ]))
}

fn expression_document(
    source: &ParsedSource,
    expression: &Expr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    if let Some(chain) = member_chain_document(source, expression, context, expression_group_ids)? {
        return Ok(chain);
    }
    match expression {
        Expr::Literal(literal) => {
            let token = literal
                .token()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            literal_document(source, token)
        }
        Expr::FormatString(format_string) => Ok(Doc::Verbatim(
            node_text(source, format_string.syntax()).to_owned(),
        )),
        Expr::Path(path) => {
            let name = path.name().ok_or(SourceDocumentError::UnsupportedShape)?;
            let generic_args = path.generic_args();
            if path.syntax().children().count() != 1 + usize::from(generic_args.is_some())
                || !direct_token_kinds(path.syntax()).is_empty()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![Doc::text(node_text(source, name.syntax()))];
            if let Some(generic_args) = generic_args {
                documents.push(generic_args_document(source, &generic_args)?);
            }
            Ok(Doc::concat(documents))
        }
        Expr::ImplicitMember(member) => {
            let name = member.name().ok_or(SourceDocumentError::UnsupportedShape)?;
            let tokens = direct_token_kinds(member.syntax())
                .into_iter()
                .filter(|kind| *kind != SyntaxKind::NEWLINE)
                .collect::<Vec<_>>();
            if member.syntax().children().count() != 1
                || tokens != [SyntaxKind::DOT]
                || direct_token_kinds(name.syntax()) != [SyntaxKind::UIDENT]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::concat([
                Doc::text("."),
                Doc::text(node_text(source, name.syntax())),
            ]))
        }
        Expr::Unit(unit) => {
            if unit.syntax().children().count() != 0
                || direct_token_kinds(unit.syntax())
                    .into_iter()
                    .filter(|kind| *kind != SyntaxKind::NEWLINE)
                    .collect::<Vec<_>>()
                    != [SyntaxKind::L_PAREN, SyntaxKind::R_PAREN]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::text("()"))
        }
        Expr::Tuple(tuple) => tuple_expression_document(source, tuple, expression_group_ids),
        Expr::Array(array) => array_expression_document(source, array, expression_group_ids),
        Expr::Block(block) => block_expression_document(source, block, expression_group_ids),
        Expr::UnsafeBlock(unsafe_block) => {
            unsafe_block_expression_document(source, unsafe_block, expression_group_ids)
        }
        Expr::AsyncBlock(async_block) => {
            async_block_expression_document(source, async_block, expression_group_ids)
        }
        Expr::If(if_expr) => if_expression_document(source, if_expr, expression_group_ids),
        Expr::Match(match_expr) => {
            match_expression_document(source, match_expr, expression_group_ids)
        }
        Expr::Loop(loop_expr) => loop_expression_document(source, loop_expr, expression_group_ids),
        Expr::While(while_expr) => {
            while_expression_document(source, while_expr, expression_group_ids)
        }
        Expr::For(for_expr) => for_expression_document(source, for_expr, expression_group_ids),
        Expr::Closure(closure) => {
            closure_expression_document(source, closure, expression_group_ids)
        }
        Expr::Region(region_expr) => {
            region_expression_document(source, region_expr, expression_group_ids)
        }
        Expr::SelfValue(value) => {
            if value.syntax().children().count() != 0
                || direct_token_kinds(value.syntax()) != [SyntaxKind::SELF_KW]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::text(node_text(source, value.syntax())))
        }
        Expr::Field(field) => {
            let base = field.base().ok_or(SourceDocumentError::UnsupportedShape)?;
            let documents = vec![
                expression_document(source, &base, context, expression_group_ids)?,
                field_suffix_document(source, field)?,
            ];
            Ok(Doc::concat(documents))
        }
        Expr::Paren(paren) => {
            let expression = paren
                .expression()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            if paren.syntax().children().count() != 1
                || direct_token_kinds(paren.syntax()) != [SyntaxKind::L_PAREN, SyntaxKind::R_PAREN]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let group_id = expression_group_ids.next();
            Ok(Doc::group(
                group_id,
                Doc::concat([
                    Doc::text("("),
                    Doc::indent(Doc::concat([
                        Doc::SoftLine,
                        expression_document(
                            source,
                            &expression,
                            ExpressionContext::ParenthesizedWhole,
                            expression_group_ids,
                        )?,
                    ])),
                    Doc::SoftLine,
                    Doc::text(")"),
                ]),
            ))
        }
        Expr::Call(call) => {
            let callee = call.callee().ok_or(SourceDocumentError::UnsupportedShape)?;
            let arguments = call.args().ok_or(SourceDocumentError::UnsupportedShape)?;
            if call.syntax().children().count() != 2
                || !direct_token_kinds(call.syntax()).is_empty()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::concat([
                expression_document(source, &callee, context, expression_group_ids)?,
                call_arguments_document(source, &arguments, expression_group_ids)?,
            ]))
        }
        Expr::TupleIndex(tuple_index) => {
            let base = tuple_index
                .base()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                expression_document(source, &base, context, expression_group_ids)?,
                tuple_index_suffix_document(tuple_index)?,
            ]))
        }
        Expr::Await(await_expr) => {
            let operand = await_expr
                .operand()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                expression_document(source, &operand, context, expression_group_ids)?,
                await_suffix_document(await_expr)?,
            ]))
        }
        Expr::Try(try_expr) => {
            let operand = try_expr
                .operand()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                expression_document(source, &operand, context, expression_group_ids)?,
                try_suffix_document(try_expr)?,
            ]))
        }
        Expr::Index(index_expr) => {
            let base = index_expr
                .base()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                expression_document(source, &base, context, expression_group_ids)?,
                index_suffix_document(source, index_expr, expression_group_ids)?,
            ]))
        }
        Expr::Return(return_expr) => {
            return_expression_document(source, return_expr, context, expression_group_ids, &[])
        }
        Expr::Break(break_expr) => {
            break_expression_document(source, break_expr, context, expression_group_ids, &[])
        }
        Expr::Continue(continue_expr) => continue_expression_document(continue_expr, &[]),
        Expr::Binary(binary) => {
            operator_chain_document(source, binary, context, expression_group_ids)
        }
        Expr::Prefix(prefix) => {
            prefix_expression_document(source, prefix, context, expression_group_ids)
        }
        Expr::SelfType(self_type) => {
            if self_type.syntax().children().count() != 0
                || direct_token_kinds(self_type.syntax()) != [SyntaxKind::SELF_TYPE_KW]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            Ok(Doc::text(node_text(source, self_type.syntax())))
        }
        Expr::Cast(cast) => cast_expression_document(source, cast, expression_group_ids),
        Expr::Range(range) => {
            range_expression_document(source, range, context, expression_group_ids)
        }
        _ => Err(SourceDocumentError::UnsupportedShape),
    }
}

fn tuple_index_suffix_document(
    tuple_index: &ast::TupleIndexExpr,
) -> Result<Doc, SourceDocumentError> {
    let index = tuple_index
        .index()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let tokens = direct_token_kinds(tuple_index.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if tuple_index.syntax().children().count() != 1 || tokens != [SyntaxKind::DOT, SyntaxKind::INT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::text(format!(".{index}")))
}

fn await_suffix_document(await_expr: &ast::AwaitExpr) -> Result<Doc, SourceDocumentError> {
    let tokens = direct_token_kinds(await_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if await_expr.syntax().children().count() != 1
        || tokens != [SyntaxKind::DOT, SyntaxKind::AWAIT_KW]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::text(".await"))
}

fn try_suffix_document(try_expr: &ast::TryExpr) -> Result<Doc, SourceDocumentError> {
    let tokens = direct_token_kinds(try_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if try_expr.syntax().children().count() != 1 || tokens != [SyntaxKind::QUESTION] {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::text("?"))
}

fn index_suffix_document(
    source: &ParsedSource,
    index_expr: &ast::IndexExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let index = index_expr
        .index()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let tokens = direct_token_kinds(index_expr.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if index_expr.syntax().children().count() != 2
        || tokens != [SyntaxKind::L_BRACKET, SyntaxKind::R_BRACKET]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(Doc::concat([
        Doc::text("["),
        expression_document(
            source,
            &index,
            ExpressionContext::Protected,
            expression_group_ids,
        )?,
        Doc::text("]"),
    ]))
}

fn tuple_expression_document(
    source: &ParsedSource,
    tuple: &ast::TupleExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let elements = tuple.elements().collect::<Vec<_>>();
    let tokens = direct_token_kinds(tuple.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if elements.is_empty()
        || tuple.syntax().children().count() != elements.len()
        || !comma_delimited_tokens_match(
            tuple.syntax(),
            SyntaxKind::L_PAREN,
            SyntaxKind::R_PAREN,
            elements.len(),
        )
        || (elements.len() == 1
            && tokens != [SyntaxKind::L_PAREN, SyntaxKind::COMMA, SyntaxKind::R_PAREN])
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = elements
        .iter()
        .map(|element| {
            expression_document(
                source,
                element,
                ExpressionContext::Protected,
                expression_group_ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let range = tuple.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "(",
        ")",
        documents,
        true,
    ))
}

fn array_expression_document(
    source: &ParsedSource,
    array: &ast::ArrayExpr,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let elements = array.elements().collect::<Vec<_>>();
    if array.syntax().children().count() != elements.len()
        || !comma_delimited_tokens_match(
            array.syntax(),
            SyntaxKind::L_BRACKET,
            SyntaxKind::R_BRACKET,
            elements.len(),
        )
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = elements
        .iter()
        .map(|element| {
            expression_document(
                source,
                element,
                ExpressionContext::Protected,
                expression_group_ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let range = array.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "[",
        "]",
        documents,
        false,
    ))
}

fn prefix_expression_document(
    source: &ParsedSource,
    prefix: &ast::PrefixExpr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let mut operators = Vec::new();
    let mut expression = Expr::Prefix(prefix.clone());
    while let Expr::Prefix(prefix) = expression {
        let (operator, kind) = prefix.op().ok_or(SourceDocumentError::UnsupportedShape)?;
        let operand = prefix
            .operand()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        let expected_tokens = match kind {
            ast::PrefixOp::Neg => vec![SyntaxKind::MINUS],
            ast::PrefixOp::BitNot => vec![SyntaxKind::TILDE],
            ast::PrefixOp::Deref => vec![SyntaxKind::STAR],
            ast::PrefixOp::Borrow => vec![SyntaxKind::BORROW_KW],
            ast::PrefixOp::BorrowMut => vec![SyntaxKind::BORROW_KW, SyntaxKind::MUT_KW],
            ast::PrefixOp::Not => vec![SyntaxKind::NOT_KW],
        };
        let direct_tokens = direct_token_kinds(prefix.syntax())
            .into_iter()
            .filter(|kind| *kind != SyntaxKind::NEWLINE)
            .collect::<Vec<_>>();
        if prefix.syntax().children().count() != 1
            || direct_tokens != expected_tokens
            || expected_tokens.first() != Some(&operator.kind())
        {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let text = match kind {
            ast::PrefixOp::Neg | ast::PrefixOp::BitNot | ast::PrefixOp::Deref => {
                token_text(source, &operator)?.to_owned()
            }
            ast::PrefixOp::Borrow => "borrow ".to_owned(),
            ast::PrefixOp::BorrowMut => "borrow mut ".to_owned(),
            ast::PrefixOp::Not => "not ".to_owned(),
        };
        operators.push(text);
        expression = operand;
    }

    let operand_context = match context {
        ExpressionContext::ParenthesizedWhole => ExpressionContext::Protected,
        context => context,
    };
    let mut document =
        expression_document(source, &expression, operand_context, expression_group_ids)?;
    for operator in operators.into_iter().rev() {
        document = Doc::concat([Doc::text(operator), document]);
    }
    Ok(document)
}

fn range_expression_document(
    source: &ParsedSource,
    range: &ast::RangeExpr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let start = range.start();
    let end = range.end();
    let direct_tokens = direct_token_kinds(range.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let operators = direct_tokens
        .iter()
        .copied()
        .filter(|kind| matches!(kind, SyntaxKind::DOT2 | SyntaxKind::DOT2EQ))
        .collect::<Vec<_>>();
    let children = usize::from(start.is_some()) + usize::from(end.is_some());
    if range.syntax().children().count() != children
        || operators.len() != 1
        || direct_tokens != operators
        || (operators[0] == SyntaxKind::DOT2EQ && end.is_none())
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let operator = range
        .syntax()
        .children_with_tokens()
        .find_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() == operators[0] => Some(token),
            SyntaxElement::Node(_) | SyntaxElement::Token(_) => None,
        })
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let endpoint_context = match context {
        ExpressionContext::ParenthesizedWhole => ExpressionContext::Protected,
        context => context,
    };

    let mut documents = Vec::with_capacity(3);
    if let Some(start) = start {
        documents.push(expression_document(
            source,
            &start,
            endpoint_context,
            expression_group_ids,
        )?);
    }
    documents.push(Doc::text(token_text(source, &operator)?.to_owned()));
    if let Some(end) = end {
        documents.push(expression_document(
            source,
            &end,
            endpoint_context,
            expression_group_ids,
        )?);
    }
    Ok(Doc::concat(documents))
}

fn operator_chain_document(
    source: &ParsedSource,
    binary: &ast::BinExpr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let (_, _, _, root_operator) = binary_parts(binary)?;
    let precedence = binary_precedence(root_operator);
    let is_comparison = matches!(
        root_operator,
        ast::BinOp::Eq
            | ast::BinOp::Ne
            | ast::BinOp::Lt
            | ast::BinOp::Le
            | ast::BinOp::Gt
            | ast::BinOp::Ge
    );
    let mut operands = Vec::new();
    let mut operators = Vec::new();
    collect_binary_chain(
        &Expr::Binary(binary.clone()),
        precedence,
        &mut operands,
        &mut operators,
    )?;
    if operands.len() != operators.len() + 1 || (is_comparison && operators.len() != 1) {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut operand_documents = operands
        .iter()
        .map(|operand| {
            expression_document(
                source,
                operand,
                ExpressionContext::Protected,
                expression_group_ids,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let first = operand_documents
        .drain(..1)
        .next()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let mut suffixes = Vec::with_capacity(operators.len() * 4);
    for (operator, operand) in operators.iter().zip(operand_documents) {
        suffixes.push(Doc::Line);
        suffixes.push(Doc::text(token_text(source, operator)?.to_owned()));
        suffixes.push(Doc::text(" "));
        suffixes.push(operand);
    }

    let group_id = expression_group_ids.next();
    match context {
        ExpressionContext::Protected => Ok(Doc::group(
            group_id,
            Doc::concat([first, Doc::indent(Doc::concat(suffixes))]),
        )),
        ExpressionContext::ParenthesizedWhole => Ok(Doc::group(
            group_id,
            Doc::concat([first, Doc::concat(suffixes)]),
        )),
        ExpressionContext::Unprotected => Ok(Doc::group(
            group_id,
            Doc::concat([
                Doc::if_broken(group_id, Doc::text("("), Doc::text("")),
                Doc::indent(Doc::concat([Doc::SoftLine, first, Doc::concat(suffixes)])),
                Doc::SoftLine,
                Doc::if_broken(group_id, Doc::text(")"), Doc::text("")),
            ]),
        )),
    }
}

fn binary_parts(
    binary: &ast::BinExpr,
) -> Result<(Expr, SyntaxToken, Expr, ast::BinOp), SourceDocumentError> {
    let lhs = binary.lhs().ok_or(SourceDocumentError::UnsupportedShape)?;
    let rhs = binary.rhs().ok_or(SourceDocumentError::UnsupportedShape)?;
    let (operator, kind) = binary.op().ok_or(SourceDocumentError::UnsupportedShape)?;
    let token_kinds = direct_token_kinds(binary.syntax())
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    if binary.syntax().children().count() != 2 || token_kinds != [operator.kind()] {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok((lhs, operator, rhs, kind))
}

fn binary_precedence(operator: ast::BinOp) -> u8 {
    match operator {
        ast::BinOp::Or => 1,
        ast::BinOp::And => 2,
        ast::BinOp::Eq
        | ast::BinOp::Ne
        | ast::BinOp::Lt
        | ast::BinOp::Le
        | ast::BinOp::Gt
        | ast::BinOp::Ge => 3,
        ast::BinOp::BitOr => 4,
        ast::BinOp::BitXor => 5,
        ast::BinOp::BitAnd => 6,
        ast::BinOp::Shl | ast::BinOp::Shr => 7,
        ast::BinOp::Add | ast::BinOp::Sub | ast::BinOp::WrappingAdd | ast::BinOp::WrappingSub => 8,
        ast::BinOp::Mul | ast::BinOp::Div | ast::BinOp::Rem | ast::BinOp::WrappingMul => 9,
    }
}

fn collect_binary_chain(
    expression: &Expr,
    precedence: u8,
    operands: &mut Vec<Expr>,
    operators: &mut Vec<SyntaxToken>,
) -> Result<(), SourceDocumentError> {
    enum WorkItem {
        Expression(Expr),
        Operator(SyntaxToken),
    }

    let mut pending = vec![WorkItem::Expression(expression.clone())];
    while let Some(item) = pending.pop() {
        match item {
            WorkItem::Operator(operator) => operators.push(operator),
            WorkItem::Expression(Expr::Binary(binary)) => {
                let (lhs, operator, rhs, kind) = binary_parts(&binary)?;
                if binary_precedence(kind) == precedence {
                    pending.push(WorkItem::Expression(rhs));
                    pending.push(WorkItem::Operator(operator));
                    pending.push(WorkItem::Expression(lhs));
                } else {
                    operands.push(Expr::Binary(binary));
                }
            }
            WorkItem::Expression(operand) => operands.push(operand),
        }
    }
    Ok(())
}

fn member_chain_document(
    source: &ParsedSource,
    expression: &Expr,
    context: ExpressionContext,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Option<Doc>, SourceDocumentError> {
    #[derive(Clone, Copy, Eq, PartialEq)]
    enum OperationKind {
        Segment,
        CallSegment,
        AwaitSegment,
        TupleIndexSegment,
        JoinPrevious,
        AttachPrevious,
    }

    struct Operation {
        kind: OperationKind,
        document: Doc,
        forces_chain_break: bool,
    }

    let mut head = expression.clone();
    let mut reverse_operations = Vec::new();

    loop {
        match &head {
            Expr::Call(call) => {
                let callee = call.callee().ok_or(SourceDocumentError::UnsupportedShape)?;
                let Expr::Field(field) = callee else {
                    break;
                };
                let name = field.name().ok_or(SourceDocumentError::UnsupportedShape)?;
                let joins_previous = match direct_token_kinds(name.syntax()).as_slice() {
                    [SyntaxKind::LIDENT] => false,
                    [SyntaxKind::UIDENT] => true,
                    _ => break,
                };
                let arguments = call.args().ok_or(SourceDocumentError::UnsupportedShape)?;
                let base = field.base().ok_or(SourceDocumentError::UnsupportedShape)?;
                if call.syntax().children().count() != 2
                    || !direct_token_kinds(call.syntax()).is_empty()
                {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                let forces_chain_break = call_arguments_force_member_chain_break(
                    source,
                    &arguments,
                    expression_group_ids,
                )?;
                reverse_operations.push(Operation {
                    kind: if joins_previous {
                        OperationKind::JoinPrevious
                    } else {
                        OperationKind::CallSegment
                    },
                    document: Doc::concat([
                        field_suffix_document(source, &field)?,
                        call_arguments_document(source, &arguments, expression_group_ids)?,
                    ]),
                    forces_chain_break,
                });
                head = base;
            }
            Expr::Field(field) => {
                let name = field.name().ok_or(SourceDocumentError::UnsupportedShape)?;
                let kind = match direct_token_kinds(name.syntax()).as_slice() {
                    [SyntaxKind::LIDENT] => OperationKind::Segment,
                    [SyntaxKind::UIDENT] => OperationKind::JoinPrevious,
                    _ => return Err(SourceDocumentError::UnsupportedShape),
                };
                let base = field.base().ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind,
                    document: field_suffix_document(source, field)?,
                    forces_chain_break: false,
                });
                head = base;
            }
            Expr::Await(await_expr) => {
                let operand = await_expr
                    .operand()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind: OperationKind::AwaitSegment,
                    document: await_suffix_document(await_expr)?,
                    forces_chain_break: false,
                });
                head = operand;
            }
            Expr::TupleIndex(tuple_index) => {
                let base = tuple_index
                    .base()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind: OperationKind::TupleIndexSegment,
                    document: tuple_index_suffix_document(tuple_index)?,
                    forces_chain_break: false,
                });
                head = base;
            }
            Expr::Index(index_expr) => {
                let base = index_expr
                    .base()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind: OperationKind::AttachPrevious,
                    document: index_suffix_document(source, index_expr, expression_group_ids)?,
                    forces_chain_break: false,
                });
                head = base;
            }
            Expr::Try(try_expr) => {
                let operand = try_expr
                    .operand()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                reverse_operations.push(Operation {
                    kind: OperationKind::AttachPrevious,
                    document: try_suffix_document(try_expr)?,
                    forces_chain_break: false,
                });
                head = operand;
            }
            _ => break,
        }
    }

    if reverse_operations
        .iter()
        .filter(|operation| operation.kind == OperationKind::CallSegment)
        .count()
        < 2
    {
        return Ok(None);
    }

    reverse_operations.reverse();
    let first_call_segment = reverse_operations
        .iter()
        .position(|operation| operation.kind == OperationKind::CallSegment)
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let first_segment = if matches!(head, Expr::Call(_)) {
        reverse_operations[..first_call_segment]
            .iter()
            .position(|operation| {
                matches!(
                    operation.kind,
                    OperationKind::AwaitSegment | OperationKind::TupleIndexSegment
                )
            })
            .unwrap_or(first_call_segment)
    } else {
        first_call_segment
    };
    let last_segment = reverse_operations
        .iter()
        .enumerate()
        .rfind(|(_, operation)| {
            matches!(
                operation.kind,
                OperationKind::Segment
                    | OperationKind::CallSegment
                    | OperationKind::AwaitSegment
                    | OperationKind::TupleIndexSegment
            )
        })
        .map(|(index, _)| index)
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let head_forces_chain_break = head
        .syntax()
        .descendants()
        .filter_map(ast::ClosureExpr::cast)
        .try_fold(false, |found, closure| {
            if found {
                Ok(true)
            } else {
                closure_body_forces_break(source, &closure, expression_group_ids)
            }
        })?;
    let mut head_suffixes = Vec::new();
    let mut segments = Vec::with_capacity(reverse_operations.len() - first_segment);
    let mut active_segment = None;
    let mut force_chain_break = head_forces_chain_break;
    for (index, operation) in reverse_operations.into_iter().enumerate() {
        if matches!(
            operation.kind,
            OperationKind::Segment
                | OperationKind::CallSegment
                | OperationKind::AwaitSegment
                | OperationKind::TupleIndexSegment
        ) {
            active_segment = Some(index);
        }
        if operation.forces_chain_break && active_segment != Some(last_segment) {
            force_chain_break = true;
        }
        if index < first_segment {
            head_suffixes.push(operation.document);
            continue;
        }
        match operation.kind {
            OperationKind::Segment
            | OperationKind::CallSegment
            | OperationKind::AwaitSegment
            | OperationKind::TupleIndexSegment => {
                segments.push(operation.document);
            }
            OperationKind::JoinPrevious | OperationKind::AttachPrevious => {
                if let Some(previous) = segments.pop() {
                    segments.push(Doc::concat([previous, operation.document]));
                } else {
                    head_suffixes.push(operation.document);
                }
            }
        }
    }

    let mut chain = vec![expression_document(
        source,
        &head,
        context,
        expression_group_ids,
    )?];
    chain.extend(head_suffixes);
    let mut suffixes = Vec::with_capacity(segments.len() * 2);
    let chain_group = expression_group_ids.next();
    for segment in segments {
        suffixes.push(Doc::indent(if force_chain_break {
            Doc::HardLine
        } else {
            Doc::SoftLine
        }));
        suffixes.push(Doc::if_broken(
            chain_group,
            Doc::indent(segment.clone()),
            segment,
        ));
    }
    chain.push(Doc::concat(suffixes));
    Ok(Some(Doc::group(chain_group, Doc::concat(chain))))
}

fn field_suffix_document(
    source: &ParsedSource,
    field: &ast::FieldExpr,
) -> Result<Doc, SourceDocumentError> {
    let name = field.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let generic_args = field.generic_args();
    if field.syntax().children().count() != 2 + usize::from(generic_args.is_some())
        || direct_token_kinds(field.syntax()) != [SyntaxKind::DOT]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("."), Doc::text(node_text(source, name.syntax()))];
    if let Some(generic_args) = generic_args {
        documents.push(generic_args_document(source, &generic_args)?);
    }
    Ok(Doc::concat(documents))
}

fn call_arguments_document(
    source: &ParsedSource,
    arguments: &ast::ArgList,
    expression_group_ids: &mut ExpressionGroupIds,
) -> Result<Doc, SourceDocumentError> {
    let argument_count = arguments.syntax().children().count();
    let mut documents = Vec::with_capacity(argument_count);
    let mut has_trailing_record_update = false;
    let mut final_closure = None;
    for node in arguments.syntax().children() {
        if has_trailing_record_update {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let document = match node.kind() {
            SyntaxKind::ARG => {
                let argument = ast::Arg::cast(node).ok_or(SourceDocumentError::UnsupportedShape)?;
                let expression = argument
                    .expression()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                if argument.syntax().children().count() != 1
                    || !direct_token_kinds(argument.syntax()).is_empty()
                {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                final_closure = match &expression {
                    Expr::Closure(closure) => Some((Doc::text(""), closure.clone())),
                    _ => None,
                };
                expression_document(
                    source,
                    &expression,
                    ExpressionContext::Protected,
                    expression_group_ids,
                )?
            }
            SyntaxKind::NAMED_ARG => {
                let argument =
                    ast::NamedArg::cast(node).ok_or(SourceDocumentError::UnsupportedShape)?;
                let name = argument
                    .name()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                let expression = argument
                    .expression()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                if argument.syntax().children().count() != 2
                    || direct_token_kinds(argument.syntax()) != [SyntaxKind::COLON]
                {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                let prefix =
                    Doc::concat([Doc::text(node_text(source, name.syntax())), Doc::text(": ")]);
                final_closure = match &expression {
                    Expr::Closure(closure) => Some((prefix.clone(), closure.clone())),
                    _ => None,
                };
                Doc::concat([
                    prefix,
                    expression_document(
                        source,
                        &expression,
                        ExpressionContext::Protected,
                        expression_group_ids,
                    )?,
                ])
            }
            SyntaxKind::BASE_ARG => {
                let argument =
                    ast::BaseArg::cast(node).ok_or(SourceDocumentError::UnsupportedShape)?;
                let expression = argument
                    .expression()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                if argument.syntax().children().count() != 1
                    || direct_token_kinds(argument.syntax()) != [SyntaxKind::DOT2]
                {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                has_trailing_record_update = true;
                final_closure = None;
                Doc::concat([
                    Doc::text(".."),
                    expression_document(
                        source,
                        &expression,
                        ExpressionContext::Protected,
                        expression_group_ids,
                    )?,
                ])
            }
            _ => return Err(SourceDocumentError::UnsupportedShape),
        };
        documents.push((document, has_trailing_record_update));
    }
    if !comma_delimited_tokens_match(
        arguments.syntax(),
        SyntaxKind::L_PAREN,
        SyntaxKind::R_PAREN,
        documents.len(),
    ) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if documents.is_empty() {
        return Ok(Doc::text("()"));
    }

    let range = arguments.syntax().text_range();
    let group = GroupId(range.start().raw());
    let mut contents = Vec::with_capacity(documents.len() * 2 + 2);
    contents.push(Doc::SoftLine);
    for (index, (document, is_record_update)) in documents.iter().enumerate() {
        if index > 0 {
            contents.push(Doc::text(","));
            contents.push(Doc::Line);
        }
        contents.push(document.clone());
        if index + 1 == documents.len() && !is_record_update {
            contents.push(Doc::if_broken(group, Doc::text(","), Doc::text("")));
        }
    }
    let ordinary = Doc::group(
        group,
        Doc::concat([
            Doc::text("("),
            Doc::indent(Doc::concat(contents)),
            Doc::SoftLine,
            Doc::text(")"),
        ]),
    );
    let Some((closure_prefix, closure)) = final_closure else {
        return Ok(ordinary);
    };

    let mut alternatives = Vec::with_capacity(3);
    if flat_document_width(&ordinary).is_some() {
        alternatives.push(Doc::flat(first_choice_layout(&ordinary)));
    }

    let closure_header = closure_header_document(source, &closure, &[])?;
    let closure_body = closure
        .body()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if flat_document_width(&closure_header).is_some()
        && documents[..documents.len() - 1]
            .iter()
            .all(|(document, _)| flat_document_width(document).is_some())
    {
        let closure_body = block_document(source, &closure_body, expression_group_ids)?;
        let mut hugged = vec![Doc::text("(")];
        for (index, (document, _)) in documents[..documents.len() - 1].iter().enumerate() {
            if index > 0 {
                hugged.push(Doc::text(", "));
            }
            hugged.push(Doc::flat(first_choice_layout(document)));
        }
        if documents.len() > 1 {
            hugged.push(Doc::text(", "));
        }
        hugged.extend([
            closure_prefix,
            Doc::flat(first_choice_layout(&closure_header)),
            Doc::text(" "),
            closure_body,
            Doc::text(")"),
        ]);
        alternatives.push(Doc::concat(hugged));
    }
    alternatives.push(ordinary);
    Ok(Doc::choice(alternatives))
}

fn generic_params_document(
    source: &ParsedSource,
    params: &ast::GenericParamList,
) -> Result<Doc, SourceDocumentError> {
    let parameters = params.params().collect::<Vec<_>>();
    let token_kinds = direct_token_kinds(params.syntax());
    if parameters.is_empty()
        || params.syntax().children().count() != parameters.len()
        || token_kinds.first() != Some(&SyntaxKind::LT)
        || token_kinds.last() != Some(&SyntaxKind::GT)
        || token_kinds[1..token_kinds.len() - 1]
            .iter()
            .any(|kind| !matches!(kind, SyntaxKind::COMMA | SyntaxKind::NEWLINE))
        || token_kinds
            .iter()
            .filter(|kind| **kind == SyntaxKind::COMMA)
            .count()
            < parameters.len() - 1
        || token_kinds
            .iter()
            .filter(|kind| **kind == SyntaxKind::COMMA)
            .count()
            > parameters.len()
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = parameters
        .into_iter()
        .map(|parameter| generic_param_document(source, parameter))
        .collect::<Result<Vec<_>, _>>()?;
    let range = params.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "<",
        ">",
        documents,
        false,
    ))
}

fn generic_param_document(
    source: &ParsedSource,
    parameter: ast::GenericParam,
) -> Result<Doc, SourceDocumentError> {
    match parameter {
        ast::GenericParam::Type(parameter) => {
            let bounds = parameter
                .bounds()
                .map(|bounds| bound_list_document(source, &bounds))
                .transpose()?;
            let expected_tokens = if bounds.is_some() {
                vec![SyntaxKind::COLON]
            } else {
                Vec::new()
            };
            if parameter.attrs().next().is_some()
                || parameter.syntax().children().count() != 1 + usize::from(bounds.is_some())
                || direct_token_kinds(parameter.syntax()) != expected_tokens
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let name = parameter
                .name()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let mut documents = vec![Doc::text(node_text(source, name.syntax()))];
            if let Some(bounds) = bounds {
                documents.extend([Doc::text(": "), bounds]);
            }
            Ok(Doc::concat(documents))
        }
        ast::GenericParam::Const(parameter) => {
            if parameter.attrs().next().is_some()
                || parameter.syntax().children().count() != 2
                || direct_token_kinds(parameter.syntax())
                    != [SyntaxKind::CONST_KW, SyntaxKind::COLON]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let name = parameter
                .name()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let ty = parameter
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            Ok(Doc::concat([
                Doc::text("const "),
                Doc::text(node_text(source, name.syntax())),
                Doc::text(": "),
                type_document(source, &ty)?,
            ]))
        }
    }
}

fn bound_list_document(
    source: &ParsedSource,
    bound_list: &ast::BoundList,
) -> Result<Doc, SourceDocumentError> {
    let bounds = bound_list.bounds().collect::<Vec<_>>();
    let syntax = bound_list.syntax();
    if bounds.is_empty()
        || syntax.children().count() != bounds.len()
        || direct_token_kinds(syntax) != vec![SyntaxKind::PLUS; bounds.len() - 1]
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let documents = bounds
        .into_iter()
        .map(|bound| bound_document(source, bound))
        .collect::<Result<Vec<_>, _>>()?;
    let range = syntax.text_range();
    let mut list = Vec::with_capacity(documents.len() * 2 - 1);
    for (index, bound) in documents.into_iter().enumerate() {
        if index > 0 {
            list.extend([
                Doc::text(" "),
                Doc::text("+"),
                Doc::indent(Doc::concat([Doc::Line, bound])),
            ]);
        } else {
            list.push(bound);
        }
    }
    Ok(Doc::group(GroupId(range.start().raw()), Doc::concat(list)))
}

fn bound_document(source: &ParsedSource, bound: ast::Bound) -> Result<Doc, SourceDocumentError> {
    match bound {
        ast::Bound::Trait(bound) => {
            let path = bound.path().ok_or(SourceDocumentError::UnsupportedShape)?;
            let effects = bound.effects();
            if bound.syntax().children().count() != 1 + usize::from(effects.is_some())
                || !direct_token_kinds(bound.syntax()).is_empty()
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let mut documents = vec![type_document(source, &ast::Type::PathType(path))?];
            if let Some(effects) = effects {
                documents.extend([Doc::text(" "), effects_clause_document(source, &effects)?]);
            }
            Ok(Doc::concat(documents))
        }
        ast::Bound::FnSugar(bound) => fn_sugar_bound_document(source, &bound),
    }
}

fn fn_sugar_bound_document(
    source: &ParsedSource,
    bound: &ast::FnSugarBound,
) -> Result<Doc, SourceDocumentError> {
    let name = bound.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let parameters = bound.params().collect::<Vec<_>>();
    let return_type = bound.ret_type();
    let effects = bound.effects();
    let mut expected_tokens = vec![SyntaxKind::L_PAREN];
    expected_tokens.extend(std::iter::repeat_n(
        SyntaxKind::COMMA,
        parameters.len().saturating_sub(1),
    ));
    expected_tokens.push(SyntaxKind::R_PAREN);
    if return_type.is_some() {
        expected_tokens.push(SyntaxKind::THIN_ARROW);
    }
    let direct_tokens = direct_token_kinds(bound.syntax());
    if !parameters.is_empty() && direct_tokens.get(parameters.len()) == Some(&SyntaxKind::COMMA) {
        expected_tokens.insert(parameters.len(), SyntaxKind::COMMA);
    }
    if bound.syntax().children().count()
        != 1 + parameters.len()
            + usize::from(return_type.is_some())
            + usize::from(effects.is_some())
        || direct_tokens != expected_tokens
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let parameter_documents = parameters
        .iter()
        .map(|parameter| type_document(source, parameter))
        .collect::<Result<Vec<_>, _>>()?;
    let range = bound.syntax().text_range();
    let mut documents = vec![
        Doc::text(node_text(source, name.syntax())),
        delimited_list_document(
            GroupId(range.start().raw()),
            "(",
            ")",
            parameter_documents,
            false,
        ),
    ];
    if let Some(return_type) = return_type {
        documents.extend([Doc::text(" -> "), type_document(source, &return_type)?]);
    }
    if let Some(effects) = effects {
        documents.extend([Doc::text(" "), effects_clause_document(source, &effects)?]);
    }
    Ok(Doc::concat(documents))
}

fn flat_document_width(document: &Doc) -> Option<usize> {
    let mut width = 0usize;
    let mut pending = vec![document];
    while let Some(document) = pending.pop() {
        let added_width = match document {
            Doc::Text(text) => text.chars().count(),
            Doc::Line => 1,
            Doc::SoftLine | Doc::LineSuffix(_) => 0,
            Doc::HardLine | Doc::HardLineCrlf => return None,
            Doc::Indent(child) | Doc::Group(_, child) | Doc::Flat(child) => {
                pending.push(child);
                continue;
            }
            Doc::IfBroken(_, _, flat) => {
                pending.push(flat);
                continue;
            }
            Doc::Choice(alternatives) => {
                pending.push(alternatives.first()?);
                continue;
            }
            Doc::Verbatim(text) if !text.contains(['\r', '\n']) => text.chars().count(),
            Doc::Verbatim(_) => return None,
            Doc::VerbatimExact(text) if !text.contains(['\r', '\n']) => text.chars().count(),
            Doc::VerbatimExact(_) => return None,
            Doc::Concat(documents) => {
                pending.extend(documents.iter().rev());
                continue;
            }
        };
        width = width.checked_add(added_width)?;
    }
    Some(width)
}

fn parameter_list_document(
    source: &ParsedSource,
    params: &ast::ParamList,
) -> Result<Doc, SourceDocumentError> {
    let mut parameters = Vec::new();
    let mut self_params = params.syntax().children().filter_map(ast::SelfParam::cast);
    let self_param = self_params.next();
    if self_params.next().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if let Some(self_param) = self_param {
        if params.params().next().is_some_and(|param| {
            self_param.syntax().text_range().start().raw()
                > param.syntax().text_range().start().raw()
        }) {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        parameters.push(self_parameter_document(self_param)?);
    }
    parameters.extend(
        params
            .params()
            .map(|param| parameter_document(source, param))
            .collect::<Result<Vec<_>, _>>()?,
    );
    if parameters.is_empty() {
        return Ok(Doc::text("()"));
    }

    let group = GroupId(params.syntax().text_range().start().raw());
    Ok(delimited_list_document(group, "(", ")", parameters, false))
}

fn parameter_document(
    source: &ParsedSource,
    param: ast::Param,
) -> Result<Doc, SourceDocumentError> {
    if param.attrs().next().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let name = param.name().ok_or(SourceDocumentError::UnsupportedShape)?;
    let ty = param.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
    let type_doc = type_document(source, &ty)?;
    let mutable = if param.is_mutable() { "var " } else { "" };
    Ok(Doc::concat([
        Doc::text(format!("{mutable}{}: ", node_text(source, name.syntax()))),
        type_doc,
    ]))
}

fn self_parameter_document(param: ast::SelfParam) -> Result<Doc, SourceDocumentError> {
    if param.attrs().next().is_some() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let ty = param.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
    let type_name = match ty {
        ast::Type::SelfType(self_type) if self_type.segments().next().is_none() => {
            "Self".to_owned()
        }
        ast::Type::BorrowType(borrow_type)
            if matches!(
                borrow_type.ty(),
                Some(ast::Type::SelfType(self_type)) if self_type.segments().next().is_none()
            ) =>
        {
            let mutable = if borrow_type.mut_token().is_some() {
                "mut "
            } else {
                ""
            };
            format!("borrow {mutable}Self")
        }
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    let mutable = if param.is_mutable() { "var " } else { "" };
    Ok(Doc::text(format!("{mutable}self: {type_name}")))
}

fn type_document(source: &ParsedSource, ty: &ast::Type) -> Result<Doc, SourceDocumentError> {
    match ty {
        ast::Type::PathType(path) => path_type_document(source, path),
        ast::Type::SelfType(self_type) => self_type_document(source, self_type),
        ast::Type::BorrowType(borrow_type) => {
            if borrow_type.syntax().children().count() != 1 {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let prefix = match direct_token_kinds(borrow_type.syntax()).as_slice() {
                [SyntaxKind::BORROW_KW] => "borrow",
                [SyntaxKind::BORROW_KW, SyntaxKind::MUT_KW] => "borrow mut",
                _ => return Err(SourceDocumentError::UnsupportedShape),
            };
            let inner = borrow_type
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let range = borrow_type.syntax().text_range();
            Ok(Doc::group(
                GroupId(range.start().raw()),
                Doc::concat([
                    Doc::text(prefix),
                    Doc::indent(Doc::concat([Doc::Line, type_document(source, &inner)?])),
                ]),
            ))
        }
        ast::Type::OwnedType(owned_type) => {
            if owned_type.syntax().children().count() != 1
                || direct_token_kinds(owned_type.syntax()) != [SyntaxKind::OWNED_KW]
            {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let inner = owned_type
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let range = owned_type.syntax().text_range();
            Ok(Doc::group(
                GroupId(range.start().raw()),
                Doc::concat([
                    Doc::text("owned"),
                    Doc::indent(Doc::concat([Doc::Line, type_document(source, &inner)?])),
                ]),
            ))
        }
        ast::Type::TupleType(tuple_type) => {
            let elements = tuple_type.elements().collect::<Vec<_>>();
            if tuple_type.syntax().children().count() != elements.len() {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            if elements.is_empty() {
                return Ok(Doc::text("()"));
            }
            let documents = elements
                .iter()
                .map(|element| type_document(source, element))
                .collect::<Result<Vec<_>, _>>()?;
            let range = tuple_type.syntax().text_range();
            Ok(delimited_list_document(
                GroupId(range.start().raw()),
                "(",
                ")",
                documents,
                elements.len() == 1,
            ))
        }
        ast::Type::ParenType(paren_type) => {
            if paren_type.syntax().children().count() != 1 {
                return Err(SourceDocumentError::UnsupportedShape);
            }
            let inner = paren_type
                .ty()
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            let range = paren_type.syntax().text_range();
            Ok(Doc::group(
                GroupId(range.start().raw()),
                Doc::concat([
                    Doc::text("("),
                    Doc::indent(Doc::concat([Doc::SoftLine, type_document(source, &inner)?])),
                    Doc::SoftLine,
                    Doc::text(")"),
                ]),
            ))
        }
        ast::Type::InferType(infer_type) if infer_type.syntax().children().next().is_none() => {
            Ok(Doc::text("_"))
        }
        ast::Type::FnPtrType(fn_ptr_type) => fn_ptr_type_document(source, fn_ptr_type),
        ast::Type::InferType(_) => Err(SourceDocumentError::UnsupportedShape),
    }
}

fn path_type_document(
    source: &ParsedSource,
    path: &ast::PathType,
) -> Result<Doc, SourceDocumentError> {
    let segments = path.segments().collect::<Vec<_>>();
    if segments.is_empty() || path.syntax().children().count() != segments.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = Vec::with_capacity(segments.len() * 2);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            documents.push(Doc::text("."));
        }
        let name = segment
            .name()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        documents.push(Doc::text(node_text(source, name.syntax())));
        if let Some(args) = segment.generic_args() {
            documents.push(generic_args_document(source, &args)?);
        }
    }
    Ok(Doc::concat(documents))
}

fn self_type_document(
    source: &ParsedSource,
    self_type: &ast::SelfType,
) -> Result<Doc, SourceDocumentError> {
    let segments = self_type.segments().collect::<Vec<_>>();
    if self_type.syntax().children().count() != segments.len()
        || segments
            .iter()
            .any(|segment| segment.generic_args().is_some())
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::text("Self")];
    for segment in segments {
        let name = segment
            .name()
            .ok_or(SourceDocumentError::UnsupportedShape)?;
        documents.push(Doc::text("."));
        documents.push(Doc::text(node_text(source, name.syntax())));
    }
    Ok(Doc::concat(documents))
}

fn generic_args_document(
    source: &ParsedSource,
    args: &ast::GenericArgList,
) -> Result<Doc, SourceDocumentError> {
    let children = args.syntax().children().collect::<Vec<_>>();
    if children.is_empty() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = children
        .into_iter()
        .map(|child| {
            if let Some(type_arg) = ast::TypeArg::cast(child.clone()) {
                if type_arg.syntax().children().count() != 1 {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                let ty = type_arg.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
                type_document(source, &ty)
            } else if let Some(const_arg) = ast::ConstArg::cast(child.clone()) {
                const_argument_document(&const_arg)
            } else if let Some(binding) = ast::AssocTypeBinding::cast(child) {
                if binding.syntax().children().count() != 2 {
                    return Err(SourceDocumentError::UnsupportedShape);
                }
                let name = binding
                    .name()
                    .ok_or(SourceDocumentError::UnsupportedShape)?;
                let ty = binding.ty().ok_or(SourceDocumentError::UnsupportedShape)?;
                Ok(Doc::concat([
                    Doc::text(node_text(source, name.syntax())),
                    Doc::text(" = "),
                    type_document(source, &ty)?,
                ]))
            } else {
                Err(SourceDocumentError::UnsupportedShape)
            }
        })
        .collect::<Result<Vec<_>, _>>()?;
    let range = args.syntax().text_range();
    Ok(delimited_list_document(
        GroupId(range.start().raw()),
        "<",
        ">",
        documents,
        false,
    ))
}

fn const_argument_document(arg: &ast::ConstArg) -> Result<Doc, SourceDocumentError> {
    let tokens = arg
        .syntax()
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) => Some(token),
            SyntaxElement::Node(_) => None,
        })
        .collect::<Vec<_>>();
    let text = match tokens.as_slice() {
        [integer] if integer.kind() == SyntaxKind::INT => normalize_integer_literal(integer.text()),
        [minus, integer]
            if minus.kind() == SyntaxKind::MINUS && integer.kind() == SyntaxKind::INT =>
        {
            format!("-{}", normalize_integer_literal(integer.text()))
        }
        [character] if character.kind() == SyntaxKind::CHAR => character.text().to_owned(),
        [boolean] if matches!(boolean.kind(), SyntaxKind::TRUE_KW | SyntaxKind::FALSE_KW) => {
            boolean.text().to_owned()
        }
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    Ok(Doc::text(text))
}

fn direct_token_kinds(node: &sai_syntax::SyntaxNode) -> Vec<SyntaxKind> {
    node.children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) => Some(token.kind()),
            SyntaxElement::Node(_) => None,
        })
        .collect()
}

fn has_contextual_token(
    source: &ParsedSource,
    node: &sai_syntax::SyntaxNode,
    spelling: &str,
) -> Result<bool, SourceDocumentError> {
    let tokens = node
        .children_with_tokens()
        .filter_map(|element| match element {
            SyntaxElement::Token(token) if token.kind() != SyntaxKind::NEWLINE => Some(token),
            SyntaxElement::Token(_) | SyntaxElement::Node(_) => None,
        })
        .collect::<Vec<_>>();
    let [token] = tokens.as_slice() else {
        return Ok(false);
    };
    Ok(token.kind() == SyntaxKind::LIDENT && token_text(source, token)? == spelling)
}

fn comma_delimited_tokens_match(
    node: &sai_syntax::SyntaxNode,
    open: SyntaxKind,
    close: SyntaxKind,
    item_count: usize,
) -> bool {
    let direct_tokens = direct_token_kinds(node)
        .into_iter()
        .filter(|kind| *kind != SyntaxKind::NEWLINE)
        .collect::<Vec<_>>();
    let mut expected = vec![open];
    expected.extend(std::iter::repeat_n(
        SyntaxKind::COMMA,
        item_count.saturating_sub(1),
    ));
    expected.push(close);
    if item_count > 0 && direct_tokens.get(item_count) == Some(&SyntaxKind::COMMA) {
        expected.insert(item_count, SyntaxKind::COMMA);
    }
    direct_tokens == expected
}

fn delimited_list_document(
    group: GroupId,
    open: &str,
    close: &str,
    items: Vec<Doc>,
    keep_singleton_comma: bool,
) -> Doc {
    delimited_list_document_with_options(group, open, close, items, true, keep_singleton_comma)
}

fn delimited_list_document_with_options(
    group: GroupId,
    open: &str,
    close: &str,
    items: Vec<Doc>,
    trailing_comma_when_broken: bool,
    keep_singleton_comma: bool,
) -> Doc {
    delimited_list_document_with_options_and_opening_suffix(
        group,
        open,
        close,
        items,
        trailing_comma_when_broken,
        keep_singleton_comma,
        Vec::new(),
    )
}

fn delimited_list_document_with_options_and_opening_suffix(
    group: GroupId,
    open: &str,
    close: &str,
    items: Vec<Doc>,
    trailing_comma_when_broken: bool,
    keep_singleton_comma: bool,
    opening_suffix: Vec<Doc>,
) -> Doc {
    delimited_list_document_with_options_and_suffixes(
        group,
        open,
        close,
        items,
        trailing_comma_when_broken,
        keep_singleton_comma,
        DelimitedListSuffixes {
            opening: opening_suffix,
            separators: Vec::new(),
        },
    )
}

struct DelimitedListSuffixes {
    opening: Vec<Doc>,
    separators: Vec<Vec<Doc>>,
}

fn delimited_list_document_with_options_and_suffixes(
    group: GroupId,
    open: &str,
    close: &str,
    items: Vec<Doc>,
    trailing_comma_when_broken: bool,
    keep_singleton_comma: bool,
    suffixes: DelimitedListSuffixes,
) -> Doc {
    if items.is_empty() {
        return Doc::concat([
            Doc::text(open),
            Doc::concat(suffixes.opening),
            Doc::text(close),
        ]);
    }
    let mut contents = Vec::with_capacity(items.len() * 2 + 2);
    contents.push(if suffixes.opening.is_empty() {
        Doc::SoftLine
    } else {
        Doc::Line
    });
    for (index, item) in items.iter().cloned().enumerate() {
        if index > 0 {
            contents.push(Doc::text(","));
            if let Some(suffix) = suffixes.separators.get(index - 1) {
                contents.extend(suffix.iter().cloned());
            }
            contents.push(Doc::Line);
        }
        contents.push(item);
    }
    contents.push(Doc::if_broken(
        group,
        Doc::text(if trailing_comma_when_broken { "," } else { "" }),
        Doc::text(if keep_singleton_comma && items.len() == 1 {
            ","
        } else {
            ""
        }),
    ));
    Doc::group(
        group,
        Doc::concat([
            Doc::text(open),
            Doc::concat(suffixes.opening),
            Doc::indent(Doc::concat(contents)),
            Doc::SoftLine,
            Doc::text(close),
        ]),
    )
}

fn fn_ptr_type_document(
    source: &ParsedSource,
    fn_ptr_type: &ast::FnPtrType,
) -> Result<Doc, SourceDocumentError> {
    let params = fn_ptr_type.params().collect::<Vec<_>>();
    let return_type = fn_ptr_type.ret_type();
    let effects = fn_ptr_type.effects();
    if fn_ptr_type.syntax().children().count()
        != params.len() + return_type.is_some() as usize + effects.is_some() as usize
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let token_kinds = direct_token_kinds(fn_ptr_type.syntax());
    let unsafe_prefix = token_kinds.first() == Some(&SyntaxKind::UNSAFE_KW);
    let prefix = if unsafe_prefix {
        &[
            SyntaxKind::UNSAFE_KW,
            SyntaxKind::FN_KW,
            SyntaxKind::L_PAREN,
        ][..]
    } else {
        &[SyntaxKind::FN_KW, SyntaxKind::L_PAREN][..]
    };
    if !token_kinds.starts_with(prefix)
        || token_kinds.iter().any(|kind| {
            !matches!(
                kind,
                SyntaxKind::UNSAFE_KW
                    | SyntaxKind::FN_KW
                    | SyntaxKind::L_PAREN
                    | SyntaxKind::R_PAREN
                    | SyntaxKind::COMMA
                    | SyntaxKind::THIN_ARROW
                    | SyntaxKind::NEWLINE
            )
        })
        || return_type.is_some() != token_kinds.contains(&SyntaxKind::THIN_ARROW)
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let params = params
        .iter()
        .map(|param| type_document(source, param))
        .collect::<Result<Vec<_>, _>>()?;
    let range = fn_ptr_type.syntax().text_range();
    let params = if params.is_empty() {
        Doc::text("()")
    } else {
        delimited_list_document(GroupId(range.start().raw()), "(", ")", params, false)
    };
    let mut documents = vec![
        Doc::text(if unsafe_prefix { "unsafe fn" } else { "fn" }),
        params,
    ];
    if let Some(return_type) = return_type {
        documents.push(Doc::text(" -> "));
        documents.push(type_document(source, &return_type)?);
    }
    if let Some(effects) = effects {
        documents.push(Doc::text(" "));
        documents.push(effects_clause_document(source, &effects)?);
    }
    Ok(Doc::concat(documents))
}

fn effects_clause_document(
    source: &ParsedSource,
    clause: &ast::EffectsClause,
) -> Result<Doc, SourceDocumentError> {
    let has_effects_keyword = clause
        .syntax()
        .children_with_tokens()
        .any(|element| matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::LIDENT && token.text() == "effects"));
    if clause.syntax().children().count() != 1
        || direct_token_kinds(clause.syntax()) != [SyntaxKind::LIDENT]
        || !has_effects_keyword
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let set = clause.set().ok_or(SourceDocumentError::UnsupportedShape)?;
    effect_set_document(source, &set).map(|set| Doc::concat([Doc::text("effects "), set]))
}

fn borrows_clause_document(
    source: &ParsedSource,
    clause: &ast::BorrowsClause,
) -> Result<Doc, SourceDocumentError> {
    let has_borrows_keyword = clause
        .syntax()
        .children_with_tokens()
        .any(|element| matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::LIDENT && token.text() == "borrows"));
    let provenances = clause.provenances().collect::<Vec<_>>();
    let token_kinds = direct_token_kinds(clause.syntax());
    if clause.syntax().children().count() != provenances.len()
        || token_kinds.first() != Some(&SyntaxKind::LIDENT)
        || token_kinds.get(1) != Some(&SyntaxKind::L_BRACE)
        || token_kinds.last() != Some(&SyntaxKind::R_BRACE)
        || token_kinds[2..token_kinds.len() - 1]
            .iter()
            .any(|kind| !matches!(kind, SyntaxKind::COMMA | SyntaxKind::NEWLINE))
        || !has_borrows_keyword
    {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let documents = provenances
        .iter()
        .map(|provenance| provenance_document(source, provenance))
        .collect::<Result<Vec<_>, _>>()?;
    let range = clause.syntax().text_range();
    Ok(Doc::concat([
        Doc::text("borrows "),
        brace_list_document(GroupId(range.start().raw()), documents),
    ]))
}

fn provenance_document(
    source: &ParsedSource,
    provenance: &ast::Provenance,
) -> Result<Doc, SourceDocumentError> {
    let segments = provenance.segments().collect::<Vec<_>>();
    let token_kinds = direct_token_kinds(provenance.syntax());
    if segments.is_empty() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let expected_tokens =
        std::iter::repeat_n(SyntaxKind::DOT, segments.len() - 1).collect::<Vec<_>>();
    if token_kinds != expected_tokens {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = Vec::with_capacity(segments.len() * 2 - 1);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            documents.push(Doc::text("."));
        }
        documents.push(Doc::text(node_text(source, segment.syntax())));
    }
    Ok(Doc::concat(documents))
}

fn effect_set_document(
    source: &ParsedSource,
    set: &ast::EffectSet,
) -> Result<Doc, SourceDocumentError> {
    let effects = set.effects().collect::<Vec<_>>();
    if set.syntax().children().count() != effects.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    if effects.is_empty() {
        return Ok(Doc::text("{}"));
    }
    let documents = effects
        .iter()
        .map(|effect| effect_ref_document(source, effect))
        .collect::<Result<Vec<_>, _>>()?;
    let range = set.syntax().text_range();
    Ok(brace_list_document(GroupId(range.start().raw()), documents))
}

fn brace_list_document(group: GroupId, items: Vec<Doc>) -> Doc {
    brace_list_document_with_forced_break(group, items, false)
}

fn brace_list_document_with_forced_break(
    group: GroupId,
    items: Vec<Doc>,
    force_break: bool,
) -> Doc {
    brace_list_document_with_separator_suffixes(group, items, force_break, &[], &[])
}

fn brace_list_document_with_separator_suffixes(
    group: GroupId,
    items: Vec<Doc>,
    force_break: bool,
    separator_suffixes: &[Vec<Doc>],
    closing_suffix: &[Doc],
) -> Doc {
    if items.is_empty() {
        if closing_suffix.is_empty() {
            return Doc::text("{}");
        }
        return Doc::group(
            group,
            Doc::concat([
                Doc::text("{"),
                Doc::indent(Doc::concat(closing_suffix.to_vec())),
                Doc::HardLine,
                Doc::text("}"),
            ]),
        );
    }
    if force_break {
        let mut contents = vec![Doc::HardLine];
        for (index, item) in items.into_iter().enumerate() {
            if index > 0 {
                contents.push(Doc::HardLine);
            }
            contents.push(item);
            if let Some(suffix) = separator_suffixes.get(index)
                && !suffix.is_empty()
            {
                contents.push(Doc::text(","));
                contents.push(Doc::concat(suffix.clone()));
            }
        }
        contents.extend_from_slice(closing_suffix);
        return Doc::group(
            group,
            Doc::concat([
                Doc::text("{"),
                Doc::indent(Doc::concat(contents)),
                Doc::HardLine,
                Doc::text("}"),
            ]),
        );
    }

    let item_count = items.len();
    let mut contents = Vec::with_capacity(items.len() * 2 + 1);
    contents.push(Doc::Line);
    for (index, item) in items.into_iter().enumerate() {
        if index > 0 {
            let separator_index = index - 1;
            if let Some(suffix) = separator_suffixes
                .get(separator_index)
                .filter(|suffix| !suffix.is_empty())
            {
                contents.push(Doc::text(","));
                contents.push(Doc::concat(suffix.clone()));
            } else {
                contents.push(Doc::if_broken(group, Doc::text(""), Doc::text(",")));
            }
            contents.push(Doc::Line);
        }
        contents.push(item);
        if index + 1 == item_count
            && let Some(suffix) = separator_suffixes
                .get(index)
                .filter(|suffix| !suffix.is_empty())
        {
            contents.push(Doc::text(","));
            contents.push(Doc::concat(suffix.clone()));
        }
    }
    contents.extend_from_slice(closing_suffix);
    Doc::group(
        group,
        Doc::concat([
            Doc::text("{"),
            Doc::indent(Doc::concat(contents)),
            Doc::Line,
            Doc::text("}"),
        ]),
    )
}

fn effect_ref_document(
    source: &ParsedSource,
    effect: &ast::EffectRef,
) -> Result<Doc, SourceDocumentError> {
    let segments = effect.segments().collect::<Vec<_>>();
    if segments.is_empty() || effect.syntax().children().count() != segments.len() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = Vec::with_capacity(segments.len() * 2 - 1);
    for (index, segment) in segments.iter().enumerate() {
        if index > 0 {
            documents.push(Doc::text("."));
        }
        documents.push(Doc::text(node_text(source, segment.syntax())));
    }
    Ok(Doc::concat(documents))
}

fn node_text<'a>(source: &'a ParsedSource, node: &sai_syntax::SyntaxNode) -> &'a str {
    let range = node.text_range();
    source
        .text()
        .get(range.start().to_usize()..range.end().to_usize())
        .unwrap_or_default()
}

fn token_text<'a>(
    source: &'a ParsedSource,
    token: &SyntaxToken,
) -> Result<&'a str, SourceDocumentError> {
    let range = token.text_range();
    source
        .text()
        .get(range.start().to_usize()..range.end().to_usize())
        .ok_or(SourceDocumentError::UnsupportedShape)
}

pub(super) fn normalize_integer_literal(literal: &str) -> String {
    if let Some(digits) = literal.strip_prefix("0x") {
        format!("0x{}", digits.to_ascii_uppercase())
    } else {
        literal.to_owned()
    }
}

fn normalize_literal(
    source: &ParsedSource,
    literal: LiteralToken,
) -> Result<String, SourceDocumentError> {
    let token = literal.syntax();
    let range = token.text_range();
    let text = source
        .text()
        .get(range.start().to_usize()..range.end().to_usize())
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    if text.contains(['\r', '\n']) {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    Ok(match literal {
        LiteralToken::Int(_) => normalize_integer_literal(text),
        LiteralToken::Float(_) => text.replace('E', "e"),
        LiteralToken::String(_)
        | LiteralToken::ByteString(_)
        | LiteralToken::Char(_)
        | LiteralToken::Byte(_)
        | LiteralToken::True(_)
        | LiteralToken::False(_) => text.to_owned(),
    })
}

fn literal_document(
    source: &ParsedSource,
    literal: LiteralToken,
) -> Result<Doc, SourceDocumentError> {
    let text = token_text(source, literal.syntax())?;
    if !text.contains(['\n', '\r']) {
        return normalize_literal(source, literal).map(Doc::text);
    }
    match literal {
        LiteralToken::String(_) if text.starts_with("\"\"\"") => multiline_string_document(text),
        LiteralToken::String(_) if text.starts_with('r') => Ok(Doc::VerbatimExact(text.to_owned())),
        LiteralToken::String(_) if !text.starts_with('r') => continued_string_document(text),
        LiteralToken::ByteString(_) => Ok(Doc::VerbatimExact(text.to_owned())),
        _ => Err(SourceDocumentError::UnsupportedShape),
    }
}

fn multiline_string_document(text: &str) -> Result<Doc, SourceDocumentError> {
    let lines = split_string_lines(text);
    if lines.len() < 2 || lines[0].0 != "\"\"\"" || lines[0].1.is_empty() {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let (closing_line, closing_ending) = lines
        .last()
        .copied()
        .ok_or(SourceDocumentError::UnsupportedShape)?;
    let base_indent_length =
        closing_line.len() - closing_line.trim_start_matches([' ', '\t']).len();
    let base_indent = &closing_line[..base_indent_length];
    if &closing_line[base_indent_length..] != "\"\"\"" || !closing_ending.is_empty() {
        return Err(SourceDocumentError::UnsupportedShape);
    }

    let mut documents = vec![Doc::Verbatim(lines[0].0.to_owned())];
    for index in 1..lines.len() - 1 {
        let (line, _) = lines[index];
        let previous_ending = lines[index - 1].1;
        if line.is_empty() || line == base_indent {
            documents.push(string_line_break(previous_ending, None)?);
        } else {
            let content = line
                .strip_prefix(base_indent)
                .ok_or(SourceDocumentError::UnsupportedShape)?;
            documents.push(string_line_break(previous_ending, Some(content))?);
        }
    }
    documents.push(string_line_break(lines[lines.len() - 2].1, Some("\"\"\""))?);
    Ok(Doc::concat(documents))
}

fn continued_string_document(text: &str) -> Result<Doc, SourceDocumentError> {
    let lines = split_string_lines(text);
    if lines.len() < 2 {
        return Err(SourceDocumentError::UnsupportedShape);
    }
    let mut documents = vec![Doc::Verbatim(lines[0].0.to_owned())];
    for index in 1..lines.len() {
        if !lines[index - 1].0.ends_with('\\') || lines[index - 1].1.is_empty() {
            return Err(SourceDocumentError::UnsupportedShape);
        }
        let content = lines[index].0.trim_start_matches([' ', '\t']);
        documents.push(Doc::indent(Doc::concat([
            Doc::HardLine,
            Doc::Verbatim(content.to_owned()),
        ])));
    }
    Ok(Doc::concat(documents))
}

pub(super) fn split_string_lines(text: &str) -> Vec<(&str, &str)> {
    let mut lines = Vec::new();
    let mut line_start = 0;
    for (line_feed, character) in text.char_indices() {
        if character != '\n' {
            continue;
        }
        let content_end = if line_feed > line_start && text.as_bytes()[line_feed - 1] == b'\r' {
            line_feed - 1
        } else {
            line_feed
        };
        lines.push((
            &text[line_start..content_end],
            &text[content_end..line_feed + 1],
        ));
        line_start = line_feed + 1;
    }
    if line_start < text.len() || lines.is_empty() {
        lines.push((&text[line_start..], ""));
    }
    lines
}

pub(super) fn normalized_string_value(text: &str) -> Option<String> {
    let lines = split_string_lines(text);
    if !text.starts_with("\"\"\"") {
        if lines.len() < 2 || text.starts_with('r') {
            return None;
        }
        let mut value = lines.first()?.0.to_string();
        for line in lines.iter().skip(1) {
            if !value.ends_with('\\') {
                return None;
            }
            value.pop();
            value.push_str(line.0.trim_start_matches([' ', '\t']));
        }
        return Some(value);
    }

    if lines.len() < 2 || lines.first()?.0 != "\"\"\"" {
        return None;
    }
    let closing_line = lines.last()?.0;
    let base_indent_length =
        closing_line.len() - closing_line.trim_start_matches([' ', '\t']).len();
    let base_indent = &closing_line[..base_indent_length];
    if &closing_line[base_indent_length..] != "\"\"\"" {
        return None;
    }
    let mut value_lines = Vec::with_capacity(lines.len().saturating_sub(2));
    for index in 1..lines.len() - 1 {
        let line = lines[index].0;
        if line.is_empty() || line == base_indent {
            value_lines.push("");
        } else {
            value_lines.push(line.strip_prefix(base_indent)?);
        }
        if index + 1 < lines.len() - 1 {
            value_lines.push(lines[index].1);
        }
    }
    Some(value_lines.concat())
}

fn string_line_break(ending: &str, content: Option<&str>) -> Result<Doc, SourceDocumentError> {
    let line_break = match ending {
        "\n" => Doc::HardLine,
        "\r\n" => Doc::HardLineCrlf,
        _ => return Err(SourceDocumentError::UnsupportedShape),
    };
    match content {
        Some(content) => Ok(Doc::indent(Doc::concat([
            line_break,
            Doc::Verbatim(content.to_owned()),
        ]))),
        None => Ok(line_break),
    }
}

fn document_contains_unavoidable_forced_break(document: &Doc) -> bool {
    match document {
        Doc::HardLine | Doc::HardLineCrlf => true,
        Doc::Verbatim(text) | Doc::VerbatimExact(text) => text.contains(['\n', '\r']),
        Doc::Concat(documents) => documents
            .iter()
            .any(document_contains_unavoidable_forced_break),
        Doc::Indent(child) | Doc::Group(_, child) | Doc::Flat(child) | Doc::LineSuffix(child) => {
            document_contains_unavoidable_forced_break(child)
        }
        Doc::IfBroken(_, broken, flat) => {
            document_contains_unavoidable_forced_break(broken)
                && document_contains_unavoidable_forced_break(flat)
        }
        Doc::Choice(alternatives) => {
            !alternatives.is_empty()
                && alternatives
                    .iter()
                    .all(document_contains_unavoidable_forced_break)
        }
        Doc::Text(_) | Doc::Line | Doc::SoftLine => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ExpressionGroupIds, SourceDocumentError, function_document, normalized_string_value,
        source_document,
    };
    use crate::{parse_source, print};
    use sai_syntax::{
        SyntaxElement, SyntaxKind, SyntaxNode, SyntaxToken,
        ast::{AstNode, Expr, FnDecl, Pattern, RangeExpr, Stmt, Type},
    };

    fn format_shape(source: &[u8]) -> Result<String, SourceDocumentError> {
        let parsed = parse_source(source).expect("source parses");
        let document = source_document(&parsed)?;
        Ok(print(&document).expect("document prints"))
    }

    #[test]
    fn formats_function_modifiers_and_bodyless_signatures() {
        for (input, expected) in [
            (
                b"pub async fn fetch(){}".as_slice(),
                "pub async fn fetch() {}\n",
            ),
            (
                b"export unsafe async fn load()->U32{}",
                "export unsafe async fn load() -> U32 {}\n",
            ),
            (b"fn read() -> I32", "fn read() -> I32\n"),
        ] {
            let output = format_shape(input).expect("function modifiers are supported");
            assert_eq!(output, expected);
            let parsed = parse_source(input).expect("function declaration parses");
            let reparsed = parse_source(output.as_bytes()).expect("formatted function parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let params = (0..10)
            .map(|index| format!("p{index}:I32"))
            .collect::<Vec<_>>()
            .join(",");
        let input = format!("pub unsafe async fn f({params}){{}}\nfn g(){{}}");
        let expected = concat!(
            "pub unsafe async fn f(\n",
            "    p0: I32,\n",
            "    p1: I32,\n",
            "    p2: I32,\n",
            "    p3: I32,\n",
            "    p4: I32,\n",
            "    p5: I32,\n",
            "    p6: I32,\n",
            "    p7: I32,\n",
            "    p8: I32,\n",
            "    p9: I32,\n",
            ") {}\n",
            "\n",
            "fn g() {}\n",
        );
        let output = format_shape(input.as_bytes()).expect("wrapped function head is supported");
        assert_eq!(output, expected);
        let parsed = parse_source(input.as_bytes()).expect("wrapped function heads parse");
        let reparsed = parse_source(output.as_bytes()).expect("wrapped function heads reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let parsed = parse_source(b"trait Reader { fn read() -> I32 }")
            .expect("bodyless function declaration parses");
        let function = parsed
            .root()
            .descendants()
            .filter_map(FnDecl::cast)
            .next()
            .expect("trait function exists");
        let document = function_document(&parsed, &function, &mut ExpressionGroupIds::default())
            .expect("bodyless function declaration formats");
        assert_eq!(
            print(&document.document).expect("bodyless function document prints"),
            "fn read() -> I32\n"
        );

        let parsed = parse_source(b"trait Reader { fn read() -> I32 ensures result >= 0 }")
            .expect("bodyless function with a clause parses");
        let function = parsed
            .root()
            .descendants()
            .filter_map(FnDecl::cast)
            .next()
            .expect("trait function exists");
        let document = function_document(&parsed, &function, &mut ExpressionGroupIds::default())
            .expect("bodyless function clause formats");
        assert_eq!(
            print(&document.document).expect("bodyless function document prints"),
            "fn read() -> I32\n    ensures result >= 0\n"
        );
    }

    #[test]
    fn formats_inherent_trait_and_interface_impls() {
        for (input, expected) in [
            (
                b"impl Buffer{fn capacity(self:borrow Self)->USize{self.len}}".as_slice(),
                concat!(
                    "impl Buffer {\n",
                    "    fn capacity(self: borrow Self) -> USize {\n",
                    "        self.len\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                b"impl Buffer{fn reset(){}\ntype Item=U8}".as_slice(),
                concat!(
                    "impl Buffer {\n",
                    "    fn reset() {}\n",
                    "    type Item = U8\n",
                    "}\n",
                ),
            ),
            (
                concat!(
                    "unsafe impl<T> Convert<T,Output=T> for Box<T> where T:Copy {\n",
                    "type Output=T\n",
                    "fn convert(value:T)->T{value}\n",
                    "}\n",
                )
                .as_bytes(),
                concat!(
                    "unsafe impl<T> Convert<T, Output = T> for Box<T>\n",
                    "    where T: Copy\n",
                    "{\n",
                    "    type Output = T\n",
                    "    fn convert(value: T) -> T {\n",
                    "        value\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                b"unsafe impl wasi.http{fn get()->I32{1}}".as_slice(),
                concat!(
                    "unsafe impl wasi.http {\n",
                    "    fn get() -> I32 {\n",
                    "        1\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (b"impl Empty {}".as_slice(), "impl Empty {}\n"),
        ] {
            let parsed = parse_source(input).expect("impl declaration parses");
            let output = format_shape(input).unwrap_or_else(|error| {
                panic!(
                    "impl declaration should be supported: {error:?}\n{}",
                    String::from_utf8_lossy(input)
                )
            });
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted impl reparses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn preserves_impl_member_blank_lines_and_formats_annotations() {
        let input = concat!(
            "impl Buffer {\n",
            "type Item=I32\n",
            "\n",
            "fn first(){}\n",
            "fn second(){}\n",
            "}\n",
        );
        assert_eq!(
            format_shape(input.as_bytes()),
            Ok(concat!(
                "impl Buffer {\n",
                "    type Item = I32\n",
                "\n",
                "    fn first() {}\n",
                "    fn second() {}\n",
                "}\n",
            )
            .to_owned())
        );
        assert_eq!(
            format_shape(b"/// An implementation.\n@inline\nimpl Buffer {}"),
            Ok("/// An implementation.\n@inline\nimpl Buffer {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"impl Buffer { /// The item.\n@inline type Item = I32 }"),
            Ok(
                "impl Buffer {\n    /// The item.\n    @inline\n    type Item = I32\n}\n"
                    .to_owned()
            )
        );
    }

    #[test]
    fn formats_traits_superbounds_associated_types_and_methods() {
        for (input, expected) in [
            (
                concat!(
                    "pub trait Container<T>:Clone where T:Copy {\n",
                    "type Item:Clone\n",
                    "fn next<U>(self:borrow Self)->Item where U:Clone\n",
                    "fn default_value(value:T)->T{value}\n",
                    "}\n",
                )
                .as_bytes(),
                concat!(
                    "pub trait Container<T>: Clone\n",
                    "    where T: Copy\n",
                    "{\n",
                    "    type Item: Clone\n",
                    "    fn next<U>(self: borrow Self) -> Item\n",
                    "        where U: Clone\n",
                    "    fn default_value(value: T) -> T {\n",
                    "        value\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                b"unsafe trait Marker{}".as_slice(),
                "unsafe trait Marker {}\n",
            ),
            (b"trait Empty {}".as_slice(), "trait Empty {}\n"),
        ] {
            let parsed = parse_source(input).expect("trait declaration parses");
            let output = format_shape(input).unwrap_or_else(|error| {
                panic!(
                    "trait declaration should be supported: {error:?}\n{}",
                    String::from_utf8_lossy(input)
                )
            });
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted trait reparses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn preserves_trait_member_blank_lines_and_formats_annotations() {
        let input = concat!(
            "trait Render {\n",
            "type Output:Clone\n",
            "\n",
            "fn render()\n",
            "fn flush(){}\n",
            "}\n",
        );
        assert_eq!(
            format_shape(input.as_bytes()),
            Ok(concat!(
                "trait Render {\n",
                "    type Output: Clone\n",
                "\n",
                "    fn render()\n",
                "    fn flush() {}\n",
                "}\n",
            )
            .to_owned())
        );
        assert_eq!(
            format_shape(b"/// Rendering contract. \n@inline\ntrait Render {}"),
            Ok("/// Rendering contract.\n@inline\ntrait Render {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"trait Render { /// The rendered output.\n@inline type Output }"),
            Ok(
                "trait Render {\n    /// The rendered output.\n    @inline\n    type Output\n}\n"
                    .to_owned()
            )
        );
    }

    #[test]
    fn separates_following_items_after_a_wrapped_trait_header() {
        let bounds = (0..12)
            .map(|index| format!("BoundName{index}"))
            .collect::<Vec<_>>()
            .join(" + ");
        let input = format!("trait Marker: {bounds} {{}}\nfn following() {{}}");
        let parsed = parse_source(input.as_bytes()).expect("long trait header parses");
        let output = format_shape(input.as_bytes()).expect("long trait header formats");

        assert!(output.contains("\n\nfn following() {}\n"), "{output}");
        assert!(
            output.lines().all(|line| line.chars().count() <= 100),
            "{output}"
        );
        let reparsed = parse_source(output.as_bytes()).expect("formatted trait header reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_interfaces_resources_and_bodyless_functions() {
        let input = concat!(
            "pub interface storage{\n",
            "fn open(path:Str)->Handle\n",
            "resource Handle\n",
            "resource Empty{}\n",
            "resource File{fn close(self:borrow Self)}\n",
            "}\n",
        );
        let expected = concat!(
            "pub interface storage {\n",
            "    fn open(path: Str) -> Handle\n",
            "    resource Handle\n",
            "    resource Empty {}\n",
            "    resource File {\n",
            "        fn close(self: borrow Self)\n",
            "    }\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("interface declarations parse");
        let output = format_shape(input.as_bytes()).expect("interface declarations format");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted interface reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        for (input, expected) in [
            (b"interface empty {}".as_slice(), "interface empty {}\n"),
            (
                b"interface empty { resource Empty }",
                "interface empty {\n    resource Empty\n}\n",
            ),
        ] {
            let output = format_shape(input).expect("empty interface shape is supported");
            assert_eq!(output, expected);
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_top_level_and_interface_type_aliases() {
        let input = concat!(
            "type Scalar=U32\n",
            "pub type Lookup<K>=Map<Str,K>\n",
            "interface storage{type Entry=(U32,Str)\n",
            "resource Handle\n",
            "fn open()->Handle\n",
            "}\n",
        );
        let expected = concat!(
            "type Scalar = U32\n",
            "pub type Lookup<K> = Map<Str, K>\n\n",
            "interface storage {\n",
            "    type Entry = (U32, Str)\n",
            "    resource Handle\n",
            "    fn open() -> Handle\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("type aliases parse");
        let output = format_shape(input.as_bytes()).expect("type aliases format");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted aliases reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_top_level_and_interface_record_types() {
        let input = concat!(
            "pub type Point{x:F64,\n\npub y:F64,\n}\n",
            "type Pair<T> where T: Clone{left:T\nright:T}\n",
            "type Marker{}\n",
            "interface geometry{type Coordinate{x:F64}}\n",
        );
        let expected = concat!(
            "pub type Point {\n",
            "    x: F64\n",
            "\n",
            "    pub y: F64\n",
            "}\n\n",
            "type Pair<T>\n",
            "    where T: Clone\n",
            "{\n",
            "    left: T\n",
            "    right: T\n",
            "}\n\n",
            "type Marker {}\n\n",
            "interface geometry {\n",
            "    type Coordinate {\n",
            "        x: F64\n",
            "    }\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("record declarations parse");
        let output = format_shape(input.as_bytes()).expect("record declarations format");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted records reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_top_level_and_interface_enums() {
        let input = concat!(
            "pub enum Result<T,E>\n",
            "where T: Clone\n",
            "{\n",
            "Ok(T,),\n\n",
            "Err(code:E,),\n",
            "}\n",
            "enum Status{Pending=0,Failed=-2,Hex=0xdead}\n",
            "enum Void{}\n",
            "interface image{enum ImageError{TooLarge,Unsupported}}\n",
        );
        let expected = concat!(
            "pub enum Result<T, E>\n",
            "    where T: Clone\n",
            "{\n",
            "    Ok(T)\n",
            "\n",
            "    Err(code: E)\n",
            "}\n\n",
            "enum Status {\n",
            "    Pending = 0\n",
            "    Failed = -2\n",
            "    Hex = 0xDEAD\n",
            "}\n\n",
            "enum Void {}\n\n",
            "interface image {\n",
            "    enum ImageError {\n",
            "        TooLarge\n",
            "        Unsupported\n",
            "    }\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("enum declarations parse");
        let output = format_shape(input.as_bytes()).expect("enum declarations format");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted enums reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn removes_blank_lines_between_wrapped_variant_payload_elements() {
        let first_tuple_type = "A".repeat(50);
        let second_tuple_type = "B".repeat(50);
        let long_record_type = "C".repeat(80);
        let input = format!(
            "enum E {{ Pair({first_tuple_type},\n\n{second_tuple_type}), Record(first:A,\n\nsecond:{long_record_type}) }}"
        );
        let expected = format!(
            "enum E {{\n    Pair(\n        {first_tuple_type},\n        {second_tuple_type},\n    )\n    Record(\n        first: A,\n        second: {long_record_type},\n    )\n}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("enum payloads parse");
        let output = format_shape(input.as_bytes()).expect("enum payloads format");

        assert_eq!(output, expected);
        assert!(
            output.lines().all(|line| line.chars().count() <= 100),
            "{output}"
        );
        let reparsed = parse_source(output.as_bytes()).expect("formatted enum payloads reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_enum_declaration_and_variant_docs_and_attributes() {
        let input = concat!(
            "/// Represents a status.  \n",
            "@repr(U32)\n",
            "enum Status {\n",
            "/// A ready value. \n",
            "@since(1)\n",
            "Ready,\n",
            "\n",
            "@deprecated(\"old\",)\n",
            "Failed\n",
            "}\n",
        );
        let expected = concat!(
            "/// Represents a status.\n",
            "@repr(U32)\n",
            "enum Status {\n",
            "    /// A ready value.\n",
            "    @since(1)\n",
            "    Ready\n",
            "\n",
            "    @deprecated(\"old\")\n",
            "    Failed\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("annotated enum parses");
        let output = format_shape(input.as_bytes()).expect("annotated enum formats");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted enum reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn moves_long_alias_right_hand_sides_and_separates_following_items() {
        let right_hand_side = "A".repeat(96);
        let input = format!("type T = {right_hand_side}\ntype Next=U32\n");
        let expected = format!("type T =\n    {right_hand_side}\n\ntype Next = U32\n");
        let parsed = parse_source(input.as_bytes()).expect("long alias parses");
        let output = format_shape(input.as_bytes()).expect("long alias formats");

        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted alias reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_top_level_distinct_types_and_inner_visibility() {
        let input = concat!(
            "distinct UserId=U64\n",
            "pub distinct Money<T>=pub I64\n",
            "distinct Meters<T>=T\n",
        );
        let expected = concat!(
            "distinct UserId = U64\n",
            "pub distinct Money<T> = pub I64\n",
            "distinct Meters<T> = T\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("distinct declarations parse");
        let output = format_shape(input.as_bytes()).expect("distinct declarations format");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted distincts reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_refined_aliases_and_distinct_types() {
        let input = concat!(
            "type Percentage=I32\n",
            "where self>=0 and self<=100\n",
            "type TrimmedLabel=Str where self.len()>0 and not self.starts_with(\" \") and not self.ends_with(\" \")\n",
            "type RefinedAlias<T>=T where self==self\n",
            "distinct Age=U8 where self<=150\n",
            "pub distinct Money=pub I64 where self>=0\n",
            "distinct Refined<T>=T where self==self\n",
        );
        let expected = concat!(
            "type Percentage = I32 where self >= 0 and self <= 100\n",
            "\n",
            "type TrimmedLabel = Str\n",
            "    where self.len() > 0 and not self.starts_with(\" \") and not self.ends_with(\" \")\n",
            "\n",
            "type RefinedAlias<T> = T where self == self\n",
            "distinct Age = U8 where self <= 150\n",
            "pub distinct Money = pub I64 where self >= 0\n",
            "distinct Refined<T> = T where self == self\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("refined declarations parse");
        let output = format_shape(input.as_bytes()).expect("refined declarations format");

        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted refinements reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_docs_and_attributes_on_refined_and_distinct_types() {
        let input = concat!(
            "/// Percentage value.  \n",
            "@range(min = 0, max = 100)\n",
            "type Percentage = I32 where self >= 0\n",
            "@opaque\n",
            "distinct UserId = U64\n",
        );
        let expected = concat!(
            "/// Percentage value.\n",
            "@range(min = 0, max = 100)\n",
            "type Percentage = I32 where self >= 0\n",
            "\n",
            "@opaque\n",
            "distinct UserId = U64\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("annotated refinements parse");
        let output = format_shape(input.as_bytes()).expect("annotated refinements format");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted refinements reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn moves_long_refined_bases_and_separates_following_items() {
        let base_type = "BaseType".repeat(11);
        let input =
            format!("distinct Measurement = {base_type} where self > 0\nfn following() {{}}\n");
        let expected = format!(
            "distinct Measurement =\n    {base_type}\n    where self > 0\n\nfn following() {{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("long refined base parses");
        let output = format_shape(input.as_bytes()).expect("long refined base formats");

        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted refined base reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn keeps_refinement_on_the_line_at_the_width_limit() {
        let exact_name = "A".repeat(70);
        let exact_input = format!("type {exact_name} = I32 where self == self\n");
        let exact_output = format_shape(exact_input.as_bytes()).expect("exact-width alias formats");
        let exact_line = exact_output.lines().next().expect("formatted alias line");

        assert_eq!(exact_line.chars().count(), 100);
        assert_eq!(exact_output, exact_input);
        assert_eq!(
            format_shape(exact_output.as_bytes()),
            Ok(exact_output.clone())
        );

        let over_name = "A".repeat(71);
        let over_input = format!("type {over_name} = I32 where self == self\n");
        let over_expected = format!("type {over_name} = I32\n    where self == self\n");
        let over_output = format_shape(over_input.as_bytes()).expect("over-width alias formats");

        assert_eq!(over_output, over_expected);
        assert!(over_output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(over_output.as_bytes()).expect("formatted alias reparses");
        assert_eq!(
            syntax_shape(
                parse_source(over_input.as_bytes())
                    .expect("over-width alias parses")
                    .root()
            ),
            syntax_shape(reparsed.root())
        );
        assert_eq!(format_shape(over_output.as_bytes()), Ok(over_output));
    }

    #[test]
    fn formats_refined_alias_syntax_inside_interfaces() {
        let input = "interface storage { type Entry = Str where self != \"\" }\n";
        let expected = concat!(
            "interface storage {\n",
            "    type Entry = Str where self != \"\"\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("interface alias parses");
        let output = format_shape(input.as_bytes()).expect("interface alias formats");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted interface reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_refined_rhs_before_its_continuation_clause() {
        let first = "AlphaAlphaAlphaAlphaAlphaAlphaAlphaAlpha";
        let second = "BravoBravoBravoBravoBravoBravoBravoBravo";
        let third = "CharlieCharlieCharlieCharlieCharlieCharlie";
        let tuple_input = format!("type T = ({first}, {second}, {third}) where true\n");
        let tuple_expected =
            format!("type T = (\n    {first},\n    {second},\n    {third},\n)\n    where true\n");
        let tuple_output =
            format_shape(tuple_input.as_bytes()).expect("broken tuple refinement formats");

        assert_eq!(tuple_output, tuple_expected);
        let reparsed =
            parse_source(tuple_output.as_bytes()).expect("formatted tuple refinement reparses");
        assert_eq!(
            syntax_shape(
                parse_source(tuple_input.as_bytes())
                    .expect("tuple refinement parses")
                    .root()
            ),
            syntax_shape(reparsed.root())
        );
        assert_eq!(format_shape(tuple_output.as_bytes()), Ok(tuple_output));
    }

    #[test]
    fn moves_broken_refined_rhs_and_allows_unavoidable_overflow() {
        let long_path = "A".repeat(93);
        let moved_input = format!("type T = {long_path}<Alpha, Beta> where true\n");
        let moved_expected = format!(
            "type T =\n    {long_path}<\n        Alpha,\n        Beta,\n    >\n    where true\n"
        );
        let moved_output =
            format_shape(moved_input.as_bytes()).expect("moved broken refinement formats");

        assert_eq!(moved_output, moved_expected);
        assert!(moved_output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(moved_output.as_bytes()).expect("moved refinement reparses");
        assert_eq!(
            syntax_shape(
                parse_source(moved_input.as_bytes())
                    .expect("moved refinement input parses")
                    .root()
            ),
            syntax_shape(reparsed.root())
        );
        assert_eq!(format_shape(moved_output.as_bytes()), Ok(moved_output));

        let unbreakable_path = "B".repeat(98);
        let overflow_input = format!("type T = {unbreakable_path} where true\n");
        let overflow_expected = format!("type T = {unbreakable_path}\n    where true\n");
        let overflow_output =
            format_shape(overflow_input.as_bytes()).expect("unbreakable refinement formats");

        assert_eq!(overflow_output, overflow_expected);
        assert_eq!(
            overflow_output
                .lines()
                .next()
                .expect("formatted declaration line")
                .chars()
                .count(),
            107
        );
        let reparsed =
            parse_source(overflow_output.as_bytes()).expect("overflow refinement reparses");
        assert_eq!(
            syntax_shape(
                parse_source(overflow_input.as_bytes())
                    .expect("overflow refinement input parses")
                    .root()
            ),
            syntax_shape(reparsed.root())
        );
        assert_eq!(
            format_shape(overflow_output.as_bytes()),
            Ok(overflow_output)
        );
    }

    #[test]
    fn moves_long_distinct_bases_and_separates_following_items() {
        let base_type = "BaseType".repeat(11);
        let input = format!("distinct T = {base_type}\nfn following() {{}}\n");
        let expected = format!("distinct T =\n    {base_type}\n\nfn following() {{}}\n");
        let parsed = parse_source(input.as_bytes()).expect("long distinct base parses");
        let output = format_shape(input.as_bytes()).expect("long distinct base formats");

        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted distinct reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_top_level_resources_with_optional_field_lists() {
        let input = concat!(
            "pub resource File{pub name:Str\n",
            "\n",
            "owner:U32}\n",
            "resource Empty{}\n",
            "resource Opaque\n",
        );
        let expected = concat!(
            "pub resource File {\n",
            "    pub name: Str\n",
            "\n",
            "    owner: U32\n",
            "}\n\n",
            "resource Empty {}\n",
            "resource Opaque\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("resource declarations parse");
        let output = format_shape(input.as_bytes()).expect("resource declarations format");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted resources reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_docs_and_attributes_on_supported_declarations_and_fields() {
        let input = concat!(
            "@meta(enabled, feature(\"fast\"), mode = \"strict\", version = 0xdead)\n",
            "@empty()\n",
            "@allow(UNUSED_IMPORTS)\n",
            "type Scalar = U32\n",
            "\n",
            "interface storage { @allow(UNUSED_IMPORTS) type Entry = Str }\n",
            "\n",
            "type Point {\n",
            "/// X coordinate.  \n",
            "@range(min = 0x1, max = 100)\n",
            "x: I32\n",
            "\n",
            "@deprecated(\"old\")\n",
            "y: I32\n",
            "}\n",
            "\n",
            "@allow(UNUSED_IMPORTS)\n",
            "resource Handle\n",
            "\n",
            "/// A resource.  \n",
            "resource File {}\n",
            "\n",
            "/// Runs the operation.  \n",
            "/// The second line stays separate.\n",
            "@inline\n",
            "fn run() {}\n",
        );
        let expected = concat!(
            "@meta(enabled, feature(\"fast\"), mode = \"strict\", version = 0xDEAD)\n",
            "@empty()\n",
            "@allow(UNUSED_IMPORTS)\n",
            "type Scalar = U32\n",
            "\n",
            "interface storage {\n",
            "    @allow(UNUSED_IMPORTS)\n",
            "    type Entry = Str\n",
            "}\n",
            "\n",
            "type Point {\n",
            "    /// X coordinate.\n",
            "    @range(min = 0x1, max = 100)\n",
            "    x: I32\n",
            "\n",
            "    @deprecated(\"old\")\n",
            "    y: I32\n",
            "}\n",
            "\n",
            "@allow(UNUSED_IMPORTS)\n",
            "resource Handle\n",
            "\n",
            "/// A resource.\n",
            "resource File {}\n",
            "\n",
            "/// Runs the operation.\n",
            "/// The second line stays separate.\n",
            "@inline\n",
            "fn run() {}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("annotated declarations parse");
        let output = format_shape(input.as_bytes()).expect("annotated declarations format");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted declarations reparse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
        assert_eq!(
            format_shape(b"/// Kept documentation.\n// ordinary comment\nfn run() {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        let non_ascii_space_doc = "/// Non-breaking space:\u{00a0}\nfn run() {}\n";
        assert_eq!(
            format_shape(non_ascii_space_doc.as_bytes()),
            Ok(non_ascii_space_doc.to_owned())
        );

        let long_attribute = concat!(
            "@configuration(first_feature_option, second_feature_option, ",
            "third_feature_option, fourth_feature_option)\n",
            "type T = U8\n",
        );
        let output =
            format_shape(long_attribute.as_bytes()).expect("long attribute argument list formats");
        assert!(output.contains("\n    second_feature_option,"), "{output}");
        assert!(
            output.lines().all(|line| line.chars().count() <= 100),
            "{output}"
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_docs_and_attributes_on_interface_members() {
        let input = concat!(
            "/// Storage contract.  \n",
            "@allow(UNUSED_IMPORTS)\n",
            "interface storage {\n",
            "/// An entry type. \n",
            "@repr(U32)\n",
            "enum Entry {}\n",
            "@allow(UNUSED_IMPORTS)\n",
            "resource Handle\n",
            "}\n",
        );
        let expected = concat!(
            "/// Storage contract.\n",
            "@allow(UNUSED_IMPORTS)\n",
            "interface storage {\n",
            "    /// An entry type.\n",
            "    @repr(U32)\n",
            "    enum Entry {}\n",
            "    @allow(UNUSED_IMPORTS)\n",
            "    resource Handle\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("annotated interface parses");
        let output = format_shape(input.as_bytes()).expect("annotated interface formats");

        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted interface reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_supported_function_shape_independently_of_input_spacing() {
        let expected = "fn f() {\n    let x = 1\n    x\n}\n";

        assert_eq!(format_shape(b"fn f(){let x=1\nx}"), Ok(expected.to_owned()));
        assert_eq!(
            format_shape(b"fn  f ( ) {\n    let   x = 1\n\n    x\n}\n"),
            Ok(expected.replace("1\n    x", "1\n\n    x"))
        );
    }

    #[test]
    fn formats_simple_parameter_lists_and_breaks_them_at_the_width_limit() {
        assert_eq!(
            format_shape(b"fn f(x:I32){}"),
            Ok("fn f(x: I32) {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f( x : I32 , y:U32 ) {}"),
            Ok("fn f(x: I32, y: U32) {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(_:I32){}"),
            Ok("fn f(_: I32) {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(x:I32,){}"),
            Ok("fn f(x: I32) {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(\n x:I32,\n y:U32\n){}"),
            Ok("fn f(x: I32, y: U32) {}\n".to_owned())
        );

        let first = "a".repeat(46);
        let second = "b".repeat(46);
        let input = format!("fn f({first}:I32,{second}:U32){{}}");
        let parsed_input = parse_source(input.as_bytes()).expect("long input parses");
        let expected = format!("fn f(\n    {first}: I32,\n    {second}: U32,\n) {{}}\n");
        let output = format_shape(input.as_bytes()).expect("simple parameters are supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped parameter list reparses");
        assert_eq!(
            syntax_shape(parsed_input.root()),
            syntax_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_var_parameters_and_simple_return_types() {
        let input = b"fn f(var value:I32, _:U32)\n->Bool {}";
        let expected = "fn f(var value: I32, _: U32) -> Bool {}\n";
        let parsed_input = parse_source(input).expect("input parses");
        let output = format_shape(input).expect("simple signature is supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted signature parses");
        assert_eq!(
            syntax_shape(parsed_input.root()),
            syntax_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
        assert_eq!(
            format_shape(b"fn f(var _:I32){}"),
            Ok("fn f(var _: I32) {}\n".to_owned())
        );
    }

    #[test]
    fn formats_function_type_and_const_generic_parameters() {
        for (input, expected) in [
            (
                b"fn identity<T>(value:T)->T{}".as_slice(),
                "fn identity<T>(value: T) -> T {}\n",
            ),
            (
                b"fn make<const N:USize,T>(value:Array<T,N>)->Array<T,N>{}",
                "fn make<const N: USize, T>(value: Array<T, N>) -> Array<T, N> {}\n",
            ),
            (
                b"fn f<T,>(value:T)borrows{value}effects{io}{}",
                "fn f<T>(value: T)\n    borrows { value }\n    effects { io }\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input).expect("function generics are supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_long_function_generic_parameter_lists() {
        let first = "A".repeat(48);
        let second = "B".repeat(48);
        let input = format!("fn f<{first},{second}>(){{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("generic parameters are supported");
        let expected = format!("fn f<\n    {first},\n    {second},\n>() {{}}\n");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped generics parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_function_generic_trait_and_function_bounds() {
        for (input, expected) in [
            (
                b"fn f<T:Copy+Clone>(){}".as_slice(),
                "fn f<T: Copy + Clone>() {}\n",
            ),
            (
                b"fn f<T:Iterator<Item=U32>+Clone>(){}",
                "fn f<T: Iterator<Item = U32> + Clone>() {}\n",
            ),
            (
                b"fn f<T:Fn(I32)->I32 effects{io}>(){}",
                "fn f<T: Fn(I32) -> I32 effects { io }>() {}\n",
            ),
            (b"fn f<T:Fn(I32,)>(){}", "fn f<T: Fn(I32)>() {}\n"),
            (b"fn f<T:Fn(I32,Str,)>(){}", "fn f<T: Fn(I32, Str)>() {}\n"),
            (
                b"fn f<T:Clone effects{io}>(){}",
                "fn f<T: Clone effects { io }>() {}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input)
                .unwrap_or_else(|error| panic!("{}: {error:?}", String::from_utf8_lossy(input)));
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_long_function_generic_bounds_after_plus() {
        let first = format!("Trait{}", "A".repeat(42));
        let second = format!("Clone{}", "B".repeat(42));
        let input = format!("fn f<T:{first}+{second}>(){{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("long generic bounds are supported");
        let expected = format!("fn f<\n    T: {first} +\n        {second},\n>() {{}}\n");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped bounds parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_function_where_clauses_and_breaks_by_predicate() {
        for (input, expected) in [
            (
                b"fn f<T>(x:T)\nwhere T:Clone+Hash\n{}".as_slice(),
                "fn f<T>(x: T)\n    where T: Clone + Hash\n{}\n",
            ),
            (
                b"fn f<T>()\nwhere T:(Fn() effects{io})\n{}".as_slice(),
                "fn f<T>()\n    where T: (Fn() effects { io })\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input)
                .unwrap_or_else(|error| panic!("{}: {error:?}", String::from_utf8_lossy(input)));
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted where clause parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let first = format!("Trait{}", "A".repeat(42));
        let second = format!("Trait{}", "B".repeat(42));
        let input = format!("fn f<T,U>()\nwhere T:{first},U:{second}\n{{}}");
        let parsed = parse_source(input.as_bytes()).expect("long input parses");
        let output = format_shape(input.as_bytes()).expect("where predicates are supported");
        let expected =
            format!("fn f<T, U>()\n    where\n        T: {first},\n        U: {second}\n{{}}\n");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped where clause parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let first_bound = format!("Trait{}", "C".repeat(42));
        let second_bound = format!("Trait{}", "D".repeat(42));
        let input = format!("fn f<T>()\nwhere T:{first_bound}+{second_bound}\n{{}}");
        let parsed = parse_source(input.as_bytes()).expect("long bounds parse");
        let output = format_shape(input.as_bytes()).expect("long where bounds are supported");
        let expected = format!(
            "fn f<T>()\n    where\n        T: {first_bound} +\n            {second_bound}\n{{}}\n"
        );
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped where bounds parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn orders_where_before_borrows_and_effects() {
        let input = b"fn f<T>() effects{io} borrows{self} where T:Clone {}";
        let expected = concat!(
            "fn f<T>()\n",
            "    where T: Clone\n",
            "    borrows { self }\n",
            "    effects { io }\n",
            "{}\n",
        );
        parse_source(input).expect("input parses with a clause-order diagnostic");
        assert_eq!(format_shape(input), Ok(expected.to_owned()));
        parse_source(expected.as_bytes()).expect("canonical output parses");
        assert_eq!(format_shape(expected.as_bytes()), Ok(expected.to_owned()));
    }

    #[test]
    fn formats_requires_and_ensures_clauses_in_canonical_order() {
        let input =
            b"fn f() ensures result>=Money(0) requires amount<=balance requires true ensures false {}";
        let expected = concat!(
            "fn f()\n",
            "    requires amount <= balance\n",
            "    requires true\n",
            "    ensures result >= Money(0)\n",
            "    ensures false\n",
            "{}\n",
        );
        parse_source(input).expect("input parses with a clause-order diagnostic");
        let output = format_shape(input).expect("contract clauses are supported");
        assert_eq!(output, expected);
        parse_source(output.as_bytes()).expect("canonical clauses parse");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_binary_operators_by_precedence_and_source_order() {
        for (input, expected) in [
            (
                b"fn f() requires a+b*c>=limit {}".as_slice(),
                "fn f()\n    requires a + b * c >= limit\n{}\n",
            ),
            (
                b"fn f() ensures a-b+c==d {}",
                "fn f()\n    ensures a - b + c == d\n{}\n",
            ),
            (
                b"fn f() requires a or b and c {}",
                "fn f()\n    requires a or b and c\n{}\n",
            ),
            (
                b"fn f() requires a<<b|c {}",
                "fn f()\n    requires a << b | c\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("binary operator expression parses");
            let output = format_shape(input).expect("binary operator expression is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted expression parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_prefix_operators_with_canonical_spacing() {
        for (expression, expected_expression) in [
            ("-value", "-value"),
            ("~ value", "~value"),
            ("* value", "*value"),
            ("borrow value", "borrow value"),
            ("borrow   mut   value", "borrow mut value"),
            ("not value", "not value"),
            ("not not value", "not not value"),
            ("-~value", "-~value"),
        ] {
            let input = format!("fn f() requires {expression} {{}}");
            let expected = format!("fn f()\n    requires {expected_expression}\n{{}}\n");
            let parsed = parse_source(input.as_bytes()).expect("prefix expression parses");
            let output = format_shape(input.as_bytes()).expect("prefix expression is supported");
            assert_eq!(output, expected);
            let reparsed =
                parse_source(output.as_bytes()).expect("formatted prefix expression parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_long_prefix_chains_iteratively() {
        let prefix_count = 256;
        let expression = format!("{}value", "-".repeat(prefix_count));
        let input = format!("fn f() requires {expression} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("long prefix chain parses");
        let output = format_shape(input.as_bytes()).expect("long prefix chain is supported");
        assert_eq!(output, format!("fn f()\n    requires {expression}\n{{}}\n"));
        let reparsed = parse_source(output.as_bytes()).expect("formatted prefix chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_ranges_without_spaces_and_preserves_endpoint_precedence() {
        for (expression, expected_expression) in [
            ("start..end", "start..end"),
            ("start ..= end", "start..=end"),
            ("..end", "..end"),
            ("check(start..)", "check(start..)"),
            ("(..)", "(..)"),
            ("start+step..end*factor", "start + step..end * factor"),
            ("start..end>=limit", "start..end >= limit"),
        ] {
            let input = format!("fn f() requires {expression} {{}}");
            let expected = format!("fn f()\n    requires {expected_expression}\n{{}}\n");
            let parsed = parse_source(input.as_bytes())
                .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
            let output = format_shape(input.as_bytes())
                .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
            assert_eq!(output, expected);
            let reparsed =
                parse_source(output.as_bytes()).expect("formatted range expression parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn range_endpoints_can_break_without_breaking_the_range_operator() {
        let first = "first".repeat(9);
        let second = "second".repeat(8);
        let third = "third".repeat(6);
        let expression = format!("{first}+{second}+{third}..limit");
        let input = format!("fn f() requires {expression} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("long range endpoint parses");
        let output = format_shape(input.as_bytes()).expect("long range endpoint is supported");
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert!(output.contains(")..limit\n"));
        let reparsed = parse_source(output.as_bytes()).expect("formatted range parses");
        let original_range = parsed
            .root()
            .descendants()
            .find_map(RangeExpr::cast)
            .expect("original range expression exists");
        let formatted_range = reparsed
            .root()
            .descendants()
            .find_map(RangeExpr::cast)
            .expect("formatted range expression exists");
        let original_start = original_range.start().expect("original range start");
        let formatted_start = formatted_range.start().expect("formatted range start");
        let formatted_start = match formatted_start {
            Expr::Paren(paren) => paren
                .expression()
                .expect("formatter-inserted parentheses retain the endpoint"),
            expression => expression,
        };
        assert_eq!(
            syntax_shape(original_start.syntax()),
            syntax_shape(formatted_start.syntax())
        );
        assert_eq!(
            syntax_shape(original_range.end().expect("original range end").syntax()),
            syntax_shape(formatted_range.end().expect("formatted range end").syntax())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn parenthesized_prefix_and_range_operands_use_protected_chain_layout() {
        let first = "first".repeat(8);
        let second = "second".repeat(7);
        let third = "third".repeat(5);

        let input = format!("fn f() requires (not {first}+{second}+{third}) {{}}");
        let expected = format!(
            "fn f()\n    requires (\n        not {first}\n            + {second}\n            + {third}\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("parenthesized prefix parses");
        let output = format_shape(input.as_bytes()).expect("parenthesized prefix is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted prefix parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input = format!("fn f() requires ({first}+{second}+{third}..limit) {{}}");
        let expected = format!(
            "fn f()\n    requires (\n        {first}\n            + {second}\n            + {third}..limit\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("parenthesized range parses");
        let output = format_shape(input.as_bytes()).expect("parenthesized range is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted range parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_unit_tuple_array_and_postfix_expression_forms() {
        for (expression, expected_expression) in [
            ("()", "()"),
            ("(value,)", "(value,)"),
            ("(first,second,)", "(first, second)"),
            ("[]", "[]"),
            ("[first,second,]", "[first, second]"),
            (".Variant", ".Variant"),
            (".Variant(payload)", ".Variant(payload)"),
            ("value.0", "value.0"),
            ("value?", "value?"),
            ("value.await", "value.await"),
            ("items[index+offset]", "items[index + offset]"),
            ("items[index].field().next()", "items[index].field().next()"),
        ] {
            let input = format!("fn f() requires {expression} {{}}");
            let expected = format!("fn f()\n    requires {expected_expression}\n{{}}\n");
            let parsed = parse_source(input.as_bytes())
                .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
            let output = format_shape(input.as_bytes())
                .unwrap_or_else(|error| panic!("{expression}: {error:?}"));
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes())
                .unwrap_or_else(|error| panic!("formatted {expression}: {error:?}"));
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_named_and_record_update_call_arguments_in_source_order() {
        let input = "fn f() requires call(first,named: value,last,..base,) {}\n";
        let expected = "fn f()\n    requires call(first, named: value, last, ..base)\n{}\n";
        let parsed = parse_source(input.as_bytes()).expect("mixed call arguments parse");
        let output = format_shape(input.as_bytes()).expect("mixed call arguments are supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted call arguments parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let first = "first".repeat(8);
        let name = format!("named_{}", "n".repeat(15));
        let value = "value".repeat(7);
        let base = "base".repeat(8);
        let input = format!("fn f() requires call({first},{name}:{value},..{base},) {{}}\n");
        let expected = format!(
            "fn f()\n    requires call(\n        {first},\n        {name}: {value},\n        ..{base}\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("long mixed call arguments parse");
        let output =
            format_shape(input.as_bytes()).expect("long mixed call arguments are supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert!(!output.contains(&format!("..{base},\n")));
        let reparsed = parse_source(output.as_bytes()).expect("broken mixed call arguments parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_tuple_and_array_lists_with_required_trailing_commas() {
        let first = "first".repeat(9);
        let second = "second".repeat(8);
        for (expression, expected_expression) in [
            (
                format!("({first},{second})"),
                format!("(\n        {first},\n        {second},\n    )"),
            ),
            (
                format!("[{first},{second}]"),
                format!("[\n        {first},\n        {second},\n    ]"),
            ),
        ] {
            let input = format!("fn f() requires {expression} {{}}");
            let expected = format!("fn f()\n    requires {expected_expression}\n{{}}\n");
            let parsed = parse_source(input.as_bytes()).expect("long delimited expression parses");
            let output =
                format_shape(input.as_bytes()).expect("long delimited expression is supported");
            assert_eq!(output, expected);
            assert!(output.lines().all(|line| line.chars().count() <= 100));
            let reparsed =
                parse_source(output.as_bytes()).expect("broken delimited expression parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let left = "left".repeat(12);
        let right = "right".repeat(10);
        let input = format!("fn f() requires items[{left}+{right}] {{}}");
        let output = format_shape(input.as_bytes()).expect("long index expression is supported");
        assert!(output.contains(&format!("items[{left}\n        + {right}]")));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken index expression parses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
        assert!(reparsed.diagnostics().is_empty());
    }

    #[test]
    fn member_chains_keep_postfix_operations_inside_the_chain_group() {
        let first = format!("first_call_{}", "a".repeat(20));
        let second = format!("second_call_{}", "b".repeat(20));
        let third = format!("third_call_{}", "c".repeat(20));
        let input = format!(
            "fn f() requires root[index].{first}().{second}()[offset]?.await.0.{third}() {{}}"
        );
        let expected = format!(
            "fn f()\n    requires root[index]\n        .{first}()\n        .{second}()[offset]?\n        .await\n        .0\n        .{third}()\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("mixed postfix chain parses");
        let output = format_shape(input.as_bytes()).expect("mixed postfix chain is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken postfix chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn member_chains_start_segments_at_await_and_tuple_index_before_calls() {
        let first = format!("first_call_{}", "a".repeat(40));
        let second = format!("second_call_{}", "b".repeat(40));

        for (postfix, expected_postfix) in [("await", ".await"), ("0", ".0")] {
            let input = format!("fn f() requires root().{postfix}.{first}().{second}() {{}}");
            let expected = format!(
                "fn f()\n    requires root()\n        {expected_postfix}\n        .{first}()\n        .{second}()\n{{}}\n"
            );
            let parsed = parse_source(input.as_bytes())
                .unwrap_or_else(|error| panic!("{postfix} chain parses: {error:?}"));
            let output = format_shape(input.as_bytes())
                .unwrap_or_else(|error| panic!("{postfix} chain is supported: {error:?}"));
            assert_eq!(output, expected);
            assert!(output.lines().all(|line| line.chars().count() <= 100));
            let reparsed = parse_source(output.as_bytes())
                .unwrap_or_else(|error| panic!("formatted {postfix} chain parses: {error:?}"));
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));

            let input = format!("fn f() requires root.{postfix}.{first}().{second}() {{}}");
            let expected = format!(
                "fn f()\n    requires root{expected_postfix}\n        .{first}()\n        .{second}()\n{{}}\n"
            );
            let parsed = parse_source(input.as_bytes())
                .unwrap_or_else(|error| panic!("{postfix} head parses: {error:?}"));
            let output = format_shape(input.as_bytes())
                .unwrap_or_else(|error| panic!("{postfix} head is supported: {error:?}"));
            assert_eq!(output, expected);
            assert!(output.lines().all(|line| line.chars().count() <= 100));
            let reparsed = parse_source(output.as_bytes())
                .unwrap_or_else(|error| panic!("formatted {postfix} head parses: {error:?}"));
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_operator_chains_with_context_appropriate_parentheses() {
        let first = "first".repeat(8);
        let second = "second".repeat(7);
        let third = "third".repeat(5);

        let input = format!("fn f() requires {first}+{second}+{third} {{}}");
        let expected = format!(
            "fn f()\n    requires (\n        {first}\n        + {second}\n        + {third}\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("top-level chain parses");
        let output = format_shape(input.as_bytes()).expect("top-level chain is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped chain parses");
        assert_eq!(
            contract_expression_shape(parsed.root()),
            contract_expression_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input = format!("fn f() requires check({first}+{second}+{third}) {{}}");
        let expected = format!(
            "fn f()\n    requires check(\n        {first}\n            + {second}\n            + {third},\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("protected chain parses");
        let output = format_shape(input.as_bytes()).expect("protected chain is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("protected chain parses again");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input = format!("fn f() requires ({first}+{second}+{third}) {{}}");
        let expected = format!(
            "fn f()\n    requires (\n        {first}\n        + {second}\n        + {third}\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("explicit chain parses");
        let output = format_shape(input.as_bytes()).expect("explicit chain is supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("explicit chain reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_long_operator_chains_without_recursive_flattening() {
        let term_count = 4096;
        let terms = (0..term_count)
            .map(|index| format!("value{index}"))
            .collect::<Vec<_>>()
            .join("+");
        let input = format!("fn f() requires {terms} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("long operator chain parses");
        let output = format_shape(input.as_bytes()).expect("long operator chain is supported");
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert_eq!(
            output
                .lines()
                .filter(|line| line.trim_start().starts_with("+ "))
                .count(),
            term_count - 1
        );
        let reparsed = parse_source(output.as_bytes()).expect("formatted long chain parses");
        assert_eq!(
            contract_expression_shape(parsed.root()),
            contract_expression_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_long_contract_comparisons_with_safe_parentheses() {
        let lhs = "left".repeat(12);
        let rhs = "right".repeat(10);
        let input = format!("fn f() requires {lhs}=={rhs} {{}}");
        let expected =
            format!("fn f()\n    requires (\n        {lhs}\n        == {rhs}\n    )\n{{}}\n");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("comparison contract is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped contract parses");
        assert!(reparsed.diagnostics().is_empty());
        assert_eq!(
            contract_expression_shape(parsed.root()),
            contract_expression_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn preserves_protected_and_explicitly_parenthesized_comparisons() {
        let lhs = "left".repeat(12);
        let rhs = "right".repeat(10);

        let input = format!("fn f() requires check({lhs}=={rhs}) {{}}");
        let parsed = parse_source(input.as_bytes()).expect("protected comparison parses");
        let output = format_shape(input.as_bytes()).expect("protected comparison is supported");
        let reparsed = parse_source(output.as_bytes()).expect("formatted call parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let chain_rhs = "right".repeat(18);
        let input = format!("fn f() requires root.first().second()=={chain_rhs} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("comparison with a chain parses");
        let output = format_shape(input.as_bytes()).expect("comparison with a chain is supported");
        assert_eq!(
            output,
            format!(
                "fn f()\n    requires (\n        root.first().second()\n        == {chain_rhs}\n    )\n{{}}\n"
            )
        );
        let reparsed = parse_source(output.as_bytes()).expect("wrapped chain comparison parses");
        assert_eq!(
            contract_expression_shape(parsed.root()),
            contract_expression_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input = format!("fn f() requires ({lhs}=={rhs}) {{}}");
        let parsed = parse_source(input.as_bytes()).expect("parenthesized comparison parses");
        let output = format_shape(input.as_bytes()).expect("parenthesized comparison is supported");
        let reparsed = parse_source(output.as_bytes()).expect("formatted parentheses parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_field_and_call_contract_expressions() {
        for (input, expected) in [
            (
                b"fn f() requires user.active {}".as_slice(),
                "fn f()\n    requires user.active\n{}\n",
            ),
            (
                b"fn f() requires user.is_active() {}".as_slice(),
                "fn f()\n    requires user.is_active()\n{}\n",
            ),
            (
                b"fn f() ensures check(user.value,amount,) {}".as_slice(),
                "fn f()\n    ensures check(user.value, amount)\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("contract input parses");
            let output = format_shape(input).expect("field and call expressions are supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted contract parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_generic_method_calls_and_format_string_contracts() {
        for (input, expected) in [
            (
                b"fn f() requires user.check<I32>(value) {}".as_slice(),
                "fn f()\n    requires user.check<I32>(value)\n{}\n",
            ),
            (
                b"fn f() requires f\"hello {name}\" {}",
                "fn f()\n    requires f\"hello {name}\"\n{}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("contract input parses");
            let output = format_shape(input).expect("supported contract expression is formatted");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted contract parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn keeps_empty_call_arguments_inline_after_a_long_callee() {
        let callee = "a".repeat(100);
        let input = format!("fn f() requires {callee}() {{}}");
        let output = format_shape(input.as_bytes()).expect("empty call is supported");
        assert!(output.contains(&format!("{callee}()")));
        assert!(!output.contains("(\n\n)"));
        let reparsed = parse_source(output.as_bytes()).expect("formatted empty call parses");
        assert!(reparsed.diagnostics().is_empty());
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_long_method_chains_before_every_call_segment() {
        let first = format!("first{}", "a".repeat(45));
        let second = format!("second{}", "b".repeat(45));
        let input = format!("fn f() requires root.{first}().{second}() {{}}");
        let expected =
            format!("fn f()\n    requires root\n        .{first}()\n        .{second}()\n{{}}\n");
        let parsed = parse_source(input.as_bytes()).expect("method chain parses");
        let output = format_shape(input.as_bytes()).expect("method chain is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken method chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn keeps_intermediate_fields_in_broken_member_chains() {
        let middle = "middle".repeat(12);
        let input = format!("fn f() requires root.first().{middle}.second() {{}}");
        let expected = format!(
            "fn f()\n    requires root\n        .first()\n        .{middle}\n        .second()\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("member chain parses");
        let output = format_shape(input.as_bytes()).expect("member chain is supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("broken member chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_trailing_fields_and_joins_uppercase_members_to_the_prior_segment() {
        let trailing = "trailing".repeat(10);
        let input = format!("fn f() requires root.first().second().{trailing} {{}}");
        let expected = format!(
            "fn f()\n    requires root\n        .first()\n        .second()\n        .{trailing}\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("trailing field chain parses");
        let output = format_shape(input.as_bytes()).expect("trailing field chain is supported");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("broken trailing field chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let first = "first".repeat(5);
        let second = "second".repeat(5);
        let last = "last".repeat(5);
        let input = format!("fn f() requires root.{first}().Variant.{second}().{last}() {{}}");
        let expected = format!(
            "fn f()\n    requires root\n        .{first}().Variant\n        .{second}()\n        .{last}()\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("uppercase field chain parses");
        let output = format_shape(input.as_bytes()).expect("uppercase field joins prior segment");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted uppercase chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let input =
            format!("fn f() requires root.{first}().Variant(payload).{second}().{last}() {{}}");
        let expected = format!(
            "fn f()\n    requires root\n        .{first}().Variant(payload)\n        .{second}()\n        .{last}()\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("uppercase call chain parses");
        let output = format_shape(input.as_bytes()).expect("uppercase call joins prior segment");
        assert_eq!(output, expected);
        let reparsed =
            parse_source(output.as_bytes()).expect("formatted uppercase call chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let first = "first".repeat(18);
        let input = format!("fn f() requires root.{first}().Variant(payload) {{}}");
        let expected = format!(
            "fn f()\n    requires root.{first}().Variant(\n        payload,\n    )\n{{}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("single call segment parses");
        let output = format_shape(input.as_bytes()).expect("one segment stays attached");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("single call segment reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn keeps_short_generics_flat_when_function_parameters_wrap() {
        let first = "first".repeat(9);
        let second = "second".repeat(8);
        let input = format!("fn f<T>({first}:I32,{second}:U32){{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("generic signature is supported");
        let expected = format!("fn f<T>(\n    {first}: I32,\n    {second}: U32,\n) {{}}\n");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("wrapped parameters parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn keeps_short_generics_flat_when_return_type_overflows() {
        let return_type = "R".repeat(92);
        let input = format!("fn f<T>()->{return_type}{{}}");
        let expected = format!("fn f<T>() -> {return_type} {{}}\n");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("generic signature is supported");
        assert_eq!(output, expected);
        assert!(output.lines().next().unwrap().chars().count() > 100);
        let reparsed = parse_source(output.as_bytes()).expect("output parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_compound_parameter_and_return_types() {
        for (input, expected) in [
            (
                b"fn f(value:http.client.Response<Vec<I32>>) -> Map<Str, User> {}".as_slice(),
                "fn f(value: http.client.Response<Vec<I32>>) -> Map<Str, User> {}\n",
            ),
            (
                b"fn f(value:borrow mut Map<Str,(I32,)>) -> (I32, Str) {}",
                "fn f(value: borrow mut Map<Str, (I32,)>) -> (I32, Str) {}\n",
            ),
            (
                b"fn f(value:owned Self.Item) -> owned Vec<User> {}",
                "fn f(value: owned Self.Item) -> owned Vec<User> {}\n",
            ),
            (
                b"fn f(value: (Vec<I32>)) -> () {}",
                "fn f(value: (Vec<I32>)) -> () {}\n",
            ),
            (b"fn f(value: _) {}", "fn f(value: _) {}\n"),
            (b"fn f(value: Vec<I32,>) {}", "fn f(value: Vec<I32>) {}\n"),
            (
                b"fn f(value:Array<I32,0xff>, other:Pair<' ',true,-0xff>) {}",
                "fn f(value: Array<I32, 0xFF>, other: Pair<' ', true, -0xFF>) {}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input).expect("compound types are supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_long_generic_type_arguments_as_a_delimited_list() {
        let first = "FirstType".repeat(6);
        let second = "SecondType".repeat(6);
        let input = format!("fn f(value:Container<{first},{second}>){{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let expected = format!(
            "fn f(\n    value: Container<\n        {first},\n        {second},\n    >,\n) {{}}\n"
        );
        let output = format_shape(input.as_bytes()).expect("long type is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("output parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let return_input = format!("fn f(value:I32)->Container<{first},{second}>{{}}");
        let return_output =
            format_shape(return_input.as_bytes()).expect("compound return type is supported");
        assert!(return_output.contains(") -> Container<\n"));
        assert!(return_output.contains(&format!("    {first},\n")));
        assert!(
            return_output
                .lines()
                .all(|line| line.chars().count() <= 100)
        );
        let reparsed = parse_source(return_output.as_bytes()).expect("wrapped return type parses");
        let parsed_return = parse_source(return_input.as_bytes()).expect("input parses");
        assert_eq!(
            syntax_shape(parsed_return.root()),
            syntax_shape(reparsed.root())
        );
        assert_eq!(format_shape(return_output.as_bytes()), Ok(return_output));
    }

    #[test]
    fn breaks_long_borrow_owned_and_grouped_types_safely() {
        let long_type = "A".repeat(86);
        for (input, expected) in [
            (
                format!("fn f()->borrow {long_type}{{}}"),
                format!("fn f() -> borrow\n    {long_type} {{}}\n"),
            ),
            (
                format!("fn f()->borrow mut {long_type}{{}}"),
                format!("fn f() -> borrow mut\n    {long_type} {{}}\n"),
            ),
            (
                format!("fn f()->owned {long_type}{{}}"),
                format!("fn f() -> owned\n    {long_type} {{}}\n"),
            ),
            (
                format!("fn f()->({long_type}){{}}"),
                format!("fn f() -> (\n    {long_type}\n) {{}}\n"),
            ),
        ] {
            let parsed = parse_source(input.as_bytes()).expect("input parses");
            let output = format_shape(input.as_bytes()).expect("wrapped type is supported");
            assert_eq!(output, expected);
            assert!(output.lines().all(|line| line.chars().count() <= 100));
            let reparsed = parse_source(output.as_bytes()).expect("wrapped output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_function_pointer_types_and_effect_sets() {
        for (input, expected) in [
            (
                b"fn f(cb:fn(I32,borrow Vec<User>)->Bool effects {alloc,net.http}){}".as_slice(),
                "fn f(cb: fn(I32, borrow Vec<User>) -> Bool effects { alloc, net.http }) {}\n",
            ),
            (
                b"fn f(cb:unsafe fn(I32)->Str effects {net.http,alloc}){}",
                "fn f(cb: unsafe fn(I32) -> Str effects { net.http, alloc }) {}\n",
            ),
            (
                b"fn f(cb:fn()effects{}){}",
                "fn f(cb: fn() effects {}) {}\n",
            ),
            (
                b"fn f(cb:fn(I32)\n->Bool effects {alloc}){}",
                "fn f(cb: fn(I32) -> Bool effects { alloc }) {}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input)
                .unwrap_or_else(|error| panic!("{}: {error:?}", String::from_utf8_lossy(input)));
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_long_function_pointer_parameters_and_effect_sets() {
        let first_type = format!("T{}", "A".repeat(60));
        let second_type = format!("U{}", "B".repeat(60));
        let first_effect = format!("network.{}", "a".repeat(40));
        let second_effect = format!("memory.{}", "b".repeat(40));
        let input = format!(
            "fn f(callback:fn({first_type},{second_type}) effects {{{first_effect},{second_effect}}}){{}}"
        );
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("long function pointer is supported");
        assert!(output.contains("callback: fn(\n"));
        assert!(output.contains(&format!("        {first_type},\n")));
        assert!(output.contains(") effects {\n"));
        assert!(output.contains(&format!("        {first_effect}\n")));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped output parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_borrows_and_function_effect_clauses() {
        for (input, expected) in [
            (
                b"fn f()effects{io,net.http}{}".as_slice(),
                "fn f()\n    effects { io, net.http }\n{}\n",
            ),
            (
                b"fn f()borrows{self,user.name}effects{alloc}{}",
                "fn f()\n    borrows { self, user.name }\n    effects { alloc }\n{}\n",
            ),
            (b"fn f()borrows{}{}", "fn f()\n    borrows {}\n{}\n"),
            (
                b"fn f()effects{io}{let x=1\nx}",
                "fn f()\n    effects { io }\n{\n    let x = 1\n    x\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input)
                .unwrap_or_else(|error| panic!("{}: {error:?}", String::from_utf8_lossy(input)));
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn reorders_function_borrows_and_effect_clauses() {
        let input = b"fn f() effects { io } borrows { self } {}";
        let expected = "fn f()\n    borrows { self }\n    effects { io }\n{}\n";
        parse_source(input).expect("input parses with a clause-order diagnostic");
        assert_eq!(format_shape(input), Ok(expected.to_owned()));
        parse_source(expected.as_bytes()).expect("canonical output parses");
        assert_eq!(format_shape(expected.as_bytes()), Ok(expected.to_owned()));
    }

    #[test]
    fn breaks_long_borrows_provenances_without_commas() {
        let first = format!("resource.{}", "a".repeat(45));
        let second = format!("storage.{}", "b".repeat(45));
        let input = format!("fn f() borrows {{{first},{second}}} {{}}");
        let parsed = parse_source(input.as_bytes()).expect("input parses");
        let output = format_shape(input.as_bytes()).expect("long borrows clause is supported");
        assert!(output.contains("    borrows {\n"));
        assert!(output.contains(&format!("        {first}\n")));
        assert!(output.contains(&format!("        {second}\n")));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("wrapped output parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_supported_self_parameter_types() {
        for (input, expected) in [
            (b"fn f(self:Self){}".as_slice(), "fn f(self: Self) {}\n"),
            (b"fn f(var self:Self){}", "fn f(var self: Self) {}\n"),
            (
                b"fn f(self:borrow mut Self){}",
                "fn f(self: borrow mut Self) {}\n",
            ),
            (
                b"fn f(var self:borrow Self){}",
                "fn f(var self: borrow Self) {}\n",
            ),
            (
                b"fn f(var self:borrow mut Self,x:I32){}",
                "fn f(var self: borrow mut Self, x: I32) {}\n",
            ),
            (
                b"fn f(self:borrow   Self){}",
                "fn f(self: borrow Self) {}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("input parses");
            let output = format_shape(input).expect("receiver form is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("output parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let first = "a".repeat(39);
        let second = "b".repeat(39);
        let without_receiver = format!("fn f({first}:I32,{second}:U32){{}}");
        let flat = format_shape(without_receiver.as_bytes()).expect("signature is supported");
        assert_eq!(flat, format!("fn f({first}: I32, {second}: U32) {{}}\n"));
        assert!(flat.lines().all(|line| line.chars().count() <= 100));

        let input = format!("fn f(self:Self,{first}:I32,{second}:U32){{}}");
        let expected =
            format!("fn f(\n    self: Self,\n    {first}: I32,\n    {second}: U32,\n) {{}}\n");
        assert_eq!(format_shape(input.as_bytes()), Ok(expected));
    }

    #[test]
    fn return_type_width_can_break_a_parameter_list() {
        let first = "a".repeat(39);
        let second = "b".repeat(39);
        let no_return_type = format!("fn f({first}:I32,{second}:U32){{}}");
        let flat = format_shape(no_return_type.as_bytes()).expect("flat signature is supported");
        assert_eq!(flat, format!("fn f({first}: I32, {second}: U32) {{}}\n"));
        assert!(flat.lines().all(|line| line.chars().count() <= 100));

        let input = format!("fn f({first}:I32,{second}:U32)->Bool{{}}");
        let expected = format!("fn f(\n    {first}: I32,\n    {second}: U32,\n) -> Bool {{}}\n");
        assert_eq!(format_shape(input.as_bytes()), Ok(expected));
    }

    #[test]
    fn supported_output_is_a_fixed_point() {
        let once = format_shape(b"fn f(){let x=1\nx}").expect("supported shape");
        assert_eq!(format_shape(once.as_bytes()), Ok(once));
    }

    #[test]
    fn formats_empty_functions_and_preserves_top_level_blank_lines() {
        assert_eq!(format_shape(b""), Ok(String::new()));
        assert_eq!(format_shape(b"fn f(){}"), Ok("fn f() {}\n".to_owned()));
        assert_eq!(
            format_shape(b"fn f(){}\nfn g(){}"),
            Ok("fn f() {}\nfn g() {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(){}\n\nfn g(){}"),
            Ok("fn f() {}\n\nfn g() {}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(){let x=1\nx}\nfn g(){}"),
            Ok("fn f() {\n    let x = 1\n    x\n}\n\nfn g() {}\n".to_owned())
        );

        let input = parse_source(b"fn f(){let x=1\nx}\nfn g(){}").expect("input parses");
        let output = format_shape(b"fn f(){let x=1\nx}\nfn g(){}").expect("supported file");
        let reparsed = parse_source(output.as_bytes()).expect("formatted file parses");
        assert_eq!(syntax_shape(input.root()), syntax_shape(reparsed.root()));
    }

    #[test]
    fn formats_all_basic_statement_forms_and_preserves_one_blank_line() {
        let input = concat!(
            "fn f() {\n",
            "let value:I32=\n1\n",
            "var current:I32=2\n\n",
            "current+=\nvalue\n",
            "assert current>value,\"message\"\n",
            "consume(current)\n",
            "current\n",
            "}\n",
        );
        let expected = concat!(
            "fn f() {\n",
            "    let value: I32 = 1\n",
            "    var current: I32 = 2\n",
            "\n",
            "    current += value\n",
            "    assert current > value, \"message\"\n",
            "    consume(current)\n",
            "    current\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("statement forms parse");
        let output = format_shape(input.as_bytes()).expect("statement forms are supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted statements parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn moves_a_short_statement_rhs_to_the_next_line_when_it_fits() {
        let name = format!("binding_{}", "x".repeat(81));
        let input = format!("fn f() {{ let {name}=1\n{name}\n}}\n");
        let expected = format!("fn f() {{\n    let {name} =\n        1\n    {name}\n}}\n");
        let parsed = parse_source(input.as_bytes()).expect("long binding name parses");
        let output = format_shape(input.as_bytes()).expect("long binding name is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("moved rhs parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_return_break_and_continue_expressions() {
        let input = concat!(
            "fn f() {\n",
            "return\n",
            "return result\n",
            "break\n",
            "break 'outer result\n",
            "continue\n",
            "continue 'outer\n",
            "}\n",
        );
        let expected = concat!(
            "fn f() {\n",
            "    return\n",
            "    return result\n",
            "    break\n",
            "    break 'outer result\n",
            "    continue\n",
            "    continue 'outer\n",
            "}\n",
        );
        let parsed = parse_source(input.as_bytes()).expect("control expressions parse");
        let output = format_shape(input.as_bytes()).expect("control expressions are supported");
        assert_eq!(output, expected);
        let reparsed =
            parse_source(output.as_bytes()).expect("formatted control expressions parse");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let value = format!("result_{}", "r".repeat(88));
        let input = format!("fn f() {{ return {value}\n}}\n");
        let expected = format!("fn f() {{\n    return {value}\n}}\n");
        let output = format_shape(input.as_bytes()).expect("long return value is supported");
        assert_eq!(output, expected);
        assert!(output.lines().any(|line| line.chars().count() > 100));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_block_and_conditional_expressions() {
        for (input, expected) in [
            (
                b"fn f(){if ready {work()}}".as_slice(),
                "fn f() {\n    if ready {\n        work()\n    }\n}\n",
            ),
            (
                b"fn f(){if ready {} else {work()}}".as_slice(),
                "fn f() {\n    if ready {} else {\n        work()\n    }\n}\n",
            ),
            (
                b"fn f(){if first {one()} else if second {two()} else {three()}}".as_slice(),
                concat!(
                    "fn f() {\n",
                    "    if first {\n",
                    "        one()\n",
                    "    } else if second {\n",
                    "        two()\n",
                    "    } else {\n",
                    "        three()\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                b"fn f(){\nif ready {\nwork()\n}\nelse {\nwait()\n}\n}".as_slice(),
                concat!(
                    "fn f() {\n",
                    "    if ready {\n",
                    "        work()\n",
                    "    } else {\n",
                    "        wait()\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                b"fn f(){{value}}".as_slice(),
                "fn f() {\n    {\n        value\n    }\n}\n",
            ),
            (
                b"fn f(){let result=if ready {yes()} else {no()}\nresult}".as_slice(),
                concat!(
                    "fn f() {\n",
                    "    let result = if ready {\n",
                    "        yes()\n",
                    "    } else {\n",
                    "        no()\n",
                    "    }\n",
                    "    result\n",
                    "}\n",
                ),
            ),
            (
                b"fn f(){consume(if ready {yes()} else {no()})}".as_slice(),
                concat!(
                    "fn f() {\n",
                    "    consume(\n",
                    "        if ready {\n",
                    "            yes()\n",
                    "        } else {\n",
                    "            no()\n",
                    "        },\n",
                    "    )\n",
                    "}\n",
                ),
            ),
        ] {
            let parsed = parse_source(input).expect("control and block expressions parse");
            let output =
                format_shape(input).expect("supported control and block expressions format");
            assert_eq!(output, expected);
            let reparsed =
                parse_source(output.as_bytes()).expect("formatted expressions parse again");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_match_expressions_and_guarded_arms() {
        for (input, expected) in [
            (
                b"fn f(){match value{.Ready=>1,_=>0}}".as_slice(),
                "fn f() {\n    match value { .Ready => 1, _ => 0 }\n}\n",
            ),
            (
                b"fn f(){match value{.Ready=>1,}}",
                "fn f() {\n    match value { .Ready => 1 }\n}\n",
            ),
            (
                b"fn f(){match value{.Ready if ready=>work(),_=>fallback()}}",
                "fn f() {\n    match value { .Ready if ready => work(), _ => fallback() }\n}\n",
            ),
            (
                b"fn f(){match value{Some((left,right))=>consume(left)}}",
                "fn f() {\n    match value { Some((left, right)) => consume(left) }\n}\n",
            ),
            (
                b"fn f(){match value{|.Ready|.Other=>work()}}",
                "fn f() {\n    match value { .Ready | .Other => work() }\n}\n",
            ),
            (
                b"fn f(){match value{}}",
                "fn f() {\n    match value {}\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("match expression parses");
            let output = format_shape(input).expect("match expression is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted match parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let left = format!("left_{}", "a".repeat(43));
        let right = format!("right_{}", "b".repeat(42));
        let input = format!("fn f(){{match {left} or {right}{{_=>1}}}}");
        let expected = format!(
            "fn f() {{\n    match (\n        {left}\n        or {right}\n    ) {{ _ => 1 }}\n}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("long match scrutinee parses");
        let output = format_shape(input.as_bytes()).expect("long match scrutinee formats");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("formatted scrutinee parses");
        assert_eq!(
            match_scrutinee_shape(parsed.root()),
            match_scrutinee_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn moves_a_long_match_arm_rhs_to_the_following_line() {
        let text = format!("\"{}\"", "x".repeat(86));
        let input = format!("fn f(){{match value{{_=>{text}}}}}");
        let expected = format!(
            "fn f() {{\n    match value {{\n        _ =>\n            {text}\n    }}\n}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("long match arm parses");
        let output = format_shape(input.as_bytes()).expect("long match arm is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("moved match arm parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_match_arm_lists_for_nested_forced_blocks() {
        let input = b"fn f(){match x{_=>consume(if ready {a()}else{b()}),.Other=>0}}";
        let parsed = parse_source(input).expect("nested forced-block match parses");
        let output = format_shape(input).expect("nested forced-block match formats");
        assert_eq!(
            output,
            concat!(
                "fn f() {\n",
                "    match x {\n",
                "        _ => consume(\n",
                "            if ready {\n",
                "                a()\n",
                "            } else {\n",
                "                b()\n",
                "            },\n",
                "        )\n",
                "        .Other => 0\n",
                "    }\n",
                "}\n",
            )
        );
        let reparsed = parse_source(output.as_bytes()).expect("formatted match parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_match_arm_lists_and_protects_nonfinal_open_ranges() {
        let first = format!(".Variant{}", "A".repeat(34));
        let second = format!(".Variant{}", "B".repeat(34));
        let input = format!(
            "fn f(){{match value{{{first}=>1,{second}=>2}}\nmatch value{{_=>10..,.Other=>{{work()}}}}\nmatch value{{_=>{{work()}},.Other=>20..}}\nmatch value{{_=>..,.Other=>{{work()}}}}}}"
        );
        let expected = format!(
            concat!(
                "fn f() {{\n",
                "    match value {{\n",
                "        {first} => 1\n",
                "        {second} => 2\n",
                "    }}\n",
                "    match value {{\n",
                "        _ => (10..)\n",
                "        .Other => {{\n",
                "            work()\n",
                "        }}\n",
                "    }}\n",
                "    match value {{\n",
                "        _ => {{\n",
                "            work()\n",
                "        }}\n",
                "        .Other => 20..\n",
                "    }}\n",
                "    match value {{\n",
                "        _ => (..)\n",
                "        .Other => {{\n",
                "            work()\n",
                "        }}\n",
                "    }}\n",
                "}}\n",
            ),
            first = first,
            second = second,
        );
        let parsed = parse_source(input.as_bytes()).expect("broken match input parses");
        let output = format_shape(input.as_bytes()).expect("broken match lists are supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken match output parses");
        assert_eq!(
            syntax_shape(parsed.root()),
            syntax_shape_with_match_range_parentheses_removed(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn reindents_multiline_strings_in_match_arms_and_breaks_the_list() {
        for (input, expected) in [
            (
                concat!(
                    "fn f(){match value{.Pending=>\"\"\"\n",
                    "        pending\n",
                    "        \"\"\"\n",
                    ".Other=>0}}",
                ),
                concat!(
                    "fn f() {\n",
                    "    match value {\n",
                    "        .Pending => \"\"\"\n",
                    "            pending\n",
                    "            \"\"\"\n",
                    "        .Other => 0\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                concat!(
                    "fn f(){match value{_=>\"first\\\n",
                    "        second\",.Other=>0}}",
                ),
                concat!(
                    "fn f() {\n",
                    "    match value {\n",
                    "        _ => \"first\\\n",
                    "            second\"\n",
                    "        .Other => 0\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                concat!(
                    "fn f(){match value{_=>b\"first\\\n",
                    "        second\",.Other=>0}}",
                ),
                concat!(
                    "fn f() {\n",
                    "    match value {\n",
                    "        _ => b\"first\\\n",
                    "        second\"\n",
                    "        .Other => 0\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                concat!(
                    "fn f(){match value{.Pending=>\"\"\"\r\n",
                    "        first\r\n",
                    "        second\r\n",
                    "        \"\"\"\n",
                    ".Other=>0}}",
                ),
                concat!(
                    "fn f() {\n",
                    "    match value {\n",
                    "        .Pending => \"\"\"\r\n",
                    "            first\r\n",
                    "            second\r\n",
                    "            \"\"\"\n",
                    "        .Other => 0\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                concat!(
                    "fn f(){match value{_=>r#\"first\n",
                    "      second\"#,.Other=>0}}",
                ),
                concat!(
                    "fn f() {\n",
                    "    match value {\n",
                    "        _ => r#\"first\n",
                    "      second\"#\n",
                    "        .Other => 0\n",
                    "    }\n",
                    "}\n",
                ),
            ),
            (
                concat!(
                    "fn f(){match value{_=>br#\"first\n",
                    "  second\"#,.Other=>0}}",
                ),
                concat!(
                    "fn f() {\n",
                    "    match value {\n",
                    "        _ => br#\"first\n",
                    "  second\"#\n",
                    "        .Other => 0\n",
                    "    }\n",
                    "}\n",
                ),
            ),
        ] {
            let parsed = parse_source(input.as_bytes()).expect("multiline match parses");
            let output = format_shape(input.as_bytes()).expect("multiline string formats");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted string reparses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn raw_crlf_string_width_excludes_the_carriage_return() {
        let content = "A".repeat(84);
        let input = format!("fn f(){{match x{{_=>r#\"{content}\r\nnext\"#,.Other=>0}}}}");
        let first_line = format!("        _ => r#\"{content}");
        assert_eq!(first_line.chars().count(), 100);
        let expected = format!(
            "fn f() {{\n    match x {{\n{first_line}\r\nnext\"#\n        .Other => 0\n    }}\n}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("raw CRLF match parses");
        let output = format_shape(input.as_bytes()).expect("raw CRLF match formats");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted raw CRLF parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn triple_quoted_crlf_string_width_excludes_the_carriage_return() {
        let name = "a".repeat(85);
        let input = format!(
            "fn f(){{match x{{{name}=>\"\"\"\r\n        x\r\n        \"\"\"\r\n.Other=>0}}}}"
        );
        let first_line = format!("        {name} => \"\"\"");
        assert_eq!(first_line.chars().count(), 100);
        let expected = format!(
            "fn f() {{\n    match x {{\n{first_line}\r\n            x\r\n            \"\"\"\n        .Other => 0\n    }}\n}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("triple CRLF match parses");
        let output = format_shape(input.as_bytes()).expect("triple CRLF match formats");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted triple CRLF parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_if_let_conditions_with_patterns() {
        for (input, expected) in [
            (
                b"fn f(){if let value=input {consume(value)}}".as_slice(),
                "fn f() {\n    if let value = input {\n        consume(value)\n    }\n}\n",
            ),
            (
                b"fn f(){if let _=input {tick()}else{fallback()}}".as_slice(),
                "fn f() {\n    if let _ = input {\n        tick()\n    } else {\n        fallback()\n    }\n}\n",
            ),
            (
                b"fn f(){if let borrow mut value=next(){consume(value)}}".as_slice(),
                "fn f() {\n    if let borrow mut value = next() {\n        consume(value)\n    }\n}\n",
            ),
            (
                b"fn f(){if let value=input {one()}else if ready{two()}else{three()}}".as_slice(),
                "fn f() {\n    if let value = input {\n        one()\n    } else if ready {\n        two()\n    } else {\n        three()\n    }\n}\n",
            ),
            (
                b"fn f(){if let Some(value)=input {consume(value)}}".as_slice(),
                "fn f() {\n    if let Some(value) = input {\n        consume(value)\n    }\n}\n",
            ),
            (
                b"fn f(){if let Some()=input {tick()}}".as_slice(),
                "fn f() {\n    if let Some() = input {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){if let |.Pending|.Active=input {tick()}}".as_slice(),
                "fn f() {\n    if let .Pending | .Active = input {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){if let .Disabled(..)=input {tick()}}".as_slice(),
                "fn f() {\n    if let .Disabled(..) = input {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){if let User(id,name:label,..)=input {consume(label)}}".as_slice(),
                "fn f() {\n    if let User(id, name: label, ..) = input {\n        consume(label)\n    }\n}\n",
            ),
            (
                b"fn f(){if let [first,..rest,last]=input {consume(rest)}}".as_slice(),
                "fn f() {\n    if let [first, ..rest, last] = input {\n        consume(rest)\n    }\n}\n",
            ),
            (
                b"fn f(){if let name@module.State.Ready=input {consume(name)}}".as_slice(),
                "fn f() {\n    if let name @ module.State.Ready = input {\n        consume(name)\n    }\n}\n",
            ),
            (
                b"fn f(){if let (left,right)=input {consume(left)}}".as_slice(),
                "fn f() {\n    if let (left, right) = input {\n        consume(left)\n    }\n}\n",
            ),
            (
                b"fn f(){if let (value,)=input {consume(value)}}".as_slice(),
                "fn f() {\n    if let (value,) = input {\n        consume(value)\n    }\n}\n",
            ),
            (
                b"fn f(){if let ()=input {tick()}}".as_slice(),
                "fn f() {\n    if let () = input {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){if let (value)=input {consume(value)}}".as_slice(),
                "fn f() {\n    if let (value) = input {\n        consume(value)\n    }\n}\n",
            ),
            (
                b"fn f(){if let 0x2a..=0xff=input {tick()}}".as_slice(),
                "fn f() {\n    if let 0x2A..=0xFF = input {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){if let -1=input {tick()}}".as_slice(),
                "fn f() {\n    if let -1 = input {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){if let ..=MAX=input {tick()}}".as_slice(),
                "fn f() {\n    if let ..=MAX = input {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){if let 10.. = input {tick()}}".as_slice(),
                "fn f() {\n    if let 10.. = input {\n        tick()\n    }\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("if-let condition parses");
            let output = format_shape(input).expect("if-let condition pattern formats");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted if-let parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_long_or_patterns_after_each_alternative_separator() {
        let first = format!("Variant{}", "A".repeat(37));
        let second = format!("Variant{}", "B".repeat(37));
        let input = format!("fn f(){{if let {first}|{second}=value{{tick()}}}}");
        let expected = format!(
            "fn f() {{\n    if let {first} |\n        {second} = value {{\n        tick()\n    }}\n}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("long or-pattern parses");
        let output = format_shape(input.as_bytes()).expect("long or-pattern formats");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken or-pattern reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_constructor_pattern_arguments_without_comma_after_final_rest() {
        let first = format!("first_{}", "a".repeat(36));
        let second = format!("second_{}", "b".repeat(36));
        let input = format!("fn f(){{if let Some({first},{second},..)=value{{tick()}}}}");
        let expected = format!(
            "fn f() {{\n    if let Some(\n        {first},\n        {second},\n        ..\n    ) = value {{\n        tick()\n    }}\n}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("long constructor pattern parses");
        let output = format_shape(input.as_bytes()).expect("long constructor pattern formats");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed =
            parse_source(output.as_bytes()).expect("broken constructor pattern reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_single_element_slice_patterns_with_a_trailing_comma() {
        let binding = format!("value_{}", "x".repeat(85));
        let input = format!("fn f(){{if let [{binding}]=values{{tick()}}}}");
        let expected = format!(
            "fn f() {{\n    if let [\n        {binding},\n    ] = values {{\n        tick()\n    }}\n}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("long slice pattern parses");
        let output = format_shape(input.as_bytes()).expect("long slice pattern formats");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken slice pattern reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_long_if_conditions_with_parentheses() {
        let left = format!("first_{}", "a".repeat(43));
        let right = format!("second_{}", "b".repeat(42));
        let input = format!("fn f(){{if {left} or {right}{{act()}}}}");
        let expected = format!(
            "fn f() {{\n    if (\n        {left}\n        or {right}\n    ) {{\n        act()\n    }}\n}}\n"
        );

        parse_source(input.as_bytes()).expect("long condition parses");
        let output = format_shape(input.as_bytes()).expect("long condition is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        parse_source(output.as_bytes()).expect("wrapped condition reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn breaks_long_if_let_initializers_with_parentheses() {
        let left = format!("first_{}", "a".repeat(43));
        let right = format!("second_{}", "b".repeat(42));
        let input = format!("fn f(){{if let value={left} or {right}{{consume(value)}}}}");
        let expected = format!(
            "fn f() {{\n    if let value = (\n        {left}\n        or {right}\n    ) {{\n        consume(value)\n    }}\n}}\n"
        );

        parse_source(input.as_bytes()).expect("long if-let initializer parses");
        let output = format_shape(input.as_bytes()).expect("long if-let initializer is supported");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        parse_source(output.as_bytes()).expect("wrapped if-let initializer reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_loop_expressions_and_labels() {
        for (input, expected) in [
            (
                b"fn f(){loop {tick()}}".as_slice(),
                "fn f() {\n    loop {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){'outer:\nloop {break 'outer}}".as_slice(),
                "fn f() {\n    'outer: loop {\n        break 'outer\n    }\n}\n",
            ),
            (b"fn f(){'scan:loop{}}", "fn f() {\n    'scan: loop {}\n}\n"),
        ] {
            let parsed = parse_source(input).expect("loop expression parses");
            let output = format_shape(input).expect("loop expression is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted loop parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_while_expressions_and_labels() {
        for (input, expected) in [
            (
                b"fn f(){while ready {tick()}}".as_slice(),
                "fn f() {\n    while ready {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){'scan:\nwhile ready {break 'scan}}".as_slice(),
                "fn f() {\n    'scan: while ready {\n        break 'scan\n    }\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("while expression parses");
            let output = format_shape(input).expect("while expression is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted while parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_while_let_conditions_with_simple_patterns() {
        for (input, expected) in [
            (
                b"fn f(){while let value=next() {consume(value)}}".as_slice(),
                "fn f() {\n    while let value = next() {\n        consume(value)\n    }\n}\n",
            ),
            (
                b"fn f(){'scan:\nwhile let _=next() {tick()}}".as_slice(),
                "fn f() {\n    'scan: while let _ = next() {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){while let Some((left,right))=next() {consume(left)}}".as_slice(),
                "fn f() {\n    while let Some((left, right)) = next() {\n        consume(left)\n    }\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("while-let condition parses");
            let output = format_shape(input).expect("while-let pattern formats");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted while-let parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_closures_with_supported_parameters_and_body_layouts() {
        for (input, expected) in [
            (
                b"fn f(){fn(x){x+1}}".as_slice(),
                "fn f() {\n    fn(x) { x + 1 }\n}\n",
            ),
            (
                b"fn f(){move fn(var value:I32,other:Str)->I32{value}}".as_slice(),
                "fn f() {\n    move fn(var value: I32, other: Str) -> I32 { value }\n}\n",
            ),
            (b"fn f(){fn(){}}", "fn f() {\n    fn() {}\n}\n"),
            (
                b"fn f(){consume(fn(value){value})}".as_slice(),
                "fn f() {\n    consume(fn(value) { value })\n}\n",
            ),
            (
                b"fn f(){fn(x){let y=x\ny}}".as_slice(),
                "fn f() {\n    fn(x) {\n        let y = x\n        y\n    }\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("closure expression parses");
            let output = format_shape(input).expect("closure expression is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted closure parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn breaks_a_long_single_expression_closure_body() {
        let left = format!("left_{}", "a".repeat(43));
        let right = format!("right_{}", "b".repeat(42));
        let input = format!("fn f(){{fn(x){{{left} and {right}}}}}");
        let expected = format!(
            "fn f() {{\n    fn(x) {{\n        (\n            {left}\n            and {right}\n        )\n    }}\n}}\n"
        );

        parse_source(input.as_bytes()).expect("long closure body parses");
        let output = format_shape(input.as_bytes()).expect("long closure body formats");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        parse_source(output.as_bytes()).expect("broken closure body reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn hugs_a_broken_last_closure_argument_to_the_call_line() {
        let input = b"fn f(){collect(items,fn(x){let y=x\ny})}";
        let expected =
            "fn f() {\n    collect(items, fn(x) {\n        let y = x\n        y\n    })\n}\n";
        let parsed = parse_source(input).expect("hugged closure argument parses");
        let output = format_shape(input).expect("hugged closure argument formats");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("hugged closure reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn hugs_a_single_expression_closure_when_only_its_body_breaks() {
        let left = format!("left_{}", "a".repeat(43));
        let right = format!("right_{}", "b".repeat(42));
        let input = format!("fn f(){{collect(items,fn(x){{{left} and {right}}})}}");
        let expected = format!(
            "fn f() {{\n    collect(items, fn(x) {{\n        (\n            {left}\n            and {right}\n        )\n    }})\n}}\n"
        );

        parse_source(input.as_bytes()).expect("long closure argument parses");
        let output = format_shape(input.as_bytes()).expect("long closure argument formats");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        parse_source(output.as_bytes()).expect("hugged long closure reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn hugs_a_named_last_closure_argument_but_not_an_earlier_one() {
        for (input, expected) in [
            (
                b"fn f(){collect(items,callback:fn(x){let y=x\ny})}".as_slice(),
                "fn f() {\n    collect(items, callback: fn(x) {\n        let y = x\n        y\n    })\n}\n",
            ),
            (
                b"fn f(){collect(fn(x){let y=x\ny},items)}".as_slice(),
                "fn f() {\n    collect(\n        fn(x) {\n            let y = x\n            y\n        },\n        items,\n    )\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("closure argument parses");
            let output = format_shape(input).expect("closure argument formats");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted call reparses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn falls_back_to_broken_arguments_when_a_hugged_header_would_overflow() {
        let argument = format!("argument_{}", "a".repeat(90));
        let input = format!("fn f(){{collect({argument},fn(x){{let y=x\ny}})}}");
        let expected = format!(
            "fn f() {{\n    collect(\n        {argument},\n        fn(x) {{\n            let y = x\n            y\n        }},\n    )\n}}\n"
        );

        parse_source(input.as_bytes()).expect("overlong call head parses");
        let output = format_shape(input.as_bytes()).expect("overlong call head formats");
        assert_eq!(output, expected);
        parse_source(output.as_bytes()).expect("broken closure argument reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn forces_member_chain_breaks_for_a_multiline_nonfinal_closure() {
        let input = b"fn f(){items.map(fn(x){let y=x\ny}).filter(fn(y){y>0})}";
        let expected = "fn f() {\n    items\n        .map(fn(x) {\n            let y = x\n            y\n        })\n        .filter(fn(y) { y > 0 })\n}\n";
        let parsed = parse_source(input).expect("closure member chain parses");
        let output = format_shape(input).expect("closure member chain formats");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted member chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn does_not_force_member_chain_breaks_for_a_multiline_final_closure() {
        let input = b"fn f(){items.map(fn(x){x+1}).filter(fn(y){let z=y\nz})}";
        let expected = "fn f() {\n    items.map(fn(x) { x + 1 }).filter(fn(y) {\n        let z = y\n        z\n    })\n}\n";
        let parsed = parse_source(input).expect("final closure member chain parses");
        let output = format_shape(input).expect("final closure member chain formats");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted chain reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn forces_member_chain_breaks_for_a_multiline_closure_in_the_head_call() {
        let input = b"fn f(){factory(fn(x){let y=x\ny}).first().second()}";
        let expected = "fn f() {\n    factory(fn(x) {\n        let y = x\n        y\n    })\n        .first()\n        .second()\n}\n";
        let parsed = parse_source(input).expect("head closure member chain parses");
        let output = format_shape(input).expect("head closure member chain formats");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted head chain parses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn forces_parenthesized_head_member_chains_for_a_multiline_closure() {
        let input = b"fn f(){(factory(fn(x){let y=x\ny})).first().second()}";
        let expected = "fn f() {\n    (factory(fn(x) {\n            let y = x\n            y\n        }))\n        .first()\n        .second()\n}\n";
        let parsed = parse_source(input).expect("parenthesized head closure chain parses");
        let output = format_shape(input).expect("parenthesized head closure chain formats");
        assert_eq!(output, expected);
        let reparsed =
            parse_source(output.as_bytes()).expect("formatted parenthesized chain reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn keeps_malformed_closure_parameters_fail_closed() {
        assert_eq!(
            format_shape(b"fn f(){fn(var _){}}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
    }

    #[test]
    fn formats_for_expressions_with_simple_patterns() {
        for (input, expected) in [
            (
                b"fn f(){for row in rows {consume(row)}}".as_slice(),
                "fn f() {\n    for row in rows {\n        consume(row)\n    }\n}\n",
            ),
            (
                b"fn f(){for _ in rows {tick()}}".as_slice(),
                "fn f() {\n    for _ in rows {\n        tick()\n    }\n}\n",
            ),
            (
                b"fn f(){for var row in rows {row+=1}}".as_slice(),
                "fn f() {\n    for var row in rows {\n        row += 1\n    }\n}\n",
            ),
            (
                b"fn f(){for borrow mut row in rows {mutate(row)}}".as_slice(),
                "fn f() {\n    for borrow mut row in rows {\n        mutate(row)\n    }\n}\n",
            ),
            (
                b"fn f(){'rows:\nfor row in rows {break 'rows}}".as_slice(),
                "fn f() {\n    'rows: for row in rows {\n        break 'rows\n    }\n}\n",
            ),
            (
                b"fn f(){for await row in rows {consume(row)}}".as_slice(),
                "fn f() {\n    for await row in rows {\n        consume(row)\n    }\n}\n",
            ),
            (
                b"fn f(){for (left,right) in pairs {consume(left)}}".as_slice(),
                "fn f() {\n    for (left, right) in pairs {\n        consume(left)\n    }\n}\n",
            ),
            (
                b"fn f(){for [head,..tail] in rows {consume(head)}}".as_slice(),
                "fn f() {\n    for [head, ..tail] in rows {\n        consume(head)\n    }\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("for expression parses");
            let output = format_shape(input).expect("for expression pattern is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted for parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_destructuring_let_statements() {
        let input = b"fn f(){let (left,right)=pair\nconsume(left)}";
        let expected = "fn f() {\n    let (left, right) = pair\n    consume(left)\n}\n";
        let parsed = parse_source(input).expect("destructuring let parses");
        let output = format_shape(input).expect("destructuring let formats");
        assert_eq!(output, expected);
        let reparsed = parse_source(output.as_bytes()).expect("formatted let reparses");
        assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_let_else_statements() {
        for (input, expected) in [
            (
                b"fn f(){let Some(value)=next()else{return}\nvalue}".as_slice(),
                concat!(
                    "fn f() {\n",
                    "    let Some(value) = next() else {\n",
                    "        return\n",
                    "    }\n",
                    "    value\n",
                    "}\n",
                ),
            ),
            (
                b"fn f(){let (Some(value),flag):(Option<I32>,Bool)=pair\nelse{fallback()}\nconsume(value)}",
                concat!(
                    "fn f() {\n",
                    "    let (Some(value), flag): (Option<I32>, Bool) = pair else {\n",
                    "        fallback()\n",
                    "    }\n",
                    "    consume(value)\n",
                    "}\n",
                ),
            ),
            (
                b"fn f(){let Some(value)=if ready{next()}else{fallback()}else{return}\nconsume(value)}",
                concat!(
                    "fn f() {\n",
                    "    let Some(value) = if ready {\n",
                    "        next()\n",
                    "    } else {\n",
                    "        fallback()\n",
                    "    } else {\n",
                    "        return\n",
                    "    }\n",
                    "    consume(value)\n",
                    "}\n",
                ),
            ),
        ] {
            let parsed = parse_source(input).expect("let-else statement parses");
            let output = format_shape(input).expect("let-else statement formats");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted let-else reparses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        for (binding_length, repeated_chars) in [(78, 70), (85, 77)] {
            let binding = format!("binding_{}", "a".repeat(repeated_chars));
            assert_eq!(binding.len(), binding_length);
            let input = format!("fn f(){{let {binding}=value else{{return}}}}");
            let expected = format!(
                "fn f() {{\n    let {binding} =\n        value else {{\n            return\n        }}\n}}\n"
            );
            let parsed = parse_source(input.as_bytes()).expect("long let-else statement parses");
            let output = format_shape(input.as_bytes()).expect("long let-else statement formats");
            assert_eq!(output, expected);
            assert!(output.lines().all(|line| line.chars().count() <= 100));
            let reparsed =
                parse_source(output.as_bytes()).expect("formatted long let-else reparses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let binding = format!("binding_{}", "a".repeat(81));
        let first = format!("first_{}", "b".repeat(44));
        let second = format!("second_{}", "c".repeat(44));
        let input = format!("fn f(){{let {binding}={first}+{second} else{{return}}}}");
        let expected = format!(
            "fn f() {{\n    let {binding} =\n        (\n            {first}\n            + {second}\n        ) else {{\n            return\n        }}\n}}\n"
        );
        let parsed = parse_source(input.as_bytes()).expect("broken let-else statement parses");
        let output = format_shape(input.as_bytes()).expect("broken let-else statement formats");
        assert_eq!(output, expected);
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed = parse_source(output.as_bytes()).expect("broken let-else reparses");
        assert_eq!(
            let_initializer_shape(parsed.root()),
            let_initializer_shape(reparsed.root())
        );
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn formats_async_and_region_block_expressions() {
        for (input, expected) in [
            (
                b"fn f(){async {work()}}".as_slice(),
                "fn f() {\n    async {\n        work()\n    }\n}\n",
            ),
            (b"fn f(){async move{}}", "fn f() {\n    async move {}\n}\n"),
            (
                b"fn f(){region arena{work()}}".as_slice(),
                "fn f() {\n    region arena {\n        work()\n    }\n}\n",
            ),
            (
                b"fn f(){region arena{}}",
                "fn f() {\n    region arena {}\n}\n",
            ),
            (b"fn f(){region}", "fn f() {\n    region\n}\n"),
        ] {
            let parsed = parse_source(input).expect("block wrapper parses");
            let output = format_shape(input).expect("async or region block is supported");
            assert_eq!(output, expected);
            let reparsed =
                parse_source(output.as_bytes()).expect("formatted block wrapper reparses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }
    }

    #[test]
    fn formats_unsafe_blocks_flat_only_for_one_fitting_expression() {
        for (input, expected) in [
            (
                b"fn f(){unsafe{}}".as_slice(),
                "fn f() {\n    unsafe {}\n}\n",
            ),
            (
                b"fn f(){unsafe{operate()}}".as_slice(),
                "fn f() {\n    unsafe { operate() }\n}\n",
            ),
            (
                b"fn f(){unsafe{unsafe{operate()}}}".as_slice(),
                "fn f() {\n    unsafe { unsafe { operate() } }\n}\n",
            ),
            (
                b"fn f(){unsafe{let value=1}}".as_slice(),
                "fn f() {\n    unsafe {\n        let value = 1\n    }\n}\n",
            ),
            (
                b"fn f(){unsafe{one()\ntwo()}}".as_slice(),
                "fn f() {\n    unsafe {\n        one()\n        two()\n    }\n}\n",
            ),
        ] {
            let parsed = parse_source(input).expect("unsafe block parses");
            let output = format_shape(input).expect("unsafe block is supported");
            assert_eq!(output, expected);
            let reparsed = parse_source(output.as_bytes()).expect("formatted unsafe block parses");
            assert_eq!(syntax_shape(parsed.root()), syntax_shape(reparsed.root()));
            assert_eq!(format_shape(output.as_bytes()), Ok(output));
        }

        let operation = format!("operation_{}", "x".repeat(101));
        let input = format!("fn f(){{unsafe{{{operation}}}}}");
        let expected = format!("fn f() {{\n    unsafe {{\n        {operation}\n    }}\n}}\n");
        let output = format_shape(input.as_bytes()).expect("overlong unsafe expression formats");
        assert_eq!(output, expected);
        parse_source(output.as_bytes()).expect("broken unsafe block reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));

        let nested_operation = format!("nested_{}", "y".repeat(92));
        let input = format!("fn f(){{unsafe{{unsafe{{{nested_operation}}}}}}}");
        let expected = format!(
            "fn f() {{\n    unsafe {{\n        unsafe {{\n            {nested_operation}\n        }}\n    }}\n}}\n"
        );
        let output = format_shape(input.as_bytes()).expect("nested overlong unsafe block formats");
        assert_eq!(output, expected);
        parse_source(output.as_bytes()).expect("nested broken unsafe block reparses");
        assert_eq!(format_shape(output.as_bytes()), Ok(output));
    }

    #[test]
    fn normalizes_float_literals_and_formats_other_simple_literal_kinds() {
        assert_eq!(
            format_shape(b"fn f(){let x=1.5E-3\nx}"),
            Ok("fn f() {\n    let x = 1.5e-3\n    x\n}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(){let x=true\nx}"),
            Ok("fn f() {\n    let x = true\n    x\n}\n".to_owned())
        );
        assert_eq!(
            format_shape(b"fn f(){let x=\"value\"\nx}"),
            Ok("fn f() {\n    let x = \"value\"\n    x\n}\n".to_owned())
        );

        for source in [
            b"fn f(){}".as_slice(),
            b"fn f(){}\n\nfn g(){}",
            b"fn f(){let x=false\nx}",
            b"fn f(){let x=1.5E-3\nx}",
        ] {
            let once = format_shape(source).expect("source has a supported shape");
            assert_eq!(format_shape(once.as_bytes()), Ok(once));
        }
    }

    #[test]
    fn normalizes_hexadecimal_digits_and_moves_long_rhs() {
        assert_eq!(
            format_shape(b"fn f(){let x=0xff\nx}"),
            Ok("fn f() {\n    let x = 0xFF\n    x\n}\n".to_owned())
        );

        let binding = "x".repeat(70);
        let literal = "340282366920938463463374607431768211455";
        let input = format!("fn f(){{let {binding}={literal}\n{binding}}}");
        let parsed_input = parse_source(input.as_bytes()).expect("long input parses");
        let output = format_shape(input.as_bytes()).expect("supported long binding");
        assert!(output.contains(&format!("let {binding} =\n        {literal}\n")));
        assert!(output.lines().all(|line| line.chars().count() <= 100));
        let reparsed_output = parse_source(output.as_bytes()).expect("wrapped output parses");
        assert_eq!(
            syntax_shape(parsed_input.root()),
            syntax_shape(reparsed_output.root())
        );
    }

    #[test]
    fn output_reparses_to_the_same_nontrivia_cst_shape() {
        let input = parse_source(b"fn  f ( ) {\n let x = 0xff\n\n x\n}\n").expect("input parses");
        let output =
            format_shape(b"fn  f ( ) {\n let x = 0xff\n\n x\n}\n").expect("input is supported");
        let reparsed = parse_source(output.as_bytes()).expect("formatted source parses");

        assert_eq!(syntax_shape(input.root()), syntax_shape(reparsed.root()));
    }

    #[test]
    fn unsupported_shapes_and_comments_fail_closed() {
        assert_eq!(
            format_shape(b"fn f(self: Foo) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(self: Self.Item) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(self: borrow Self.Item) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(x: I32, self: Self) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(self: Self, self: Self) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"fn f(self: Self, x: I32, self: Self) {}"),
            Err(SourceDocumentError::UnsupportedShape)
        );
        assert_eq!(
            format_shape(b"// header\nfn f(){let x=1\nx}"),
            Ok(String::from(
                "// header\nfn f() {\n    let x = 1\n    x\n}\n"
            ))
        );
        assert_eq!(
            format_shape(b"fn f(){} // trailing"),
            Ok(String::from("fn f() {} // trailing\n"))
        );
        assert_eq!(
            format_shape(b"fn f(){} /* trailing */"),
            Ok(String::from("fn f() {} /* trailing */\n"))
        );
        assert_eq!(
            format_shape(b"fn f(){}\n/// trailing"),
            Ok(String::from("fn f() {}\n/// trailing\n"))
        );
    }

    #[derive(Debug, Eq, PartialEq)]
    enum ShapeElement {
        NodeStart(SyntaxKind),
        NodeEnd,
        Token(SyntaxKind, String),
    }

    enum Work {
        Node(SyntaxNode),
        Token(SyntaxToken),
        NodeEnd,
    }

    fn syntax_shape(root: &SyntaxNode) -> Vec<ShapeElement> {
        syntax_shape_impl(root, false)
    }

    fn let_initializer_shape(root: &SyntaxNode) -> Vec<ShapeElement> {
        let initializer = root
            .descendants()
            .filter_map(Stmt::cast)
            .find_map(|statement| match statement {
                Stmt::Let(statement) => statement.initializer(),
                _ => None,
            })
            .expect("let initializer exists");
        match initializer {
            Expr::Paren(paren) => {
                let expression = paren
                    .expression()
                    .expect("parenthesized initializer exists");
                syntax_shape(expression.syntax())
            }
            expression => syntax_shape(expression.syntax()),
        }
    }

    fn match_scrutinee_shape(root: &SyntaxNode) -> Vec<ShapeElement> {
        let match_expr = root
            .descendants()
            .filter_map(Expr::cast)
            .find_map(|expression| match expression {
                Expr::Match(match_expr) => Some(match_expr),
                _ => None,
            })
            .expect("match expression exists");
        let scrutinee = match_expr.scrutinee().expect("match scrutinee exists");
        match scrutinee {
            Expr::Paren(paren) => {
                let inner = paren.expression().expect("scrutinee expression exists");
                syntax_shape(inner.syntax())
            }
            expression => syntax_shape(expression.syntax()),
        }
    }

    fn syntax_shape_with_match_range_parentheses_removed(root: &SyntaxNode) -> Vec<ShapeElement> {
        syntax_shape_impl(root, true)
    }

    fn syntax_shape_impl(
        root: &SyntaxNode,
        remove_open_range_parentheses: bool,
    ) -> Vec<ShapeElement> {
        let mut shape = Vec::new();
        let mut pending = vec![Work::Node(root.clone())];
        while let Some(work) = pending.pop() {
            match work {
                Work::Node(node) => {
                    if remove_open_range_parentheses
                        && node.kind() == SyntaxKind::PAREN_EXPR
                        && node
                            .parent()
                            .is_some_and(|parent| parent.kind() == SyntaxKind::MATCH_ARM)
                        && matches!(
                            Expr::cast(node.clone()),
                            Some(Expr::Paren(paren))
                                if matches!(
                                    paren.expression(),
                                    Some(Expr::Range(range)) if range.end().is_none()
                                )
                        )
                    {
                        let inner = match Expr::cast(node) {
                            Some(Expr::Paren(paren)) => {
                                paren.expression().expect("parenthesized range exists")
                            }
                            _ => unreachable!("the node is a parenthesized expression"),
                        };
                        pending.push(Work::Node(inner.syntax().clone()));
                        continue;
                    }
                    shape.push(ShapeElement::NodeStart(node.kind()));
                    pending.push(Work::NodeEnd);
                    let mut elements = node.children_with_tokens().collect::<Vec<_>>();
                    if matches!(
                        node.kind(),
                        SyntaxKind::EFFECT_SET
                            | SyntaxKind::BORROWS_CLAUSE
                            | SyntaxKind::MATCH_ARM_LIST
                            | SyntaxKind::FIELD_LIST
                            | SyntaxKind::VARIANT_LIST
                    ) {
                        elements.retain(
                            |element| !matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA),
                        );
                    } else {
                        let trailing_comma_close = match node.kind() {
                            SyntaxKind::PARAM_LIST
                            | SyntaxKind::ARG_LIST
                            | SyntaxKind::ATTR_ARGS
                            | SyntaxKind::FN_PTR_TYPE
                            | SyntaxKind::FN_SUGAR_BOUND
                            | SyntaxKind::CTOR_PAT_ARGS => Some(SyntaxKind::R_PAREN),
                            SyntaxKind::TUPLE_PAT
                                if node.children().filter_map(Pattern::cast).count() != 1 =>
                            {
                                Some(SyntaxKind::R_PAREN)
                            }
                            SyntaxKind::SLICE_PAT => Some(SyntaxKind::R_BRACKET),
                            SyntaxKind::GENERIC_ARG_LIST | SyntaxKind::GENERIC_PARAM_LIST => {
                                Some(SyntaxKind::GT)
                            }
                            SyntaxKind::TUPLE_SHAPE | SyntaxKind::RECORD_SHAPE => {
                                Some(SyntaxKind::R_PAREN)
                            }
                            SyntaxKind::TUPLE_TYPE
                                if node.children().filter_map(Type::cast).count() != 1 =>
                            {
                                Some(SyntaxKind::R_PAREN)
                            }
                            SyntaxKind::TUPLE_EXPR
                                if node.children().filter_map(Expr::cast).count() != 1 =>
                            {
                                Some(SyntaxKind::R_PAREN)
                            }
                            SyntaxKind::ARRAY_EXPR => Some(SyntaxKind::R_BRACKET),
                            _ => None,
                        };
                        if let Some(close) = trailing_comma_close {
                            let trailing_comma = elements.windows(2).position(|pair| {
                                matches!(&pair[0], SyntaxElement::Token(token) if token.kind() == SyntaxKind::COMMA)
                                    && matches!(&pair[1], SyntaxElement::Token(token) if token.kind() == close)
                            });
                            if let Some(index) = trailing_comma {
                                elements.remove(index);
                            }
                        }
                    }
                    if node.kind() == SyntaxKind::OR_PAT {
                        let first_significant = elements.iter().position(|element| {
                            !matches!(element, SyntaxElement::Token(token) if token.kind() == SyntaxKind::NEWLINE)
                        });
                        if first_significant.is_some_and(|index| {
                            matches!(
                                elements.get(index),
                                Some(SyntaxElement::Token(token))
                                    if token.kind() == SyntaxKind::PIPE
                            )
                        }) {
                            elements.remove(first_significant.expect("significant element exists"));
                        }
                    }
                    pending.extend(elements.into_iter().rev().map(|element| match element {
                        SyntaxElement::Node(node) => Work::Node(node),
                        SyntaxElement::Token(token) => Work::Token(token),
                    }));
                }
                Work::Token(token) => {
                    if !matches!(token.kind(), SyntaxKind::NEWLINE | SyntaxKind::EOF) {
                        let text = match token.kind() {
                            SyntaxKind::INT => super::normalize_integer_literal(token.text()),
                            SyntaxKind::FLOAT => token.text().replace('E', "e"),
                            SyntaxKind::DOC_OUTER => {
                                token.text().trim_end_matches([' ', '\t']).to_owned()
                            }
                            SyntaxKind::STRING if token.text().contains(['\n', '\r']) => {
                                normalized_string_value(token.text())
                                    .unwrap_or_else(|| token.text().to_owned())
                            }
                            _ => token.text().to_owned(),
                        };
                        shape.push(ShapeElement::Token(token.kind(), text));
                    }
                }
                Work::NodeEnd => shape.push(ShapeElement::NodeEnd),
            }
        }
        shape
    }

    fn contract_expression_shape(root: &SyntaxNode) -> Vec<ShapeElement> {
        let clause = root
            .descendants()
            .find(|node| node.kind() == SyntaxKind::REQUIRES_CLAUSE)
            .expect("requires clause exists");
        let expression = clause
            .children()
            .find_map(Expr::cast)
            .expect("requires expression exists");
        match expression {
            Expr::Paren(paren) => {
                let inner = paren.expression().expect("parenthesized expression exists");
                syntax_shape(inner.syntax())
            }
            expression => syntax_shape(expression.syntax()),
        }
    }
}
