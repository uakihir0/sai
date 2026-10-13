//! Structural parsing for package paths stored by the HIR.
//!
//! Package identities are retained as raw strings here. In particular,
//! `sai:core` and `sai:alloc` are valid use-path identities even though they
//! are reserved for package declarations.

use sai_hir::item_tree::UseEntry;
use sai_intern::{InternError, Interner, Symbol};

/// One empty side of the `namespace:package` identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EmptyIdentityPart {
    Namespace,
    Package,
}

/// A structural error in a HIR `UseEntry.pkg_path`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UsePathError {
    EmptyPath,
    MissingColon,
    MultipleColons,
    EmptyIdentityPart(EmptyIdentityPart),
    InvalidIdentityPart(EmptyIdentityPart),
    /// Zero-based index among path segments after the package identity.
    EmptySegment {
        index: usize,
    },
    /// Zero-based index among path segments after the package identity.
    InvalidPathSegment {
        index: usize,
    },
    Intern(InternError),
}

impl From<InternError> for UsePathError {
    fn from(error: InternError) -> Self {
        Self::Intern(error)
    }
}

/// A structurally parsed package path.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedUsePath {
    /// The exact `namespace:package` spelling, without path segments.
    pub package_identity: String,
    /// Exact path-segment spellings interned in the caller's symbol session.
    pub segments: Vec<Symbol>,
}

/// Parses the stored `UseEntry.pkg_path` text without applying declaration
/// identity rules or normalizing any spelling.
///
/// HIR does not retain the `PKG_PATH` token's `HAS_ERROR` bit, so this
/// boundary validates the package and module-segment grammar again.
pub fn parse_use_path(entry: &UseEntry, symbols: &Interner) -> Result<ParsedUsePath, UsePathError> {
    let pkg_path = entry.pkg_path.as_str();
    if pkg_path.is_empty() {
        return Err(UsePathError::EmptyPath);
    }

    let colon_count = pkg_path.bytes().filter(|byte| *byte == b':').count();
    match colon_count {
        0 => return Err(UsePathError::MissingColon),
        1 => {}
        _ => return Err(UsePathError::MultipleColons),
    }

    let (namespace, package_and_segments) = pkg_path
        .split_once(':')
        .expect("the colon count was checked above");
    if namespace.is_empty() {
        return Err(UsePathError::EmptyIdentityPart(
            EmptyIdentityPart::Namespace,
        ));
    }
    if !is_package_segment(namespace) {
        return Err(UsePathError::InvalidIdentityPart(
            EmptyIdentityPart::Namespace,
        ));
    }

    let package_end = package_and_segments
        .find('/')
        .unwrap_or(package_and_segments.len());
    let package_name = &package_and_segments[..package_end];
    if package_name.is_empty() {
        return Err(UsePathError::EmptyIdentityPart(EmptyIdentityPart::Package));
    }
    if !is_package_segment(package_name) {
        return Err(UsePathError::InvalidIdentityPart(
            EmptyIdentityPart::Package,
        ));
    }

    let identity_end = namespace.len() + 1 + package_end;
    let package_identity = pkg_path[..identity_end].to_owned();

    let segment_spellings = if package_end == package_and_segments.len() {
        Vec::new()
    } else {
        let path = &package_and_segments[package_end + 1..];
        path.split('/')
            .enumerate()
            .map(|(index, segment)| {
                if segment.is_empty() {
                    Err(UsePathError::EmptySegment { index })
                } else if !is_module_path_segment(segment) {
                    Err(UsePathError::InvalidPathSegment { index })
                } else {
                    Ok(segment)
                }
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let segments = segment_spellings
        .into_iter()
        .map(|segment| symbols.intern(segment).map_err(UsePathError::Intern))
        .collect::<Result<Vec<_>, _>>()?;

    Ok(ParsedUsePath {
        package_identity,
        segments,
    })
}

/// Matches the lexer's `scan_pkg_segment`: `[a-z][a-z0-9]*(?:-[a-z0-9]+)*`.
fn is_package_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_lowercase) {
        return false;
    }

    let mut end = 1;
    loop {
        while bytes
            .get(end)
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            end += 1;
        }
        if bytes.get(end) == Some(&b'-')
            && bytes
                .get(end + 1)
                .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            end += 1;
            continue;
        }
        return end == bytes.len();
    }
}

