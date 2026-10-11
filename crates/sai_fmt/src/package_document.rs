//! Package-contract document builders.
//!
//! Builders are added declaration by declaration. Until comment handling is
//! available for a declaration, the builder refuses to create a document that
//! would discard one of its comments.
#![allow(dead_code)]

use sai_package::lexer::{PackageToken, PackageTokenKind};

use crate::package_input::PackageInput;
use crate::package_surface::{PackageBraceList, PackageBraceOwner, PackageSyntaxIndex};
use crate::{Doc, GroupId};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageDocumentError {
    InputRefused,
    WrongListOwner,
    InvalidEntry,
    CommentHandlingPending,
}

/// Build a canonical document for one syntactically valid `targets` list.
///
/// The caller checks the package-input refusal set before constructing a
/// document. This intermediate builder rejects comments rather than dropping
/// them; comment-aware target rendering is added separately.
pub(crate) fn targets_declaration(
    input: &PackageInput<'_>,
    index: &PackageSyntaxIndex,
    list: &PackageBraceList,
) -> Result<Doc, PackageDocumentError> {
    if !input.refusals().is_empty() {
        return Err(PackageDocumentError::InputRefused);
    }
    if list.owner != PackageBraceOwner::Targets {
        return Err(PackageDocumentError::WrongListOwner);
    }

    let tokens = &input.lexed().tokens;
    let keyword_index = (list.open_token as usize)
        .checked_sub(1)
        .ok_or(PackageDocumentError::InvalidEntry)?;
    if token_text(input.source(), &tokens[keyword_index]) != "targets" {
        return Err(PackageDocumentError::InvalidEntry);
    }

    let close_token = list.close_token as usize;
    if input.comments().iter().any(|comment| {
        let token_index = match comment.attachment() {
            crate::package_input::CommentAttachment::Leading(index)
            | crate::package_input::CommentAttachment::Trailing(index) => index,
            crate::package_input::CommentAttachment::EndOfFile => return false,
        };
        (keyword_index..=close_token).contains(&token_index)
    }) {
        return Err(PackageDocumentError::CommentHandlingPending);
    }

    let entries = index.entries(list);
    let mut target_docs = Vec::with_capacity(entries.len());
    for entry in entries {
        if entry.end != entry.start + 1 {
            return Err(PackageDocumentError::InvalidEntry);
        }
        let token = &tokens[entry.start as usize];
        if token.kind != PackageTokenKind::Word {
            return Err(PackageDocumentError::InvalidEntry);
        }
        target_docs.push(Doc::text(token_text(input.source(), token)));
    }

    if target_docs.is_empty() {
        return Ok(Doc::text("targets {}"));
    }

    let group = GroupId(list.open_token);
    let mut contents = Vec::with_capacity(target_docs.len() * 2 - 1);
    for (position, target) in target_docs.into_iter().enumerate() {
        contents.push(target);
        if position + 1 < entries.len() {
            let previous_end = tokens[entries[position].start as usize].span.end;
            let next_start = tokens[entries[position + 1].start as usize].span.start;
            let newlines_between = input.source()[previous_end..next_start]
                .iter()
                .filter(|&&byte| byte == b'\n')
                .count();
            let broken_separator = if newlines_between > 1 {
                Doc::concat([Doc::Line, Doc::Line])
            } else {
                Doc::Line
            };
            contents.push(Doc::if_broken(group, broken_separator, Doc::text(", ")));
        }
    }

    Ok(Doc::group(
        group,
        Doc::concat([
            Doc::text("targets {"),
            Doc::indent(Doc::concat([Doc::Line, Doc::concat(contents)])),
            Doc::Line,
            Doc::text("}"),
        ]),
    ))
}

fn token_text<'a>(source: &'a [u8], token: &'a PackageToken) -> &'a str {
    std::str::from_utf8(&source[token.span.clone()])
        .expect("package token spans point to valid UTF-8")
}

#[cfg(test)]
mod tests {
    use crate::package_input::PackageInput;
    use crate::package_surface::{PackageBraceOwner, PackageSyntaxIndex};
    use crate::{GroupId, print, print_with_forced_groups};

    use super::{PackageDocumentError, targets_declaration};

    fn render(source: &[u8], force_break: bool) -> String {
        let input = PackageInput::new(source);
        assert!(input.refusals().is_empty(), "{:?}", input.refusals());
        let index = PackageSyntaxIndex::new(&input).expect("valid target declaration structure");
        let list = index
            .brace_lists()
            .iter()
            .find(|list| list.owner == PackageBraceOwner::Targets)
            .expect("targets list");
        let doc = targets_declaration(&input, &index, list).expect("targets document");
        if force_break {
            print_with_forced_groups(&doc, &[GroupId(list.open_token)]).expect("print document")
        } else {
            print(&doc).expect("print document")
        }
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
    fn refuses_to_build_a_targets_document_with_comments_until_they_are_supported() {
        let source =
            b"package acme:app @ 1.0.0\ntargets {\n    wasm32-core, // keep this\n    wasm32-wasi\n}\n";
        let input = PackageInput::new(source);
        assert!(input.refusals().is_empty());
        let index = PackageSyntaxIndex::new(&input).expect("valid target declaration structure");
        let list = index
            .brace_lists()
            .iter()
            .find(|list| list.owner == PackageBraceOwner::Targets)
            .expect("targets list");
        assert_eq!(
            targets_declaration(&input, &index, list),
            Err(PackageDocumentError::CommentHandlingPending)
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
}
