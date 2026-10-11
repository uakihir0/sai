//! Lossless structural index for the token stream of a `sai.package` file.
//!
//! The package AST is a semantic projection and can omit duplicate
//! declarations or invalid values. This index keeps the original token
//! boundaries available to a later document builder.
#![allow(dead_code)]

use std::ops::Range;

use sai_package::lexer::{PackagePunctuator, PackageToken, PackageTokenKind};

use crate::package_input::PackageInput;

const NO_INDEX: u32 = u32::MAX;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PackageSyntaxIndex {
    declarations: Vec<Range<u32>>,
    top_level_newlines: Vec<u32>,
    brace_lists: Vec<PackageBraceList>,
    brace_entries: Vec<Range<u32>>,
    brace_separators: Vec<u32>,
    eof_token: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageListKind {
    CommaOrLine,
    LinesOnly,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct PackageBraceList {
    pub(crate) open_token: u32,
    pub(crate) close_token: u32,
    pub(crate) owner: PackageBraceOwner,
    parent_list_index: u32,
    parent_entry_index: u32,
    entry_range: Range<u32>,
    separator_range: Range<u32>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageBraceOwner {
    Targets,
    Requires,
    RequiresTest,
    ArtifactComponents,
    Overrides,
    ProfileBody,
    Only,
    Grant,
    Allow,
    Deny,
    Policy,
    Lints,
    Authors,
    Budget,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageSyntaxIndexError {
    InvalidUtf8,
    MissingEof,
    UnmatchedClosingBrace,
    UnclosedOpeningBrace,
    UnknownBraceOwner,
    IndexOverflow,
}

impl PackageSyntaxIndex {
    pub(crate) fn new(input: &PackageInput<'_>) -> Result<Self, PackageSyntaxIndexError> {
        std::str::from_utf8(input.source()).map_err(|_| PackageSyntaxIndexError::InvalidUtf8)?;
        let tokens = &input.lexed().tokens;
        let eof_token = tokens
            .iter()
            .position(|token| token.kind == PackageTokenKind::Eof)
            .ok_or(PackageSyntaxIndexError::MissingEof)?;

        let mut opening_braces: Vec<u32> = Vec::new();
        let mut brace_lists = Vec::new();
        for (index, token) in tokens[..eof_token].iter().enumerate() {
            match token.kind {
                PackageTokenKind::Punctuator(PackagePunctuator::LeftBrace) => {
                    let parent_list_index = opening_braces.last().copied().unwrap_or(NO_INDEX);
                    let owner =
                        list_owner(input.source(), tokens, index, parent_list_index == NO_INDEX)?;
                    let list_index = u32::try_from(brace_lists.len())
                        .map_err(|_| PackageSyntaxIndexError::IndexOverflow)?;
                    brace_lists.push(PackageBraceList {
                        open_token: token_index(index)?,
                        close_token: NO_INDEX,
                        owner,
                        parent_list_index,
                        parent_entry_index: NO_INDEX,
                        entry_range: 0..0,
                        separator_range: 0..0,
                    });
                    opening_braces.push(list_index);
                }
                PackageTokenKind::Punctuator(PackagePunctuator::RightBrace) => {
                    let list_index = opening_braces
                        .pop()
                        .ok_or(PackageSyntaxIndexError::UnmatchedClosingBrace)?;
                    brace_lists[list_index as usize].close_token = token_index(index)?;
                }
                _ => {}
            }
        }
        if !opening_braces.is_empty() {
            return Err(PackageSyntaxIndexError::UnclosedOpeningBrace);
        }

        let (declarations, top_level_newlines) = index_declarations(tokens, eof_token)?;
        let mut brace_entries = Vec::new();
        let mut brace_separators = Vec::new();
        for list_index in 0..brace_lists.len() {
            let open_token = brace_lists[list_index].open_token as usize;
            let close_token = brace_lists[list_index].close_token as usize;
            let owner = brace_lists[list_index].owner;
            let kind = list_kind(owner);
            let parent_entry_index = if brace_lists[list_index].parent_list_index == NO_INDEX {
                NO_INDEX
            } else {
                let parent = &brace_lists[brace_lists[list_index].parent_list_index as usize];
                let parent_entries = &brace_entries
                    [parent.entry_range.start as usize..parent.entry_range.end as usize];
                entry_containing(parent_entries, token_index(open_token)?).unwrap_or(NO_INDEX)
            };
            let entry_start = token_index(brace_entries.len())?;
            let separator_start = token_index(brace_separators.len())?;
            index_list_entries(
                tokens,
                open_token,
                close_token,
                kind,
                &brace_lists,
                &mut brace_entries,
                &mut brace_separators,
            )?;
            brace_lists[list_index].parent_entry_index = parent_entry_index;
            brace_lists[list_index].entry_range = entry_start..token_index(brace_entries.len())?;
            brace_lists[list_index].separator_range =
                separator_start..token_index(brace_separators.len())?;
        }

        Ok(Self {
            declarations,
            top_level_newlines,
            brace_lists,
            brace_entries,
            brace_separators,
            eof_token: token_index(eof_token)?,
        })
    }

    pub(crate) fn declarations(&self) -> &[Range<u32>] {
        &self.declarations
    }

    pub(crate) fn top_level_newlines(&self) -> &[u32] {
        &self.top_level_newlines
    }

    pub(crate) fn brace_lists(&self) -> &[PackageBraceList] {
        &self.brace_lists
    }

    pub(crate) fn entries(&self, list: &PackageBraceList) -> &[Range<u32>] {
        &self.brace_entries[list.entry_range.start as usize..list.entry_range.end as usize]
    }

    pub(crate) fn separators(&self, list: &PackageBraceList) -> &[u32] {
        &self.brace_separators
            [list.separator_range.start as usize..list.separator_range.end as usize]
    }

    pub(crate) fn eof_token(&self) -> usize {
        self.eof_token as usize
    }
}

impl PackageBraceList {
    pub(crate) const fn kind(&self) -> PackageListKind {
        list_kind(self.owner)
    }

    pub(crate) fn parent_open_token(&self, index: &PackageSyntaxIndex) -> Option<u32> {
        (self.parent_list_index != NO_INDEX)
            .then(|| index.brace_lists[self.parent_list_index as usize].open_token)
    }

    pub(crate) fn parent_entry(&self) -> Option<u32> {
        (self.parent_entry_index != NO_INDEX).then_some(self.parent_entry_index)
    }
}

fn index_declarations(
    tokens: &[PackageToken],
    eof_token: usize,
) -> Result<(Vec<Range<u32>>, Vec<u32>), PackageSyntaxIndexError> {
    let mut declarations = Vec::new();
    let mut top_level_newlines = Vec::new();
    let mut current_start = None;
    let mut depth: usize = 0;

    for (index, token) in tokens[..eof_token].iter().enumerate() {
        if depth == 0 && token.kind == PackageTokenKind::Newline {
            if let Some(start) = current_start.take() {
                declarations.push(start..token_index(index)?);
            }
            top_level_newlines.push(token_index(index)?);
            continue;
        }
        current_start.get_or_insert(token_index(index)?);
        match token.kind {
            PackageTokenKind::Punctuator(PackagePunctuator::LeftBrace) => {
                depth += 1;
            }
            PackageTokenKind::Punctuator(PackagePunctuator::RightBrace) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or(PackageSyntaxIndexError::UnmatchedClosingBrace)?;
            }
            _ => {}
        }
    }
    if let Some(start) = current_start {
        declarations.push(start..token_index(eof_token)?);
    }
    if depth != 0 {
        return Err(PackageSyntaxIndexError::UnclosedOpeningBrace);
    }
    Ok((declarations, top_level_newlines))
}

const fn list_kind(owner: PackageBraceOwner) -> PackageListKind {
    match owner {
        PackageBraceOwner::Requires
        | PackageBraceOwner::RequiresTest
        | PackageBraceOwner::ArtifactComponents
        | PackageBraceOwner::Overrides
        | PackageBraceOwner::ProfileBody => PackageListKind::LinesOnly,
        PackageBraceOwner::Targets
        | PackageBraceOwner::Only
        | PackageBraceOwner::Grant
        | PackageBraceOwner::Allow
        | PackageBraceOwner::Deny
        | PackageBraceOwner::Policy
        | PackageBraceOwner::Lints
        | PackageBraceOwner::Authors
        | PackageBraceOwner::Budget => PackageListKind::CommaOrLine,
    }
}

fn list_owner(
    source: &[u8],
    tokens: &[PackageToken],
    open_token: usize,
    top_level: bool,
) -> Result<PackageBraceOwner, PackageSyntaxIndexError> {
    if top_level && open_token >= 2 && token_text(source, &tokens[open_token - 2]) == "profile" {
        return Ok(PackageBraceOwner::ProfileBody);
    }

    let previous = open_token
        .checked_sub(1)
        .map(|index| token_text(source, &tokens[index]));
    let before_previous = open_token
        .checked_sub(2)
        .map(|index| token_text(source, &tokens[index]));
    match (previous, before_previous) {
        (Some("targets"), _) => Ok(PackageBraceOwner::Targets),
        (Some("requires"), _) => Ok(PackageBraceOwner::Requires),
        (Some("test"), Some("requires")) => Ok(PackageBraceOwner::RequiresTest),
        (Some("component"), Some("artifact")) => Ok(PackageBraceOwner::ArtifactComponents),
        (Some("overrides"), _) => Ok(PackageBraceOwner::Overrides),
        (Some("only"), _) => Ok(PackageBraceOwner::Only),
        (Some("grant"), _) => Ok(PackageBraceOwner::Grant),
        (Some("allow"), _) => Ok(PackageBraceOwner::Allow),
        (Some("deny"), _) => Ok(PackageBraceOwner::Deny),
        (Some("policy"), _) => Ok(PackageBraceOwner::Policy),
        (Some("lints"), _) => Ok(PackageBraceOwner::Lints),
        (Some("authors"), _) => Ok(PackageBraceOwner::Authors),
        (Some("budget"), _) => Ok(PackageBraceOwner::Budget),
        _ => Err(PackageSyntaxIndexError::UnknownBraceOwner),
    }
}

fn entry_containing(entries: &[Range<u32>], token_index: u32) -> Option<u32> {
    let index = entries.partition_point(|entry| entry.start <= token_index);
    index
        .checked_sub(1)
        .filter(|&candidate| entries[candidate].contains(&token_index))
        .map(|candidate| candidate as u32)
}

fn token_index(index: usize) -> Result<u32, PackageSyntaxIndexError> {
    u32::try_from(index).map_err(|_| PackageSyntaxIndexError::IndexOverflow)
}

fn token_text<'a>(source: &'a [u8], token: &'a PackageToken) -> &'a str {
    std::str::from_utf8(&source[token.span.clone()])
        .expect("package token spans point to valid UTF-8")
}

fn index_list_entries(
    tokens: &[PackageToken],
    open_token: usize,
    close_token: usize,
    kind: PackageListKind,
    brace_lists: &[PackageBraceList],
    entries: &mut Vec<Range<u32>>,
    separators: &mut Vec<u32>,
) -> Result<(), PackageSyntaxIndexError> {
    let mut entry_start = open_token + 1;
    let mut index = entry_start;

    while index < close_token {
        let token = &tokens[index];
        let separator = token.kind == PackageTokenKind::Newline
            || (kind == PackageListKind::CommaOrLine
                && token.kind == PackageTokenKind::Punctuator(PackagePunctuator::Comma));
        if separator {
            if entry_start < index {
                entries.push(token_index(entry_start)?..token_index(index)?);
            }
            separators.push(token_index(index)?);
            entry_start = index + 1;
            index += 1;
            continue;
        }

        if token.kind == PackageTokenKind::Punctuator(PackagePunctuator::LeftBrace) {
            let nested_open = token_index(index)?;
            let nested_index = brace_lists
                .binary_search_by_key(&nested_open, |list| list.open_token)
                .map_err(|_| PackageSyntaxIndexError::UnclosedOpeningBrace)?;
            index = brace_lists[nested_index].close_token as usize + 1;
        } else {
            index += 1;
        }
    }
    if entry_start < close_token {
        entries.push(token_index(entry_start)?..token_index(close_token)?);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use sai_package::lexer::{PackagePunctuator, PackageTokenKind};

    use super::{PackageBraceOwner, PackageListKind, PackageSyntaxIndex, PackageSyntaxIndexError};
    use crate::package_input::PackageInput;

    #[test]
    fn indexes_every_token_and_preserves_duplicate_declaration_ranges() {
        let input = PackageInput::new(
            b"package acme:app @ 1.0.0\npackage acme:app @ 1.0.0\ntargets { wasm32-core }\n",
        );
        assert!(input.refusals().is_empty());
        let index = PackageSyntaxIndex::new(&input).expect("valid package token structure");
        assert_eq!(index.declarations().len(), 3);

        let tokens = &input.lexed().tokens;
        let mut coverage = vec![0_u8; tokens.len()];
        for declaration in index.declarations() {
            for token_index in declaration.clone() {
                coverage[token_index as usize] += 1;
            }
        }
        for &newline in index.top_level_newlines() {
            coverage[newline as usize] += 1;
        }
        coverage[index.eof_token()] += 1;
        assert!(coverage.iter().all(|count| *count == 1), "{coverage:?}");
        assert_eq!(
            tokens[index.declarations()[0].start as usize].kind,
            PackageTokenKind::Word
        );
    }

    #[test]
    fn classifies_list_separators_by_the_enclosing_package_grammar() {
        let source = b"package acme:app @ 1.0.0\n\
targets { wasm32-core, wasm32-wasi }\n\
requires {\n\
    acme:one @ ^1.2,\n\
        <2.0\n\
    acme:two @ >=1.0\n\
}\n\
requires test {\n\
    acme:dev @ ^1.2,\n\
        <2.0\n\
        from path \"dev\"\n\
        only { wasm32-core }\n\
        grant { alloc }\n\
}\n\
artifact component {\n\
    wasm32-core \"core.wasm\"\n\
}\n\
overrides { acme:three use artifact }\n\
authors { \"Alice\", \"Bob\" }\n\
deny { wasi:filesystem }\n\
policy { deny unsafe, deny ambient_imports }\n\
lints { warn PUBLIC_EFFECTS_INFERRED }\n\
profile small {\n\
    allow { wasi:filesystem }\n\
    budget { wasm <= 64 KiB }\n\
}\n";
        let input = PackageInput::new(source);
        assert!(
            input.parsed().errors().is_empty(),
            "{:?}",
            input.parsed().errors()
        );
        let index = PackageSyntaxIndex::new(&input).expect("valid package token structure");
        let lists = index.brace_lists();

        assert_eq!(lists[0].kind(), PackageListKind::CommaOrLine);
        assert_eq!(lists[0].owner, PackageBraceOwner::Targets);
        assert_eq!(index.entries(&lists[0]).len(), 2);
        let requires = lists
            .iter()
            .find(|list| list.owner == PackageBraceOwner::Requires)
            .expect("requires is a line-separated entry list");
        assert_eq!(requires.kind(), PackageListKind::LinesOnly);
        assert_eq!(index.entries(requires).len(), 2);
        assert!(
            index.entries(requires)[0]
                .clone()
                .any(|token| { tokens_text(source, &input.lexed().tokens[token as usize]) == "," })
        );
        assert!(lists.iter().any(|list| {
            list.kind() == PackageListKind::LinesOnly && index.entries(list).len() == 1
        }));
        let requires_test = lists
            .iter()
            .find(|list| list.owner == PackageBraceOwner::RequiresTest)
            .expect("test requirements retain their distinct owner");
        assert_eq!(requires_test.kind(), PackageListKind::LinesOnly);
        assert_eq!(index.entries(requires_test).len(), 1);
        assert!(
            index.entries(requires_test)[0]
                .clone()
                .any(|token| { tokens_text(source, &input.lexed().tokens[token as usize]) == "," })
        );
        for owner in [PackageBraceOwner::Only, PackageBraceOwner::Grant] {
            let nested = lists
                .iter()
                .find(|list| list.owner == owner)
                .expect("nested requirement clause is indexed");
            assert_eq!(nested.kind(), PackageListKind::CommaOrLine);
            assert_eq!(
                nested.parent_open_token(&index),
                Some(requires_test.open_token)
            );
            assert_eq!(nested.parent_entry(), Some(0));
        }
        assert!(lists.iter().any(|list| {
            list.owner == PackageBraceOwner::ArtifactComponents
                && list.kind() == PackageListKind::LinesOnly
        }));
        assert!(lists.iter().any(|list| {
            list.owner == PackageBraceOwner::Overrides && list.kind() == PackageListKind::LinesOnly
        }));
        assert!(lists.iter().any(|list| {
            list.owner == PackageBraceOwner::ProfileBody
                && list.kind() == PackageListKind::LinesOnly
        }));
        assert!(lists.iter().any(|list| {
            list.owner == PackageBraceOwner::Budget
                && list.kind() == PackageListKind::CommaOrLine
                && list.parent_entry() == Some(1)
        }));
        assert!(lists.iter().any(|list| {
            list.owner == PackageBraceOwner::Authors && list.kind() == PackageListKind::CommaOrLine
        }));
        for owner in [
            PackageBraceOwner::Deny,
            PackageBraceOwner::Policy,
            PackageBraceOwner::Lints,
        ] {
            assert!(lists.iter().any(|list| {
                list.owner == owner && list.kind() == PackageListKind::CommaOrLine
            }));
        }
        let target_comma = index
            .separators(&lists[0])
            .iter()
            .copied()
            .find(|&index| {
                input.lexed().tokens[index as usize].kind
                    == PackageTokenKind::Punctuator(PackagePunctuator::Comma)
            })
            .expect("targets comma remains an explicit separator");
        assert_eq!(
            tokens_text(source, &input.lexed().tokens[target_comma as usize]),
            ","
        );
    }

    #[test]
    fn indexes_empty_optional_lists_and_rejects_empty_artifact_entries_as_grammar() {
        let input = PackageInput::new(
            b"package acme:app @ 1.0.0\ntargets {}\nrequires test {}\noverrides {}\n",
        );
        assert!(input.refusals().is_empty());
        let index = PackageSyntaxIndex::new(&input).expect("valid empty package lists");
        for owner in [
            PackageBraceOwner::Targets,
            PackageBraceOwner::RequiresTest,
            PackageBraceOwner::Overrides,
        ] {
            let list = index
                .brace_lists()
                .iter()
                .find(|list| list.owner == owner)
                .expect("empty list owner is indexed");
            assert!(index.entries(list).is_empty());
            assert!(index.separators(list).is_empty());
        }

        let input = PackageInput::new(b"package acme:app @ 1.0.0\nartifact component {}\n");
        assert!(
            input
                .refusals()
                .contains(&crate::package_input::PackageRefusal::Grammar)
        );
        let index = PackageSyntaxIndex::new(&input).expect("balanced artifact braces");
        assert!(index.entries(&index.brace_lists()[0]).is_empty());
    }

    #[test]
    fn links_repeated_profile_owners_and_comments_to_the_right_entries() {
        let source = b"package acme:app @ 1.0.0\n\
profile dev {\n\
    allow { alloc }\n\
    // budget applies after this comment\n\
    budget { wasm <= 1 MiB }\n\
}\n\
profile release {\n\
    budget { wasm <= 2 MiB }\n\
        allow { wasi:filesystem }\n\
}\n";
        let input = PackageInput::new(source);
        assert!(
            input.parsed().errors().is_empty(),
            "{:?}",
            input.parsed().errors()
        );
        let index = PackageSyntaxIndex::new(&input).expect("valid repeated profile structure");
        let profiles = index
            .brace_lists()
            .iter()
            .filter(|list| list.owner == PackageBraceOwner::ProfileBody)
            .collect::<Vec<_>>();
        assert_eq!(profiles.len(), 2);
        let nested = |owner: PackageBraceOwner, profile: &super::PackageBraceList| {
            index
                .brace_lists()
                .iter()
                .find(|list| {
                    list.owner == owner
                        && list.parent_open_token(&index) == Some(profile.open_token)
                })
                .expect("profile child belongs to its outer profile")
        };

        let dev_allow = nested(PackageBraceOwner::Allow, profiles[0]);
        let dev_budget = nested(PackageBraceOwner::Budget, profiles[0]);
        assert_eq!(dev_allow.parent_entry(), Some(0));
        assert_eq!(dev_budget.parent_entry(), Some(1));

        let release_budget = nested(PackageBraceOwner::Budget, profiles[1]);
        let release_allow = nested(PackageBraceOwner::Allow, profiles[1]);
        assert_eq!(release_budget.parent_entry(), Some(0));
        assert_eq!(release_allow.parent_entry(), Some(1));
    }

    #[test]
    fn scales_across_many_duplicate_brace_declarations() {
        let mut source = b"package acme:app @ 1.0.0\n".to_vec();
        for _ in 0..1_000 {
            source.extend_from_slice(b"allow {}\n");
        }
        let input = PackageInput::new(&source);
        assert!(input.refusals().is_empty());
        let index = PackageSyntaxIndex::new(&input).expect("balanced repeated declarations");
        assert_eq!(index.declarations().len(), 1_001);
        assert_eq!(index.brace_lists().len(), 1_000);
        assert!(
            index
                .brace_lists()
                .iter()
                .all(|list| list.owner == PackageBraceOwner::Allow)
        );
    }

    #[test]
    fn each_brace_list_accounts_for_its_direct_tokens_once() {
        let input = PackageInput::new(
            b"package acme:app @ 1.0.0\ntargets {\n wasm32-core, // target\n wasm32-wasi\n}\n",
        );
        let index = PackageSyntaxIndex::new(&input).expect("valid package token structure");
        let tokens = &input.lexed().tokens;

        for list in index.brace_lists() {
            let open_token = list.open_token as usize;
            let close_token = list.close_token as usize;
            let mut coverage = vec![0_u8; close_token - open_token + 1];
            coverage[0] += 1;
            *coverage
                .last_mut()
                .expect("brace range contains open and close") += 1;
            for entry in index.entries(list) {
                for token_index in entry.clone() {
                    coverage[token_index as usize - open_token] += 1;
                }
            }
            for &separator in index.separators(list) {
                coverage[separator as usize - open_token] += 1;
            }
            assert!(
                coverage.iter().all(|count| *count == 1),
                "list at {} has incomplete direct-token coverage: {coverage:?}; tokens={tokens:?}",
                open_token
            );
        }
    }

    #[test]
    fn reports_unbalanced_braces_in_unchecked_token_streams() {
        let input = PackageInput::new(b"package acme:app @ 1.0.0\ntargets {\n");
        assert!(matches!(
            PackageSyntaxIndex::new(&input),
            Err(PackageSyntaxIndexError::UnclosedOpeningBrace)
        ));
    }

    fn tokens_text<'a>(source: &'a [u8], token: &'a sai_package::lexer::PackageToken) -> &'a str {
        std::str::from_utf8(&source[token.span.clone()]).expect("valid token text")
    }
}
