//! Package-contract document builders.
//!
//! Builders are added declaration by declaration and reject inputs they cannot
//! render without losing syntax or comments.
#![allow(dead_code)]

use std::collections::HashMap;
use std::ops::Range;

use sai_package::lexer::{PackagePunctuator, PackageToken, PackageTokenKind, PackageTriviaKind};

use crate::package_input::{AttachedComment, CommentAttachment, PackageInput};
use crate::package_surface::{PackageBraceList, PackageBraceOwner, PackageSyntaxIndex};
use crate::{Doc, GroupId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageDocumentError {
    InputRefused,
    WrongListOwner,
    InvalidEntry,
    UnknownCommentAnchor,
}

/// Build a canonical document for one syntactically valid `targets` list.
///
/// Comments attached to the declaration are retained. Comments attached to
/// removed commas follow the formatter's token-rehoming rule.
pub(crate) fn targets_declaration(
    input: &PackageInput<'_>,
    index: &PackageSyntaxIndex<'_>,
    list: &PackageBraceList,
) -> Result<Doc, PackageDocumentError> {
    comma_list_declaration(input, index, list, PackageBraceOwner::Targets)
}

pub(crate) fn capability_declaration(
    input: &PackageInput<'_>,
    index: &PackageSyntaxIndex<'_>,
    list: &PackageBraceList,
) -> Result<Doc, PackageDocumentError> {
    match list.owner {
        PackageBraceOwner::Allow | PackageBraceOwner::Deny => {
            comma_list_declaration(input, index, list, list.owner)
        }
        _ => Err(PackageDocumentError::WrongListOwner),
    }
}

pub(crate) fn authors_declaration(
    input: &PackageInput<'_>,
    index: &PackageSyntaxIndex<'_>,
    list: &PackageBraceList,
) -> Result<Doc, PackageDocumentError> {
    comma_list_declaration(input, index, list, PackageBraceOwner::Authors)
}

pub(crate) fn artifact_components_declaration(
    input: &PackageInput<'_>,
    index: &PackageSyntaxIndex<'_>,
    list: &PackageBraceList,
) -> Result<Doc, PackageDocumentError> {
    if !input.refusals().is_empty() {
        return Err(PackageDocumentError::InputRefused);
    }
    if list.owner != PackageBraceOwner::ArtifactComponents
        || list.kind() != crate::package_surface::PackageListKind::LinesOnly
        || !index.contains_list(list)
        || !index.matches_input(input)
    {
        return Err(PackageDocumentError::WrongListOwner);
    }

    let tokens = &input.lexed().tokens;
    let opening_brace =
        usize::try_from(list.open_token).map_err(|_| PackageDocumentError::InvalidEntry)?;
    let closing_brace =
        usize::try_from(list.close_token).map_err(|_| PackageDocumentError::InvalidEntry)?;
    if opening_brace >= closing_brace {
        return Err(PackageDocumentError::InvalidEntry);
    }
    let keyword_index = opening_brace
        .checked_sub(2)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    let [artifact, component] = tokens
        .get(keyword_index..opening_brace)
        .ok_or(PackageDocumentError::InvalidEntry)?
    else {
        return Err(PackageDocumentError::InvalidEntry);
    };
    if artifact.kind != PackageTokenKind::Word
        || token_text(input.source(), artifact) != "artifact"
        || component.kind != PackageTokenKind::Word
        || token_text(input.source(), component) != "component"
        || tokens.get(opening_brace).is_none_or(|token| {
            token.kind != PackageTokenKind::Punctuator(PackagePunctuator::LeftBrace)
        })
        || tokens.get(closing_brace).is_none_or(|token| {
            token.kind != PackageTokenKind::Punctuator(PackagePunctuator::RightBrace)
        })
    {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let mut comments = TokenComments::new(input, tokens, keyword_index, closing_brace);
    let artifact_leading = comments.take_leading(keyword_index);
    let artifact_trailing = comments.take_trailing(keyword_index);
    let component_leading = comments.take_leading(keyword_index + 1);
    let component_trailing = comments.take_trailing(keyword_index + 1);
    let opening_leading = comments.take_leading(opening_brace);
    let opening_trailing = comments.take_trailing(opening_brace);
    let mut closing_leading = comments.take_leading(closing_brace);
    let closing_trailing = comments.take_trailing(closing_brace);

    let entries = index
        .entries(list)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    let mut artifact_entries = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.end.checked_sub(entry.start) != Some(2)
            || entry.start <= list.open_token
            || entry.end > list.close_token
        {
            return Err(PackageDocumentError::InvalidEntry);
        }
        let start = usize::try_from(entry.start).map_err(|_| PackageDocumentError::InvalidEntry)?;
        let end = usize::try_from(entry.end).map_err(|_| PackageDocumentError::InvalidEntry)?;
        let [target, path] = tokens
            .get(start..end)
            .ok_or(PackageDocumentError::InvalidEntry)?
        else {
            return Err(PackageDocumentError::InvalidEntry);
        };
        if target.kind != PackageTokenKind::Word || path.kind != PackageTokenKind::String {
            return Err(PackageDocumentError::InvalidEntry);
        }
        artifact_entries.push(ArtifactEntry {
            target_index: start,
            path_index: start + 1,
            target_leading: comments.take_leading(start),
            target_trailing: comments.take_trailing(start),
            path_leading: comments.take_leading(start + 1),
            path_trailing: comments.take_trailing(start + 1),
        });
    }

    if index
        .separators(list)
        .ok_or(PackageDocumentError::InvalidEntry)?
        .iter()
        .any(|separator| {
            tokens
                .get(*separator as usize)
                .is_none_or(|token| token.kind != PackageTokenKind::Newline)
        })
    {
        return Err(PackageDocumentError::InvalidEntry);
    }
    if comments.has_pending(keyword_index, closing_brace) {
        return Err(PackageDocumentError::UnknownCommentAnchor);
    }

    let mut document = leading_comments_before(input, &artifact_leading, Some(artifact.span.start));
    document.push(Doc::text("artifact"));
    document.push(trailing_comments(input, &artifact_trailing));
    if component_leading.is_empty() {
        document.push(Doc::text(" "));
    } else {
        document.push(Doc::HardLine);
        document.extend(leading_comments_before(
            input,
            &component_leading,
            Some(component.span.start),
        ));
    }
    document.push(Doc::text("component"));
    document.push(trailing_comments(input, &component_trailing));
    if opening_leading.is_empty() {
        document.push(Doc::text(" "));
    } else {
        document.push(Doc::HardLine);
        if opening_leading[0].line_breaks_before() > 1 {
            document.push(Doc::HardLine);
        }
        document.extend(leading_comments_before(
            input,
            &opening_leading,
            Some(tokens[opening_brace].span.start),
        ));
    }
    document.push(Doc::text("{"));

    let has_body =
        !artifact_entries.is_empty() || !opening_trailing.is_empty() || !closing_leading.is_empty();
    if has_body {
        let mut body = vec![trailing_comments(input, &opening_trailing)];
        if !artifact_entries.is_empty() {
            body.push(Doc::HardLine);
            for (position, entry) in artifact_entries.iter().enumerate() {
                if position > 0 {
                    let previous = &artifact_entries[position - 1];
                    let previous_end = tokens[previous.path_index].span.end;
                    let current_start = tokens[entry.target_index].span.start;
                    let preserve_blank_line = entry.target_leading.first().map_or_else(
                        || has_blank_line(input, previous_end, current_start),
                        |comment| comment.line_breaks_before() > 1,
                    );
                    body.push(Doc::HardLine);
                    if preserve_blank_line {
                        body.push(Doc::HardLine);
                    }
                }
                body.extend(leading_comments_before(
                    input,
                    &entry.target_leading,
                    Some(tokens[entry.target_index].span.start),
                ));
                body.push(Doc::text(token_text(
                    input.source(),
                    &tokens[entry.target_index],
                )));
                if trailing_comments_force_line_break(input, &entry.target_trailing)
                    || !entry.path_leading.is_empty()
                {
                    return Err(PackageDocumentError::InvalidEntry);
                }
                body.push(trailing_comments(input, &entry.target_trailing));
                body.push(Doc::text(" "));
                body.push(Doc::text(token_text(
                    input.source(),
                    &tokens[entry.path_index],
                )));
                body.push(trailing_comments(input, &entry.path_trailing));
            }
        }
        if !closing_leading.is_empty() {
            sort_comments(input, &mut closing_leading);
            body.push(Doc::HardLine);
            if !artifact_entries.is_empty()
                && has_blank_line(
                    input,
                    tokens[artifact_entries
                        .last()
                        .expect("non-empty artifact entries")
                        .path_index]
                        .span
                        .end,
                    input.comment_span(closing_leading[0]).start,
                )
            {
                body.push(Doc::HardLine);
            }
            body.extend(leading_comments_before(input, &closing_leading, None));
        }
        document.push(Doc::indent(Doc::concat(body)));
        document.push(Doc::HardLine);
    }
    document.push(Doc::text("}"));
    document.push(trailing_comments(input, &closing_trailing));
    Ok(Doc::concat(document))
}

pub(crate) fn overrides_declaration(
    input: &PackageInput<'_>,
    index: &PackageSyntaxIndex<'_>,
    list: &PackageBraceList,
) -> Result<Doc, PackageDocumentError> {
    if !input.refusals().is_empty() {
        return Err(PackageDocumentError::InputRefused);
    }
    if list.owner != PackageBraceOwner::Overrides
        || list.kind() != crate::package_surface::PackageListKind::LinesOnly
        || !index.contains_list(list)
    {
        return Err(PackageDocumentError::WrongListOwner);
    }
    if !index.matches_input(input) {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let tokens = &input.lexed().tokens;
    let opening_brace =
        usize::try_from(list.open_token).map_err(|_| PackageDocumentError::InvalidEntry)?;
    let closing_brace =
        usize::try_from(list.close_token).map_err(|_| PackageDocumentError::InvalidEntry)?;
    if opening_brace >= closing_brace {
        return Err(PackageDocumentError::InvalidEntry);
    }
    let keyword_index = opening_brace
        .checked_sub(1)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    let keyword = tokens
        .get(keyword_index)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    if keyword.kind != PackageTokenKind::Word
        || token_text(input.source(), keyword) != "overrides"
        || tokens.get(opening_brace).is_none_or(|token| {
            token.kind != PackageTokenKind::Punctuator(PackagePunctuator::LeftBrace)
        })
        || tokens.get(closing_brace).is_none_or(|token| {
            token.kind != PackageTokenKind::Punctuator(PackagePunctuator::RightBrace)
        })
    {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let mut comments = TokenComments::new(input, tokens, keyword_index, closing_brace);
    let keyword_leading = comments.take_leading(keyword_index);
    let keyword_trailing = comments.take_trailing(keyword_index);
    let opening_leading = comments.take_leading(opening_brace);
    let opening_trailing = comments.take_trailing(opening_brace);
    let mut closing_leading = comments.take_leading(closing_brace);
    let closing_trailing = comments.take_trailing(closing_brace);

    let entries = index
        .entries(list)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    let mut override_entries = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry
            .end
            .checked_sub(entry.start)
            .is_none_or(|length| length < 3)
            || entry.start <= list.open_token
            || entry.end > list.close_token
        {
            return Err(PackageDocumentError::InvalidEntry);
        }
        let start = usize::try_from(entry.start).map_err(|_| PackageDocumentError::InvalidEntry)?;
        let end = usize::try_from(entry.end).map_err(|_| PackageDocumentError::InvalidEntry)?;
        let entry_tokens = tokens
            .get(start..end)
            .ok_or(PackageDocumentError::InvalidEntry)?;
        if !valid_override_entry(input.source(), entry_tokens) {
            return Err(PackageDocumentError::InvalidEntry);
        }
        let mut leading = Vec::with_capacity(entry_tokens.len());
        let mut trailing = Vec::with_capacity(entry_tokens.len());
        for token_index in start..end {
            leading.push(comments.take_leading(token_index));
            trailing.push(comments.take_trailing(token_index));
        }
        override_entries.push(OverrideEntry {
            token_indices: start..end,
            leading,
            trailing,
        });
    }

    if index
        .separators(list)
        .ok_or(PackageDocumentError::InvalidEntry)?
        .iter()
        .any(|separator| {
            tokens
                .get(*separator as usize)
                .is_none_or(|token| token.kind != PackageTokenKind::Newline)
        })
    {
        return Err(PackageDocumentError::InvalidEntry);
    }
    if comments.has_pending(keyword_index, closing_brace) {
        return Err(PackageDocumentError::UnknownCommentAnchor);
    }

    let mut document = leading_comments_before(input, &keyword_leading, Some(keyword.span.start));
    document.extend([
        Doc::text("overrides"),
        trailing_comments(input, &keyword_trailing),
    ]);
    if opening_leading.is_empty() {
        document.push(Doc::text(" "));
    } else {
        document.push(Doc::HardLine);
        if opening_leading[0].line_breaks_before() > 1 {
            document.push(Doc::HardLine);
        }
        document.extend(leading_comments_before(
            input,
            &opening_leading,
            Some(tokens[opening_brace].span.start),
        ));
    }
    document.push(Doc::text("{"));

    let has_body =
        !override_entries.is_empty() || !opening_trailing.is_empty() || !closing_leading.is_empty();
    if has_body {
        let mut body = vec![trailing_comments(input, &opening_trailing)];
        if !override_entries.is_empty() {
            body.push(Doc::HardLine);
            for (position, entry) in override_entries.iter().enumerate() {
                if position > 0 {
                    let previous = &override_entries[position - 1];
                    let current_start = tokens[entry.token_indices.start].span.start;
                    let previous_end = tokens[previous.token_indices.end - 1].span.end;
                    let preserve_blank_line = entry.leading[0].first().map_or_else(
                        || has_blank_line(input, previous_end, current_start),
                        |comment| comment.line_breaks_before() > 1,
                    );
                    body.push(Doc::HardLine);
                    if preserve_blank_line {
                        body.push(Doc::HardLine);
                    }
                }
                body.extend(override_entry_document(input, tokens, entry));
            }
        }
        if !closing_leading.is_empty() {
            sort_comments(input, &mut closing_leading);
            body.push(Doc::HardLine);
            if !override_entries.is_empty()
                && has_blank_line(
                    input,
                    tokens[override_entries
                        .last()
                        .expect("non-empty override entries")
                        .token_indices
                        .end
                        - 1]
                    .span
                    .end,
                    input.comment_span(closing_leading[0]).start,
                )
            {
                body.push(Doc::HardLine);
            }
            body.extend(leading_comments_before(input, &closing_leading, None));
        }
        document.push(Doc::indent(Doc::concat(body)));
        document.push(Doc::HardLine);
    }
    document.push(Doc::text("}"));
    document.push(trailing_comments(input, &closing_trailing));
    Ok(Doc::concat(document))
}

pub(crate) fn package_identity_declaration(
    input: &PackageInput<'_>,
    declaration: Range<u32>,
) -> Result<Doc, PackageDocumentError> {
    if !input.refusals().is_empty() {
        return Err(PackageDocumentError::InputRefused);
    }
    if declaration.end.checked_sub(declaration.start) != Some(4) {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let tokens = &input.lexed().tokens;
    let first =
        usize::try_from(declaration.start).map_err(|_| PackageDocumentError::InvalidEntry)?;
    let end = usize::try_from(declaration.end).map_err(|_| PackageDocumentError::InvalidEntry)?;
    let declaration_tokens = tokens
        .get(first..end)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    let [keyword, package_path, at, version] = declaration_tokens else {
        return Err(PackageDocumentError::InvalidEntry);
    };
    if keyword.kind != PackageTokenKind::Word
        || token_text(input.source(), keyword) != "package"
        || package_path.kind != PackageTokenKind::PackagePath
        || at.kind != PackageTokenKind::Punctuator(PackagePunctuator::At)
        || version.kind != PackageTokenKind::Version
    {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let mut comments = TokenComments::new(input, tokens, first, end - 1);
    let mut document = Vec::new();
    let mut leading = comments.take_leading(first);
    sort_comments(input, &mut leading);
    document.extend(leading_comments_before(
        input,
        &leading,
        Some(keyword.span.start),
    ));

    for (offset, token) in declaration_tokens.iter().enumerate() {
        let token_index = first + offset;
        let mut leading = comments.take_leading(token_index);
        let trailing = comments.take_trailing(token_index);
        sort_comments(input, &mut leading);
        if token_index > first {
            if leading.is_empty() {
                document.push(Doc::text(" "));
            } else {
                document.extend(leading_comments_before(
                    input,
                    &leading,
                    Some(token.span.start),
                ));
            }
        }
        document.push(Doc::text(token_text(input.source(), token)));
        document.push(trailing_comments(input, &trailing));
    }

    if comments.has_pending(first, end - 1) {
        return Err(PackageDocumentError::UnknownCommentAnchor);
    }
    Ok(Doc::concat(document))
}

pub(crate) fn compiler_declaration(
    input: &PackageInput<'_>,
    declaration: Range<u32>,
) -> Result<Doc, PackageDocumentError> {
    if !input.refusals().is_empty() {
        return Err(PackageDocumentError::InputRefused);
    }
    if declaration
        .end
        .checked_sub(declaration.start)
        .is_none_or(|count| count < 2)
    {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let tokens = &input.lexed().tokens;
    let first =
        usize::try_from(declaration.start).map_err(|_| PackageDocumentError::InvalidEntry)?;
    let end = usize::try_from(declaration.end).map_err(|_| PackageDocumentError::InvalidEntry)?;
    let declaration_tokens = tokens
        .get(first..end)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    let (keyword, value_tokens) = declaration_tokens
        .split_first()
        .ok_or(PackageDocumentError::InvalidEntry)?;
    if keyword.kind != PackageTokenKind::Word || token_text(input.source(), keyword) != "compiler" {
        return Err(PackageDocumentError::InvalidEntry);
    }
    if !valid_version_requirement_tokens(value_tokens) {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let last = end - 1;
    let mut comments = TokenComments::new(input, tokens, first, last);
    let mut keyword_leading = comments.take_leading(first);
    let keyword_trailing = comments.take_trailing(first);
    sort_comments(input, &mut keyword_leading);
    let mut document = leading_comments_before(input, &keyword_leading, Some(keyword.span.start));
    document.push(Doc::text("compiler"));
    document.push(trailing_comments(input, &keyword_trailing));

    let mut value_document = vec![Doc::text(" ")];
    let mut previous_forced_break = false;
    let mut previous_forced_comment_end = None;
    let mut previous_had_trailing_block_comment = false;
    for (offset, token) in value_tokens.iter().enumerate() {
        let token_index = first + offset + 1;
        let mut leading = comments.take_leading(token_index);
        let trailing = comments.take_trailing(token_index);
        sort_comments(input, &mut leading);

        if !leading.is_empty() {
            let preserve_blank_line = previous_forced_break
                && previous_forced_comment_end
                    .zip(leading.first())
                    .is_some_and(|(previous_end, comment)| {
                        has_blank_line(input, previous_end, input.comment_span(comment).start)
                    });
            if !previous_forced_break || preserve_blank_line {
                value_document.push(Doc::HardLine);
            }
            value_document.extend(leading_comments_before(
                input,
                &leading,
                Some(token.span.start),
            ));
        } else if offset > 0 && !previous_forced_break {
            let previous = &value_tokens[offset - 1];
            let is_comma = token.kind == PackageTokenKind::Punctuator(PackagePunctuator::Comma);
            if (previous_had_trailing_block_comment && !is_comma)
                || (!previous_had_trailing_block_comment
                    && previous.kind == PackageTokenKind::Punctuator(PackagePunctuator::Comma))
            {
                value_document.push(Doc::text(" "));
            }
        }

        value_document.push(Doc::text(token_text(input.source(), token)));
        value_document.push(trailing_comments(input, &trailing));
        previous_forced_break =
            offset + 1 < value_tokens.len() && trailing_comments_force_line_break(input, &trailing);
        if previous_forced_break {
            value_document.push(Doc::HardLine);
            previous_forced_comment_end = trailing
                .last()
                .map(|comment| input.comment_span(comment).end);
        } else {
            previous_forced_comment_end = None;
        }
        previous_had_trailing_block_comment = trailing
            .iter()
            .any(|comment| input.comment_kind(comment) == PackageTriviaKind::BlockComment);
    }

    if comments.has_pending(first, last) {
        return Err(PackageDocumentError::UnknownCommentAnchor);
    }
    document.push(Doc::indent(Doc::concat(value_document)));
    Ok(Doc::concat(document))
}

pub(crate) fn string_declaration(
    input: &PackageInput<'_>,
    declaration: Range<u32>,
) -> Result<Doc, PackageDocumentError> {
    value_declaration(
        input,
        declaration,
        &[
            ("source", PackageTokenKind::String),
            ("wit", PackageTokenKind::String),
            ("description", PackageTokenKind::String),
            ("license", PackageTokenKind::String),
            ("repository", PackageTokenKind::String),
        ],
    )
}

pub(crate) fn allocator_declaration(
    input: &PackageInput<'_>,
    declaration: Range<u32>,
) -> Result<Doc, PackageDocumentError> {
    value_declaration(
        input,
        declaration,
        &[("allocator", PackageTokenKind::PackagePath)],
    )
}

pub(crate) fn export_world_declaration(
    input: &PackageInput<'_>,
    declaration: Range<u32>,
) -> Result<Doc, PackageDocumentError> {
    if !input.refusals().is_empty() {
        return Err(PackageDocumentError::InputRefused);
    }

    let token_count = declaration
        .end
        .checked_sub(declaration.start)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    if token_count < 3 {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let tokens = &input.lexed().tokens;
    let first =
        usize::try_from(declaration.start).map_err(|_| PackageDocumentError::InvalidEntry)?;
    let end = usize::try_from(declaration.end).map_err(|_| PackageDocumentError::InvalidEntry)?;
    if end > tokens.len() {
        return Err(PackageDocumentError::InvalidEntry);
    }
    let export_token = tokens
        .get(first)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    let world_token = tokens
        .get(first + 1)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    if export_token.kind != PackageTokenKind::Word
        || token_text(input.source(), export_token) != "export"
        || world_token.kind != PackageTokenKind::Word
        || token_text(input.source(), world_token) != "world"
    {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let name_tokens = tokens
        .get(first + 2..end)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    match name_tokens {
        [name]
            if matches!(
                name.kind,
                PackageTokenKind::Word | PackageTokenKind::PackagePath
            ) => {}
        [first_name, tail @ ..]
            if first_name.kind == PackageTokenKind::Word
                && tail.len() % 2 == 0
                && tail.chunks_exact(2).all(|pair| {
                    pair[0].kind == PackageTokenKind::Punctuator(PackagePunctuator::Slash)
                        && pair[1].kind == PackageTokenKind::Word
                }) => {}
        _ => return Err(PackageDocumentError::InvalidEntry),
    }

    let last = end - 1;
    let mut comments = TokenComments::new(input, tokens, first, last);
    let mut document = Vec::new();
    let mut leading = comments.take_leading(first);
    sort_comments(input, &mut leading);
    document.extend(leading_comments_before(
        input,
        &leading,
        Some(export_token.span.start),
    ));

    let mut previous_has_trailing_block_comment = false;
    for token_index in first..end {
        let mut leading = comments.take_leading(token_index);
        let trailing = comments.take_trailing(token_index);
        sort_comments(input, &mut leading);
        if token_index > first {
            let previous = &tokens[token_index - 1];
            let current = &tokens[token_index];
            if !leading.is_empty() {
                document.extend(leading_comments_before(
                    input,
                    &leading,
                    Some(current.span.start),
                ));
            } else if previous_has_trailing_block_comment
                || (previous.kind != PackageTokenKind::Punctuator(PackagePunctuator::Slash)
                    && current.kind != PackageTokenKind::Punctuator(PackagePunctuator::Slash))
            {
                document.push(Doc::text(" "));
            }
        }
        document.push(Doc::text(token_text(input.source(), &tokens[token_index])));
        previous_has_trailing_block_comment = trailing
            .iter()
            .any(|comment| input.comment_kind(comment) == PackageTriviaKind::BlockComment);
        document.push(trailing_comments(input, &trailing));
    }

    if comments.has_pending(first, last) {
        return Err(PackageDocumentError::UnknownCommentAnchor);
    }
    Ok(Doc::concat(document))
}

fn value_declaration(
    input: &PackageInput<'_>,
    declaration: Range<u32>,
    accepted_keywords: &[(&str, PackageTokenKind)],
) -> Result<Doc, PackageDocumentError> {
    if !input.refusals().is_empty() {
        return Err(PackageDocumentError::InputRefused);
    }
    if declaration.end.checked_sub(declaration.start) != Some(2) {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let tokens = &input.lexed().tokens;
    let keyword_index = declaration.start as usize;
    let value_index = keyword_index + 1;
    let keyword_token = tokens
        .get(keyword_index)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    let value_token = tokens
        .get(value_index)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    if keyword_token.kind != PackageTokenKind::Word {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let keyword = token_text(input.source(), keyword_token);
    let expected_value_kind = accepted_keywords
        .iter()
        .find_map(|(accepted_keyword, kind)| (*accepted_keyword == keyword).then_some(*kind))
        .ok_or(PackageDocumentError::InvalidEntry)?;
    if value_token.kind != expected_value_kind {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let mut comments = TokenComments::new(input, tokens, keyword_index, value_index);
    let mut keyword_leading = comments.take_leading(keyword_index);
    let keyword_trailing = comments.take_trailing(keyword_index);
    let mut value_leading = comments.take_leading(value_index);
    let value_trailing = comments.take_trailing(value_index);
    if comments.has_pending(keyword_index, value_index) {
        return Err(PackageDocumentError::UnknownCommentAnchor);
    }
    sort_comments(input, &mut keyword_leading);
    sort_comments(input, &mut value_leading);

    let mut document =
        leading_comments_before(input, &keyword_leading, Some(keyword_token.span.start));
    document.extend([
        Doc::text(keyword),
        trailing_comments(input, &keyword_trailing),
    ]);
    document.extend(leading_comments_before(
        input,
        &value_leading,
        Some(value_token.span.start),
    ));
    document.extend([
        Doc::text(" "),
        Doc::text(token_text(input.source(), value_token)),
        trailing_comments(input, &value_trailing),
    ]);
    Ok(Doc::concat(document))
}

fn valid_override_entry(source: &[u8], tokens: &[PackageToken]) -> bool {
    let Some((package, remainder)) = tokens.split_first() else {
        return false;
    };
    if package.kind != PackageTokenKind::PackagePath
        || remainder.first().is_none_or(|token| {
            token.kind != PackageTokenKind::Word || token_text(source, token) != "use"
        })
    {
        return false;
    }

    match remainder.get(1..) {
        Some([artifact])
            if artifact.kind == PackageTokenKind::Word
                && token_text(source, artifact) == "artifact" =>
        {
            true
        }
        Some([kind, path])
            if kind.kind == PackageTokenKind::Word
                && token_text(source, kind) == "path"
                && path.kind == PackageTokenKind::String =>
        {
            true
        }
        Some([kind, url])
            if kind.kind == PackageTokenKind::Word
                && token_text(source, kind) == "git"
                && url.kind == PackageTokenKind::String =>
        {
            true
        }
        Some([kind, url, directory, path])
            if kind.kind == PackageTokenKind::Word
                && token_text(source, kind) == "git"
                && url.kind == PackageTokenKind::String
                && directory.kind == PackageTokenKind::Word
                && token_text(source, directory) == "dir"
                && path.kind == PackageTokenKind::String =>
        {
            true
        }
        _ => false,
    }
}

fn override_entry_document(
    input: &PackageInput<'_>,
    tokens: &[PackageToken],
    entry: &OverrideEntry<'_>,
) -> Vec<Doc> {
    let mut document = Vec::new();
    for offset in 0..entry.token_indices.len() {
        let token_index = entry.token_indices.start + offset;
        let token = &tokens[token_index];
        let leading = &entry.leading[offset];
        let trailing = &entry.trailing[offset];
        if offset == 0 {
            document.extend(leading_comments_before(
                input,
                leading,
                Some(token.span.start),
            ));
            document.push(Doc::text(token_text(input.source(), token)));
            document.push(trailing_comments(input, trailing));
            continue;
        }

        let previous_trailing = &entry.trailing[offset - 1];
        let requires_break =
            !leading.is_empty() || trailing_comments_force_line_break(input, previous_trailing);
        if requires_break {
            let mut continuation = vec![Doc::HardLine];
            if leading
                .first()
                .is_some_and(|comment| comment.line_breaks_before() > 1)
            {
                continuation.push(Doc::HardLine);
            }
            continuation.extend(leading_comments_before(
                input,
                leading,
                Some(token.span.start),
            ));
            continuation.extend([
                Doc::text(token_text(input.source(), token)),
                trailing_comments(input, trailing),
            ]);
            document.push(Doc::indent(Doc::concat(continuation)));
        } else {
            document.extend([
                Doc::text(" "),
                Doc::text(token_text(input.source(), token)),
                trailing_comments(input, trailing),
            ]);
        }
    }
    document
}

fn valid_version_requirement_tokens(tokens: &[PackageToken]) -> bool {
    if tokens.len() == 1 {
        return matches!(
            tokens[0].kind,
            PackageTokenKind::Integer | PackageTokenKind::Version
        );
    }

    let mut cursor = 0;
    loop {
        let Some(operator) = tokens.get(cursor) else {
            return false;
        };
        if !is_version_operator(operator.kind) {
            return false;
        }
        cursor += 1;

        let Some(version) = tokens.get(cursor) else {
            return false;
        };
        if !matches!(
            version.kind,
            PackageTokenKind::Integer | PackageTokenKind::Version
        ) {
            return false;
        }
        cursor += 1;
        if cursor == tokens.len() {
            return true;
        }

        if tokens[cursor].kind != PackageTokenKind::Punctuator(PackagePunctuator::Comma) {
            return false;
        }
        cursor += 1;
        if cursor == tokens.len() {
            return false;
        }
    }
}

const fn is_version_operator(kind: PackageTokenKind) -> bool {
    matches!(
        kind,
        PackageTokenKind::Punctuator(
            PackagePunctuator::Caret
                | PackagePunctuator::Tilde
                | PackagePunctuator::Equal
                | PackagePunctuator::Greater
                | PackagePunctuator::GreaterEqual
                | PackagePunctuator::Less
                | PackagePunctuator::LessEqual
        )
    )
}

fn trailing_comments_force_line_break(
    input: &PackageInput<'_>,
    comments: &[&AttachedComment],
) -> bool {
    comments
        .iter()
        .any(|comment| match input.comment_kind(comment) {
            PackageTriviaKind::LineComment => true,
            PackageTriviaKind::BlockComment => input.comment_text(comment).contains('\n'),
            _ => false,
        })
}

fn comma_list_declaration(
    input: &PackageInput<'_>,
    index: &PackageSyntaxIndex<'_>,
    list: &PackageBraceList,
    owner: PackageBraceOwner,
) -> Result<Doc, PackageDocumentError> {
    if !input.refusals().is_empty() {
        return Err(PackageDocumentError::InputRefused);
    }
    if list.owner != owner {
        return Err(PackageDocumentError::WrongListOwner);
    }
    if !index.contains_list(list) {
        return Err(PackageDocumentError::InvalidEntry);
    }
    if !index.matches_input(input) {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let tokens = &input.lexed().tokens;
    let keyword = match owner {
        PackageBraceOwner::Targets => "targets",
        PackageBraceOwner::Allow => "allow",
        PackageBraceOwner::Deny => "deny",
        PackageBraceOwner::Authors => "authors",
        _ => return Err(PackageDocumentError::WrongListOwner),
    };
    let keyword_index = (list.open_token as usize)
        .checked_sub(1)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    if token_text(input.source(), &tokens[keyword_index]) != keyword {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let close_token = list.close_token as usize;
    let opening_brace = list.open_token as usize;
    let mut comments = TokenComments::new(input, tokens, keyword_index, close_token);
    let mut keyword_leading = comments.take_leading(keyword_index);
    let keyword_trailing = comments.take_trailing(keyword_index);
    let opening_leading = comments.take_leading(opening_brace);
    let opening_trailing = comments.take_trailing(opening_brace);
    let mut closing_leading = comments.take_leading(close_token);
    let closing_trailing = comments.take_trailing(close_token);

    let entries = index
        .entries(list)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    let mut list_entries = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.end != entry.start + 1 {
            return Err(PackageDocumentError::InvalidEntry);
        }
        let token_index = entry.start as usize;
        let token = &tokens[token_index];
        let valid_entry = match owner {
            PackageBraceOwner::Targets => token.kind == PackageTokenKind::Word,
            PackageBraceOwner::Allow | PackageBraceOwner::Deny => matches!(
                token.kind,
                PackageTokenKind::Word | PackageTokenKind::PackagePath
            ),
            PackageBraceOwner::Authors => token.kind == PackageTokenKind::String,
            _ => false,
        };
        if !valid_entry {
            return Err(PackageDocumentError::InvalidEntry);
        }
        list_entries.push(ListEntry {
            token_index,
            leading: comments.take_leading(token_index),
            trailing: comments.take_trailing(token_index),
        });
    }

    let mut comma_trailing = vec![Vec::new(); list_entries.len().saturating_sub(1)];
    for &separator in index
        .separators(list)
        .ok_or(PackageDocumentError::InvalidEntry)?
    {
        let separator_index = separator as usize;
        if tokens[separator_index].kind != PackageTokenKind::Punctuator(PackagePunctuator::Comma) {
            continue;
        }

        let next_entry = list_entries.partition_point(|entry| entry.token_index <= separator_index);
        let previous_entry = next_entry
            .checked_sub(1)
            .ok_or(PackageDocumentError::InvalidEntry)?;
        let mut leading = comments.take_leading(separator_index);
        let mut trailing = comments.take_trailing(separator_index);
        if next_entry < list_entries.len() {
            leading.append(&mut list_entries[next_entry].leading);
            sort_comments(input, &mut leading);
            list_entries[next_entry].leading = leading;
            comma_trailing[previous_entry].append(&mut trailing);
            sort_comments(input, &mut comma_trailing[previous_entry]);
        } else {
            leading.append(&mut closing_leading);
            sort_comments(input, &mut leading);
            closing_leading = leading;
            list_entries[previous_entry].trailing.append(&mut trailing);
            sort_comments(input, &mut list_entries[previous_entry].trailing);
        }
    }

    if comments.has_pending(keyword_index, close_token) {
        return Err(PackageDocumentError::UnknownCommentAnchor);
    }
    sort_comments(input, &mut keyword_leading);

    let mut prefix = leading_comments_before(
        input,
        &keyword_leading,
        Some(tokens[keyword_index].span.start),
    );
    let needs_group = !list_entries.is_empty()
        || !opening_leading.is_empty()
        || !opening_trailing.is_empty()
        || !closing_leading.is_empty();

    if !needs_group {
        prefix.extend([
            Doc::text(keyword),
            trailing_comments(input, &keyword_trailing),
            Doc::text(" {}"),
            trailing_comments(input, &closing_trailing),
        ]);
        return Ok(Doc::concat(prefix));
    }

    let group = GroupId(list.open_token);
    let mut declaration = vec![
        Doc::text(keyword),
        trailing_comments(input, &keyword_trailing),
    ];
    if opening_leading.is_empty() {
        declaration.push(Doc::text(" "));
    } else {
        declaration.push(Doc::HardLine);
        if opening_leading[0].line_breaks_before() > 1 {
            declaration.push(Doc::HardLine);
        }
        declaration.extend(leading_comments_before(
            input,
            &opening_leading,
            Some(tokens[opening_brace].span.start),
        ));
    }
    declaration.push(Doc::text("{"));

    let empty_inline_comment = list_entries.is_empty() && !opening_trailing.is_empty();
    let mut body = vec![trailing_comments(input, &opening_trailing)];
    if !list_entries.is_empty() {
        body.push(Doc::Line);
        for (position, entry) in list_entries.iter().enumerate() {
            if position > 0 {
                let previous = &list_entries[position - 1];
                let current = &list_entries[position];
                let previous_end = tokens[previous.token_index].span.end;
                let current_start = tokens[current.token_index].span.start;
                let blank_line_before_entry = current
                    .leading
                    .first()
                    .is_some_and(|comment| comment.line_breaks_before() > 1);
                let preserve_blank_line = if current.leading.is_empty() {
                    has_blank_line(input, previous_end, current_start)
                } else {
                    blank_line_before_entry
                };
                let mut broken_separator =
                    Vec::with_capacity(comma_trailing[position - 1].len() + 2);
                broken_separator.push(trailing_comments(input, &comma_trailing[position - 1]));
                broken_separator.push(Doc::Line);
                if preserve_blank_line {
                    broken_separator.push(Doc::Line);
                }

                let mut flat_separator = Vec::with_capacity(comma_trailing[position - 1].len() + 2);
                flat_separator.push(Doc::text(","));
                flat_separator.push(trailing_comments(input, &comma_trailing[position - 1]));
                flat_separator.push(Doc::text(" "));
                body.push(Doc::if_broken(
                    group,
                    Doc::concat(broken_separator),
                    Doc::concat(flat_separator),
                ));
            }
            body.extend(leading_comments_before(
                input,
                &entry.leading,
                Some(tokens[entry.token_index].span.start),
            ));
            body.push(Doc::text(token_text(
                input.source(),
                &tokens[entry.token_index],
            )));
            body.push(trailing_comments(input, &entry.trailing));
        }
    }

    if !closing_leading.is_empty() {
        sort_comments(input, &mut closing_leading);
        let gap_start = list_entries
            .last()
            .map(|entry| tokens[entry.token_index].span.end)
            .unwrap_or(tokens[opening_brace].span.end);
        body.push(Doc::HardLine);
        let preserve_blank_line = !list_entries.is_empty()
            && has_blank_line(
                input,
                gap_start,
                input.comment_span(closing_leading[0]).start,
            );
        if preserve_blank_line {
            body.push(Doc::HardLine);
        }
        body.extend(leading_comments_before(input, &closing_leading, None));
    }

    let group_doc = Doc::group(
        group,
        Doc::concat([
            Doc::concat(declaration),
            Doc::indent(Doc::concat(body)),
            if empty_inline_comment {
                Doc::HardLine
            } else if list_entries.last().is_some_and(|entry| {
                entry.trailing.last().is_some_and(|comment| {
                    input.comment_kind(comment) == PackageTriviaKind::BlockComment
                        && !normalized_comment_text(input, comment).contains('\n')
                })
            }) {
                Doc::if_broken(group, Doc::Line, Doc::text(""))
            } else {
                Doc::Line
            },
            Doc::text("}"),
        ]),
    );
    prefix.push(group_doc);
    prefix.push(trailing_comments(input, &closing_trailing));
    Ok(Doc::concat(prefix))
}

struct ListEntry<'a> {
    token_index: usize,
    leading: Vec<&'a AttachedComment>,
    trailing: Vec<&'a AttachedComment>,
}

struct ArtifactEntry<'a> {
    target_index: usize,
    path_index: usize,
    target_leading: Vec<&'a AttachedComment>,
    target_trailing: Vec<&'a AttachedComment>,
    path_leading: Vec<&'a AttachedComment>,
    path_trailing: Vec<&'a AttachedComment>,
}

struct OverrideEntry<'a> {
    token_indices: Range<usize>,
    leading: Vec<Vec<&'a AttachedComment>>,
    trailing: Vec<Vec<&'a AttachedComment>>,
}

#[derive(Default)]
struct TokenComments<'a> {
    leading: HashMap<usize, Vec<&'a AttachedComment>>,
    trailing: HashMap<usize, Vec<&'a AttachedComment>>,
}

impl<'a> TokenComments<'a> {
    fn new(
        input: &'a PackageInput<'_>,
        tokens: &[PackageToken],
        first_token: usize,
        last_token: usize,
    ) -> Self {
        let mut comments = Self::default();
        let mut first_token_on_line = first_token;
        while first_token_on_line > 0
            && tokens[first_token_on_line - 1].kind == PackageTokenKind::Newline
        {
            first_token_on_line -= 1;
        }
        let start_offset = first_token_on_line
            .checked_sub(1)
            .map_or(0, |index| tokens[index].span.end);
        let end_offset = tokens
            .get(last_token + 1)
            .map_or(input.source().len(), |token| token.span.start);

        let source_comments = input.comments();
        let first_comment = source_comments
            .partition_point(|comment| input.comment_span(comment).end <= start_offset);
        for comment in &source_comments[first_comment..] {
            if input.comment_span(comment).start >= end_offset {
                break;
            }
            let (token_index, is_leading) = match comment.attachment() {
                CommentAttachment::Leading(index) => (index, true),
                CommentAttachment::Trailing(index) => (index, false),
                CommentAttachment::EndOfFile => continue,
            };
            if !(first_token..=last_token).contains(&token_index) {
                continue;
            }
            let bucket = if is_leading {
                &mut comments.leading
            } else {
                &mut comments.trailing
            };
            bucket.entry(token_index).or_default().push(comment);
        }
        comments
    }

    fn take_leading(&mut self, token_index: usize) -> Vec<&'a AttachedComment> {
        self.leading.remove(&token_index).unwrap_or_default()
    }

    fn take_trailing(&mut self, token_index: usize) -> Vec<&'a AttachedComment> {
        self.trailing.remove(&token_index).unwrap_or_default()
    }

    fn has_pending(&self, first_token: usize, last_token: usize) -> bool {
        self.leading
            .keys()
            .chain(self.trailing.keys())
            .any(|&token_index| (first_token..=last_token).contains(&token_index))
    }
}

fn sort_comments(input: &PackageInput<'_>, comments: &mut [&AttachedComment]) {
    comments.sort_by_key(|comment| input.comment_span(comment).start);
}

fn leading_comments_before(
    input: &PackageInput<'_>,
    comments: &[&AttachedComment],
    next_token_start: Option<usize>,
) -> Vec<Doc> {
    let mut docs = Vec::new();
    for (position, comment) in comments.iter().enumerate() {
        docs.push(comment_text_doc(input, comment));
        let next_start = comments
            .get(position + 1)
            .map(|next| input.comment_span(next).start)
            .or(next_token_start);
        if let Some(next_start) = next_start {
            docs.push(Doc::HardLine);
            if has_blank_line(input, input.comment_span(comment).end, next_start) {
                docs.push(Doc::HardLine);
            }
        }
    }
    docs
}

fn trailing_comments(input: &PackageInput<'_>, comments: &[&AttachedComment]) -> Doc {
    let mut docs = Vec::new();
    for comment in comments {
        let text = normalized_comment_text(input, comment);
        match input.comment_kind(comment) {
            PackageTriviaKind::LineComment => {
                docs.push(Doc::line_suffix(Doc::text(format!(" {text}"))));
            }
            PackageTriviaKind::BlockComment => {
                docs.push(Doc::text(" "));
                docs.push(if text.contains('\n') {
                    Doc::verbatim(text)
                } else {
                    Doc::text(text)
                });
            }
            _ => unreachable!("only comments are attached to significant tokens"),
        }
    }
    Doc::concat(docs)
}

fn comment_text_doc(input: &PackageInput<'_>, comment: &AttachedComment) -> Doc {
    let text = normalized_comment_text(input, comment);
    if text.contains('\n') {
        Doc::verbatim(text)
    } else {
        Doc::text(text)
    }
}

fn normalized_comment_text(input: &PackageInput<'_>, comment: &AttachedComment) -> String {
    input
        .comment_text(comment)
        .replace("\r\n", "\n")
        .split('\n')
        .map(str::trim_end)
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_blank_line(input: &PackageInput<'_>, start: usize, end: usize) -> bool {
    if start >= end {
        return false;
    }

    let mut cursor = start;
    let comments = input.comments();
    let first_comment =
        comments.partition_point(|comment| input.comment_span(comment).end <= start);
    for comment in &comments[first_comment..] {
        let span = input.comment_span(comment);
        if span.start >= end {
            break;
        }
        if contains_blank_line(&input.source()[cursor..span.start]) {
            return true;
        }
        cursor = span.end;
    }
    contains_blank_line(&input.source()[cursor..end])
}

fn contains_blank_line(source: &[u8]) -> bool {
    let mut previous_newline = false;
    let mut horizontal_since_newline = false;
    for &byte in source {
        match byte {
            b'\n' => {
                if previous_newline && horizontal_since_newline {
                    return true;
                }
                previous_newline = true;
                horizontal_since_newline = true;
            }
            b'\r' | b' ' | b'\t' if previous_newline => {}
            b'\r' | b' ' | b'\t' => {}
            _ => {
                previous_newline = false;
                horizontal_since_newline = false;
            }
        }
    }
    false
}

fn token_text<'a>(source: &'a [u8], token: &'a PackageToken) -> &'a str {
    std::str::from_utf8(&source[token.span.clone()])
        .expect("package token spans point to valid UTF-8")
}

#[cfg(test)]
mod tests {
    use crate::package_input::{PackageInput, PackageRefusal};
    use crate::package_surface::{PackageBraceOwner, PackageSyntaxIndex};
    use crate::{GroupId, print, print_with_forced_groups};

    use super::{
        PackageDocumentError, allocator_declaration, artifact_components_declaration,
        authors_declaration, capability_declaration, compiler_declaration,
        export_world_declaration, overrides_declaration, package_identity_declaration,
        string_declaration, targets_declaration, token_text,
    };

    fn render(source: &[u8], force_break: bool) -> String {
        render_owner(source, PackageBraceOwner::Targets, force_break)
    }

    fn render_owner(source: &[u8], owner: PackageBraceOwner, force_break: bool) -> String {
        let input = PackageInput::new(source);
        assert!(input.refusals().is_empty(), "{:?}", input.refusals());
        let index = PackageSyntaxIndex::new(&input).expect("valid package declaration structure");
        let list = index
            .brace_lists()
            .iter()
            .find(|list| list.owner == owner)
            .expect("expected brace list");
        let doc = match owner {
            PackageBraceOwner::Targets => targets_declaration(&input, &index, list),
            PackageBraceOwner::Allow | PackageBraceOwner::Deny => {
                capability_declaration(&input, &index, list)
            }
            PackageBraceOwner::Authors => authors_declaration(&input, &index, list),
            PackageBraceOwner::ArtifactComponents => {
                artifact_components_declaration(&input, &index, list)
            }
            PackageBraceOwner::Overrides => overrides_declaration(&input, &index, list),
            _ => panic!("unsupported simple-list owner in test helper"),
        }
        .expect("package list document");
        if force_break {
            print_with_forced_groups(&doc, &[GroupId(list.open_token)]).expect("print document")
        } else {
            print(&doc).expect("print document")
        }
    }

    fn render_string(source: &[u8], keyword: &str) -> String {
        let input = PackageInput::new(source);
        assert!(input.refusals().is_empty(), "{:?}", input.refusals());
        let index = PackageSyntaxIndex::new(&input).expect("valid package declaration structure");
        let declaration = find_declaration(&input, &index, keyword);
        let doc = string_declaration(&input, declaration.clone()).expect("string document");
        print(&doc).expect("print document")
    }

    fn render_allocator(source: &[u8]) -> String {
        let input = PackageInput::new(source);
        assert!(input.refusals().is_empty(), "{:?}", input.refusals());
        let index = PackageSyntaxIndex::new(&input).expect("valid package declaration structure");
        let declaration = find_declaration(&input, &index, "allocator");
        let doc = allocator_declaration(&input, declaration).expect("allocator document");
        print(&doc).expect("print document")
    }

    fn render_export_world(source: &[u8]) -> String {
        let input = PackageInput::new(source);
        assert!(input.refusals().is_empty(), "{:?}", input.refusals());
        let index = PackageSyntaxIndex::new(&input).expect("valid package declaration structure");
        let declaration = find_declaration(&input, &index, "export");
        let doc = export_world_declaration(&input, declaration).expect("export world document");
        print(&doc).expect("print document")
    }

    fn render_package_identity(source: &[u8]) -> String {
        let input = PackageInput::new(source);
        assert!(input.refusals().is_empty(), "{:?}", input.refusals());
        let index = PackageSyntaxIndex::new(&input).expect("valid package declaration structure");
        let declaration = find_declaration(&input, &index, "package");
        let doc =
            package_identity_declaration(&input, declaration).expect("package identity document");
        print(&doc).expect("print document")
    }

    fn render_compiler(source: &[u8]) -> String {
        let input = PackageInput::new(source);
        assert!(input.refusals().is_empty(), "{:?}", input.refusals());
        let index = PackageSyntaxIndex::new(&input).expect("valid package declaration structure");
        let declaration = find_declaration(&input, &index, "compiler");
        let doc = compiler_declaration(&input, declaration).expect("compiler document");
        print(&doc).expect("print document")
    }

    fn find_declaration(
        input: &PackageInput<'_>,
        index: &PackageSyntaxIndex<'_>,
        keyword: &str,
    ) -> std::ops::Range<u32> {
        index
            .declarations()
            .iter()
            .find(|declaration| {
                let token = &input.lexed().tokens[declaration.start as usize];
                token_text(input.source(), token) == keyword
            })
            .cloned()
            .expect("expected package declaration")
    }

    #[test]
    fn formats_targets_as_a_single_flat_line_when_it_fits() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets {\n wasm32-core,\n wasm32-wasi,\n}\n",
                false
            ),
            "targets { wasm32-core, wasm32-wasi }\n"
        );
    }

    #[test]
    fn formats_allow_and_deny_capability_paths_in_flat_lists() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nallow{alloc,wasi:http/outgoing-handler,alloc}\n",
                PackageBraceOwner::Allow,
                false
            ),
            "allow { alloc, wasi:http/outgoing-handler, alloc }\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\ndeny { wasi:filesystem }\n",
                PackageBraceOwner::Deny,
                false
            ),
            "deny { wasi:filesystem }\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nallow { unknown_capability }\n",
                PackageBraceOwner::Allow,
                false
            ),
            "allow { unknown_capability }\n"
        );
    }

    #[test]
    fn formats_empty_capability_lists_without_inner_spaces() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nallow { }\n",
                PackageBraceOwner::Allow,
                false
            ),
            "allow {}\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\ndeny {}\n",
                PackageBraceOwner::Deny,
                false
            ),
            "deny {}\n"
        );
    }

    #[test]
    fn breaks_capability_lists_and_rehomes_removed_comma_comments() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nallow { alloc, // retain this\n wasi:http/outgoing-handler }\n",
                PackageBraceOwner::Allow,
                false
            ),
            "allow {\n    alloc // retain this\n    wasi:http/outgoing-handler\n}\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\ndeny { wasi:filesystem, alloc }\n",
                PackageBraceOwner::Deny,
                true
            ),
            "deny {\n    wasi:filesystem\n    alloc\n}\n"
        );
    }

    #[test]
    fn formats_authors_as_source_spelling_string_lists() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nauthors{\"Ada\\tExample\",\"Grace Hopper\"}\n",
                PackageBraceOwner::Authors,
                false
            ),
            "authors { \"Ada\\tExample\", \"Grace Hopper\" }\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nauthors {}\n",
                PackageBraceOwner::Authors,
                false
            ),
            "authors {}\n"
        );
    }

    #[test]
    fn breaks_author_lists_with_comments_without_rewriting_string_contents() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nauthors { \"Ada\", // retain this\n \"Grace\" }\n",
                PackageBraceOwner::Authors,
                false
            ),
            "authors {\n    \"Ada\" // retain this\n    \"Grace\"\n}\n"
        );
    }

    #[test]
    fn always_breaks_component_artifact_entries_and_formats_empty_lists() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nartifact component { wasm32-wasi \"dist/app.wasm\" }\n",
                PackageBraceOwner::ArtifactComponents,
                false
            ),
            "artifact component {\n    wasm32-wasi \"dist/app.wasm\"\n}\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nartifact component {\n\
                    wasm32-wasi \"dist/wasm32.wasm\"\n\
                    wasm64-wasi \"dist/wasm64.wasm\"\n\
                }\n",
                PackageBraceOwner::ArtifactComponents,
                false
            ),
            "artifact component {\n    wasm32-wasi \"dist/wasm32.wasm\"\n    wasm64-wasi \"dist/wasm64.wasm\"\n}\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nartifact component {}\n",
                PackageBraceOwner::ArtifactComponents,
                false
            ),
            "artifact component {}\n"
        );
    }

    #[test]
    fn preserves_artifact_entry_comments_and_blank_lines() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\n\
                  artifact component { /* opening */\n\
                      wasm32-wasi /* target */ \"../outside.wasm\" // path\n\
                      // closing\n\
                  } // declaration\n",
                PackageBraceOwner::ArtifactComponents,
                false
            ),
            "artifact component { /* opening */\n    wasm32-wasi /* target */ \"../outside.wasm\" // path\n    // closing\n} // declaration\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\nartifact component {\n\
                    wasm32-wasi \"first.wasm\"\n\
                    \n\
                    wasm64-wasi \"second.wasm\"\n\
                }\n",
                PackageBraceOwner::ArtifactComponents,
                false
            ),
            "artifact component {\n    wasm32-wasi \"first.wasm\"\n\n    wasm64-wasi \"second.wasm\"\n}\n"
        );
    }

    #[test]
    fn preserves_semantically_invalid_artifact_targets_and_paths() {
        let source =
            b"package acme:app @ 1.0.0\nartifact component { imaginary-target \"../outside.wasm\" }\n";
        let input = PackageInput::new(source);
        assert!(input.refusals().is_empty(), "{:?}", input.refusals());
        assert!(!input.parsed().errors().is_empty());
        let index = PackageSyntaxIndex::new(&input).expect("valid package token structure");
        let list = index
            .brace_lists()
            .iter()
            .find(|list| list.owner == PackageBraceOwner::ArtifactComponents)
            .expect("artifact list");
        let doc = artifact_components_declaration(&input, &index, list).expect("artifact document");
        assert_eq!(
            print(&doc).expect("print document"),
            "artifact component {\n    imaginary-target \"../outside.wasm\"\n}\n"
        );
    }

    #[test]
    fn rejects_artifact_lists_from_another_syntax_index() {
        let multi_input = PackageInput::new(
            b"package acme:app @ 1.0.0\nartifact component {\n\
                wasm32-wasi \"first.wasm\"\n\
                wasm64-wasi \"second.wasm\"\n\
            }\n",
        );
        let multi_index =
            PackageSyntaxIndex::new(&multi_input).expect("valid multi-entry artifact list");
        let foreign_list = multi_index
            .brace_lists()
            .first()
            .expect("artifact list is indexed");

        let single_input = PackageInput::new(
            b"package acme:app @ 1.0.0\nartifact component { wasm32-wasi \"only.wasm\" }\n",
        );
        let single_index =
            PackageSyntaxIndex::new(&single_input).expect("valid single-entry artifact list");

        assert_eq!(
            artifact_components_declaration(&single_input, &single_index, foreign_list),
            Err(PackageDocumentError::WrongListOwner)
        );
        assert_eq!(
            artifact_components_declaration(&single_input, &multi_index, foreign_list),
            Err(PackageDocumentError::WrongListOwner)
        );
    }

    #[test]
    fn rejects_mismatched_input_and_index_for_comma_lists() {
        let multi_input =
            PackageInput::new(b"package acme:app @ 1.0.0\ntargets { wasm32-core, wasm32-wasi }\n");
        let multi_index =
            PackageSyntaxIndex::new(&multi_input).expect("valid multi-entry targets list");
        let list = multi_index
            .brace_lists()
            .first()
            .expect("targets list is indexed");
        let single_input =
            PackageInput::new(b"package acme:app @ 1.0.0\ntargets { wasm32-core }\n");

        assert_eq!(
            targets_declaration(&single_input, &multi_index, list),
            Err(PackageDocumentError::InvalidEntry)
        );
    }

    #[test]
    fn always_breaks_override_entries_and_formats_empty_lists() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\n\
                  overrides { acme:image use artifact\n\
                      acme:local use path \"../local\"\n\
                      acme:fork use git \"https://example.com/fork\"\n\
                      acme:subdir use git \"https://example.com/mono\" dir \"image\"\n\
                  }\n",
                PackageBraceOwner::Overrides,
                false
            ),
            "overrides {\n    acme:image use artifact\n    acme:local use path \"../local\"\n    acme:fork use git \"https://example.com/fork\"\n    acme:subdir use git \"https://example.com/mono\" dir \"image\"\n}\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\noverrides {}\n",
                PackageBraceOwner::Overrides,
                false
            ),
            "overrides {}\n"
        );
    }

    #[test]
    fn preserves_override_comments_and_safe_dir_continuations() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\n\
                  overrides { acme:image/* package */ use/* source */artifact // default\n\
                      acme:fork use git \"https://example.com/fork\" // repository\n\
                      dir \"image\"\n\
                  }\n",
                PackageBraceOwner::Overrides,
                false
            ),
            "overrides {\n    acme:image /* package */ use /* source */ artifact // default\n    acme:fork use git \"https://example.com/fork\" // repository\n        dir \"image\"\n}\n"
        );
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\n\
                  overrides {\n\
                      acme:fork use git \"https://example.com/fork\"\n\
                      // selects a repository subdirectory\n\
                      dir \"image\"\n\
                  }\n",
                PackageBraceOwner::Overrides,
                false
            ),
            "overrides {\n    acme:fork use git \"https://example.com/fork\"\n        // selects a repository subdirectory\n        dir \"image\"\n}\n"
        );
    }

    #[test]
    fn preserves_blank_lines_between_override_entries_and_invalid_paths() {
        assert_eq!(
            render_owner(
                b"package acme:app @ 1.0.0\n\
                  overrides {\n\
                      acme:local use path \"../../outside\"\n\
                      \n\
                      \n\
                      acme:image use artifact\n\
                  }\n",
                PackageBraceOwner::Overrides,
                false
            ),
            "overrides {\n    acme:local use path \"../../outside\"\n\n    acme:image use artifact\n}\n"
        );
    }

    #[test]
    fn formats_string_declarations_with_canonical_spacing_and_source_escapes() {
        assert_eq!(
            render_string(
                b"package acme:app @ 1.0.0\ndescription\"A\\tservice\"\n",
                "description"
            ),
            "description \"A\\tservice\"\n"
        );
        assert_eq!(
            render_string(
                b"package acme:app @ 1.0.0\nlicense \"MIT OR Apache-2.0\"\n",
                "license"
            ),
            "license \"MIT OR Apache-2.0\"\n"
        );
        assert_eq!(
            render_string(
                b"package acme:app @ 1.0.0\nrepository \"https://example.com/project\"\n",
                "repository"
            ),
            "repository \"https://example.com/project\"\n"
        );
        assert_eq!(
            render_string(b"package acme:app @ 1.0.0\nsource \"src\"\n", "source"),
            "source \"src\"\n"
        );
        assert_eq!(
            render_string(b"package acme:app @ 1.0.0\nwit \"wit\"\n", "wit"),
            "wit \"wit\"\n"
        );
    }

    #[test]
    fn retains_comments_around_string_declarations() {
        assert_eq!(
            render_string(
                b"package acme:app @ 1.0.0\ndescription/* field note */\"A service\" // docs\n",
                "description"
            ),
            "description /* field note */ \"A service\" // docs\n"
        );
    }

    #[test]
    fn formats_allocator_as_a_single_line_package_path_declaration() {
        assert_eq!(
            render_allocator(b"package acme:app @ 1.0.0\nallocator acme:bump-alloc\n"),
            "allocator acme:bump-alloc\n"
        );
        assert_eq!(
            render_allocator(b"package acme:app @ 1.0.0\nallocator acme:unknown-alloc\n"),
            "allocator acme:unknown-alloc\n"
        );
    }

    #[test]
    fn retains_comments_around_allocator_declarations() {
        assert_eq!(
            render_allocator(
                b"package acme:app @ 1.0.0\nallocator/* selected allocator */acme:bump-alloc // build choice\n"
            ),
            "allocator /* selected allocator */ acme:bump-alloc // build choice\n"
        );
    }

    #[test]
    fn formats_export_world_names_and_package_paths() {
        assert_eq!(
            render_export_world(b"package acme:app @ 1.0.0\nexport world api\n"),
            "export world api\n"
        );
        assert_eq!(
            render_export_world(b"package acme:app @ 1.0.0\nexport  world   api / server\n"),
            "export world api/server\n"
        );
        assert_eq!(
            render_export_world(b"package acme:app @ 1.0.0\nexport world acme:server\n"),
            "export world acme:server\n"
        );
    }

    #[test]
    fn retains_comments_around_export_world_declarations() {
        assert_eq!(
            render_export_world(
                b"package acme:app @ 1.0.0\nexport/* selected */world api/server // world\n"
            ),
            "export /* selected */ world api/server // world\n"
        );
    }

    #[test]
    fn formats_package_identity_with_canonical_at_spacing() {
        assert_eq!(
            render_package_identity(b"package acme:app@1.0.0\n"),
            "package acme:app @ 1.0.0\n"
        );
        assert_eq!(
            render_package_identity(b"package acme:invalid/path @ 1.0.0\n"),
            "package acme:invalid/path @ 1.0.0\n"
        );
    }

    #[test]
    fn retains_comments_around_package_identity_tokens() {
        assert_eq!(
            render_package_identity(
                b"package/* identity */acme:app/* version */@/* release */1.0.0\n"
            ),
            "package /* identity */ acme:app /* version */ @ /* release */ 1.0.0\n"
        );
    }

    #[test]
    fn formats_compiler_version_requirements_with_canonical_spacing() {
        assert_eq!(
            render_compiler(b"package acme:app @ 1.0.0\ncompiler^0.1\n"),
            "compiler ^0.1\n"
        );
        assert_eq!(
            render_compiler(b"package acme:app @ 1.0.0\ncompiler ^ 1.4 , < 2.0\n"),
            "compiler ^1.4, <2.0\n"
        );
    }

    #[test]
    fn preserves_semantically_invalid_compiler_requirement_tokens() {
        assert_eq!(
            render_compiler(b"package acme:app @ 1.0.0\ncompiler > 1.2\n"),
            "compiler >1.2\n"
        );
        assert_eq!(
            render_compiler(b"package acme:app @ 1.0.0\ncompiler 0.1\n"),
            "compiler 0.1\n"
        );
    }

    #[test]
    fn retains_compiler_requirement_comments_and_breaks_after_line_comments() {
        assert_eq!(
            render_compiler(b"package acme:app @ 1.0.0\ncompiler ^/* operator */ 0.1\n"),
            "compiler ^ /* operator */ 0.1\n"
        );
        assert_eq!(
            render_compiler(b"package acme:app @ 1.0.0\ncompiler ^1.4, // next comparator\n <2\n"),
            "compiler ^1.4, // next comparator\n    <2\n"
        );
    }

    #[test]
    fn preserves_blank_line_gaps_between_compiler_requirement_comments() {
        assert_eq!(
            render_compiler(
                b"package acme:app @ 1.0.0\ncompiler ^1.4, // trailing\n// own-line\n <2\n"
            ),
            "compiler ^1.4, // trailing\n    // own-line\n    <2\n"
        );
        assert_eq!(
            render_compiler(
                b"package acme:app @ 1.0.0\ncompiler ^1.4, // trailing\n\n// own-line\n <2\n"
            ),
            "compiler ^1.4, // trailing\n\n    // own-line\n    <2\n"
        );
    }

    #[test]
    fn separates_export_world_slashes_from_adjacent_block_comments() {
        assert_eq!(
            render_export_world(
                b"package acme:app @ 1.0.0\nexport world api /* path segment */ / server\n"
            ),
            "export world api /* path segment */ /server\n"
        );
        assert_eq!(
            render_export_world(
                b"package acme:app @ 1.0.0\nexport world api / /* path segment */ server\n"
            ),
            "export world api/ /* path segment */ server\n"
        );
    }

    #[test]
    fn rejects_export_world_declaration_ranges_past_the_token_stream() {
        let input = PackageInput::new(b"package acme:app @ 1.0.0\nexport world api\n");
        let index = PackageSyntaxIndex::new(&input).expect("valid package declaration structure");
        let declaration = find_declaration(&input, &index, "export");
        let past_end = u32::try_from(input.lexed().tokens.len())
            .expect("small token stream")
            .checked_add(1)
            .expect("token stream index fits in u32");
        assert_eq!(
            export_world_declaration(&input, declaration.start..past_end),
            Err(PackageDocumentError::InvalidEntry)
        );
    }

    #[test]
    fn formats_broken_targets_without_commas_and_with_four_space_indentation() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets { wasm32-core, wasm32-wasi }\n",
                true
            ),
            "targets {\n    wasm32-core\n    wasm32-wasi\n}\n"
        );
    }

    #[test]
    fn preserves_one_blank_line_between_broken_targets() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets {\n wasm32-core,\n\n wasm32-wasi\n}\n",
                true
            ),
            "targets {\n    wasm32-core\n\n    wasm32-wasi\n}\n"
        );
    }

    #[test]
    fn keeps_a_target_list_at_the_width_limit_flat_and_breaks_one_column_over() {
        let exact_width_target = "x".repeat(88);
        let exact_width_source =
            format!("package acme:app @ 1.0.0\ntargets {{ {exact_width_target} }}\n");
        assert_eq!(
            render(exact_width_source.as_bytes(), false),
            format!("targets {{ {exact_width_target} }}\n")
        );

        let over_width_target = "x".repeat(89);
        let over_width_source =
            format!("package acme:app @ 1.0.0\ntargets {{ {over_width_target} }}\n");
        assert_eq!(
            render(over_width_source.as_bytes(), false),
            format!("targets {{\n    {over_width_target}\n}}\n")
        );
    }

    #[test]
    fn formats_an_empty_targets_list_without_inner_spaces() {
        assert_eq!(
            render(b"package acme:app @ 1.0.0\ntargets {\n}\n", false),
            "targets {}\n"
        );
    }

    #[test]
    fn preserves_target_order_duplicates_and_unknown_spellings() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets { wasm32-wasi, imaginary, wasm32-wasi }\n",
                false
            ),
            "targets { wasm32-wasi, imaginary, wasm32-wasi }\n"
        );
    }

    #[test]
    fn rehomes_a_trailing_line_comment_from_a_removed_comma() {
        let source =
            b"package acme:app @ 1.0.0\ntargets {\n    wasm32-core, // keep this\n    wasm32-wasi\n}\n";
        assert_eq!(
            render(source, false),
            "targets {\n    wasm32-core // keep this\n    wasm32-wasi\n}\n"
        );
    }

    #[test]
    fn preserves_blank_line_placement_around_leading_comments() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets {\n\
                    wasm32-core,\n\
                    // attached to the next target\n\
                    \n\
                    wasm32-wasi\n\
                }\n",
                false
            ),
            "targets {\n    wasm32-core\n    // attached to the next target\n\n    wasm32-wasi\n}\n"
        );
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets {\n\
                    wasm32-core,\n\
                    \n\
                    // separated from the previous target\n\
                    wasm32-wasi\n\
                }\n",
                false
            ),
            "targets {\n    wasm32-core\n\n    // separated from the previous target\n    wasm32-wasi\n}\n"
        );
    }

    #[test]
    fn prints_leading_and_closing_comments_on_their_own_indented_lines() {
        let source = b"package acme:app @ 1.0.0\ntargets {\n\
            // core target\n\
            wasm32-core\n\
            // supported targets end here\n\
        }\n";
        assert_eq!(
            render(source, false),
            "targets {\n    // core target\n    wasm32-core\n    // supported targets end here\n}\n"
        );
    }

    #[test]
    fn removes_blank_lines_before_the_closing_brace() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets {\n\
                    wasm32-core\n\
                    // final note\n\
                    \n\
                }\n",
                false
            ),
            "targets {\n    wasm32-core\n    // final note\n}\n"
        );
    }

    #[test]
    fn removes_blank_lines_after_an_opening_brace_before_only_comments() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets {\n\
                    \n\
                    // only target note\n\
                    \n\
                }\n",
                false
            ),
            "targets {\n    // only target note\n}\n"
        );
    }

    #[test]
    fn keeps_inline_block_comments_in_flat_and_broken_layouts() {
        let source =
            b"package acme:app @ 1.0.0\ntargets { wasm32-core, /* primary */ wasm32-wasi }\n";
        assert_eq!(
            render(source, false),
            "targets { wasm32-core, /* primary */ wasm32-wasi }\n"
        );
        assert_eq!(
            render(source, true),
            "targets {\n    wasm32-core /* primary */\n    wasm32-wasi\n}\n"
        );
    }

    #[test]
    fn omits_a_space_before_a_final_inline_block_comment_closing_brace() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets { wasm32-core /* tail */ }\n",
                false
            ),
            "targets { wasm32-core /* tail */}\n"
        );
    }

    #[test]
    fn breaks_an_empty_list_that_contains_only_an_inline_block_comment() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets { /* only comment */ }\n",
                false
            ),
            "targets { /* only comment */\n}\n"
        );
    }

    #[test]
    fn keeps_comments_attached_to_the_open_and_close_braces() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets { // supported targets\n wasm32-core }\n",
                false
            ),
            "targets { // supported targets\n    wasm32-core\n}\n"
        );
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets { wasm32-core } // declaration note\n",
                false
            ),
            "targets { wasm32-core } // declaration note\n"
        );
    }

    #[test]
    fn rehomes_a_trailing_comment_from_a_removed_final_comma() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\ntargets { wasm32-core, // final target\n}\n",
                false
            ),
            "targets {\n    wasm32-core // final target\n}\n"
        );
    }

    #[test]
    fn preserves_multiline_comma_comments_verbatim_when_a_list_breaks() {
        let source = b"package acme:app @ 1.0.0\ntargets { wasm32-core, /* keep\n  this line */ wasm32-wasi }\n";
        assert_eq!(
            render(source, false),
            "targets {\n    wasm32-core /* keep\n  this line */\n    wasm32-wasi\n}\n"
        );
    }

    #[test]
    fn normalizes_crlf_and_trims_trailing_whitespace_inside_comments() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\r\ntargets { wasm32-core /* first  \r\n second \t \r\n third */ }\r\n",
                false
            ),
            "targets {\n    wasm32-core /* first\n second\n third */\n}\n"
        );
    }

    #[test]
    fn keeps_a_comment_attached_to_the_declaration_keyword() {
        assert_eq!(
            render(
                b"package acme:app @ 1.0.0\n// enabled for these targets\ntargets { wasm32-core }\n",
                false
            ),
            "// enabled for these targets\ntargets { wasm32-core }\n"
        );
    }

    #[test]
    fn refuses_malformed_package_syntax_before_rendering_a_declaration() {
        let input =
            PackageInput::new(b"package acme:app @ 1.0.0\ntargets { wasm32-core }\nunknown\n");
        assert!(!input.refusals().is_empty());
        let index = PackageSyntaxIndex::new(&input).expect("balanced package token structure");
        let list = index
            .brace_lists()
            .iter()
            .find(|list| list.owner == PackageBraceOwner::Targets)
            .expect("targets list");
        assert_eq!(
            targets_declaration(&input, &index, list),
            Err(PackageDocumentError::InputRefused)
        );
    }

    #[test]
    fn refuses_a_multiline_comment_that_inserts_a_newline_before_the_opening_brace() {
        let input = PackageInput::new(
            b"package acme:app @ 1.0.0\ntargets /* first line\nsecond line */ { wasm32-core }\n",
        );
        assert!(input.refusals().contains(&PackageRefusal::Grammar));
    }
}