/// Matches the lexer's `scan_path_segment`:
/// `[a-z][a-z0-9_]*(?:-[a-z0-9]+)*`.
fn is_module_path_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_lowercase) {
        return false;
    }

    let mut end = 1;
    while bytes
        .get(end)
        .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || *byte == b'_')
    {
        end += 1;
    }
    while bytes.get(end) == Some(&b'-')
        && bytes
            .get(end + 1)
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    {
        end += 1;
        while bytes
            .get(end)
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        {
            end += 1;
        }
    }
    end == bytes.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::item_tree::{ErasedAstId, UseItem};

    fn entry(pkg_path: &str) -> UseEntry {
        UseEntry {
            ast_id: ErasedAstId::new(0),
            is_pub: false,
            pkg_path: pkg_path.to_owned(),
            alias: None,
            items: Vec::<UseItem>::new(),
            raw_attrs: Vec::new(),
        }
    }

    fn resolved_segments(path: &ParsedUsePath, symbols: &Interner) -> Vec<String> {
        path.segments
            .iter()
            .map(|segment| symbols.resolve(*segment).unwrap().to_string())
            .collect()
    }

    #[test]
    fn parses_root_and_nested_package_paths() {
        let symbols = Interner::new();

        let root = parse_use_path(&entry("acme:app"), &symbols).unwrap();
        assert_eq!(root.package_identity, "acme:app");
        assert!(root.segments.is_empty());

        let nested = parse_use_path(&entry("acme:app/model/user"), &symbols).unwrap();
        assert_eq!(nested.package_identity, "acme:app");
        assert_eq!(
            resolved_segments(&nested, &symbols),
            ["model".to_owned(), "user".to_owned()]
        );
    }

    #[test]
    fn accepts_implicit_core_and_alloc_identities() {
        let symbols = Interner::new();

        for identity in ["sai:core", "sai:alloc"] {
            let path = parse_use_path(&entry(identity), &symbols).unwrap();
            assert_eq!(path.package_identity, identity);
            assert!(path.segments.is_empty());
        }
    }

    #[test]
    fn preserves_hyphenated_spellings_without_normalization() {
        let symbols = Interner::new();
        let path = parse_use_path(&entry("acme:my-package/some-name"), &symbols).unwrap();

        assert_eq!(path.package_identity, "acme:my-package");
        assert_eq!(resolved_segments(&path, &symbols), ["some-name".to_owned()]);
    }

    #[test]
    fn preserves_valid_underscore_and_hyphen_path_spellings() {
        let symbols = Interner::new();
        let path = parse_use_path(
            &entry("acme-space:my-package/some_name-part2/trailing_/another_name"),
            &symbols,
        )
        .unwrap();

        assert_eq!(path.package_identity, "acme-space:my-package");
        assert_eq!(
            resolved_segments(&path, &symbols),
            [
                "some_name-part2".to_owned(),
                "trailing_".to_owned(),
                "another_name".to_owned()
            ]
        );
    }

    #[test]
    fn rejects_malformed_path_structure() {
        let symbols = Interner::new();

        let malformed = [
            ("", UsePathError::EmptyPath),
            ("acme", UsePathError::MissingColon),
            ("acme:pkg:extra", UsePathError::MultipleColons),
            (
                ":pkg",
                UsePathError::EmptyIdentityPart(EmptyIdentityPart::Namespace),
            ),
            (
                "acme:",
                UsePathError::EmptyIdentityPart(EmptyIdentityPart::Package),
            ),
            ("acme:pkg/", UsePathError::EmptySegment { index: 0 }),
            ("acme:pkg//model", UsePathError::EmptySegment { index: 0 }),
            (
                "acme:pkg/model//user",
                UsePathError::EmptySegment { index: 1 },
            ),
            ("acme:pkg/model/", UsePathError::EmptySegment { index: 1 }),
        ];

        for (pkg_path, expected) in malformed {
            assert_eq!(
                parse_use_path(&entry(pkg_path), &symbols),
                Err(expected),
                "unexpected parse result for {pkg_path:?}"
            );
        }
    }

    #[test]
    fn rejects_invalid_package_identity_spellings() {
        let symbols = Interner::new();
        let malformed = [
            (
                "Acme:pkg/foo",
                UsePathError::InvalidIdentityPart(EmptyIdentityPart::Namespace),
            ),
            (
                "acme:my--pkg/foo",
                UsePathError::InvalidIdentityPart(EmptyIdentityPart::Package),
            ),
            (
                "acme:my_pkg/foo",
                UsePathError::InvalidIdentityPart(EmptyIdentityPart::Package),
            ),
        ];

        for (pkg_path, expected) in malformed {
            assert_eq!(
                parse_use_path(&entry(pkg_path), &symbols),
                Err(expected),
                "unexpected parse result for {pkg_path:?}"
            );
        }
    }

    #[test]
    fn rejects_invalid_module_path_segment_spellings() {
        let symbols = Interner::new();
        let malformed = [
            "acme:pkg/foo--bar",
            "acme:pkg/Foo",
            "acme:pkg/foo-bar_baz",
            "acme:pkg/foo_bar--baz",
        ];

        for pkg_path in malformed {
            assert_eq!(
                parse_use_path(&entry(pkg_path), &symbols),
                Err(UsePathError::InvalidPathSegment { index: 0 }),
                "unexpected parse result for {pkg_path:?}"
            );
        }
    }
}
