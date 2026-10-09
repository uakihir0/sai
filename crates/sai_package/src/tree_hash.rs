//! Deterministic `sai1:` checksums for Git trees and package artifacts.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt::{self, Display, Formatter, Write as _};

use sha2::{Digest, Sha256};
use unicode_general_category::{GeneralCategory, get_general_category};
use unicode_normalization::UnicodeNormalization;

use crate::unicode_casefold_16::FULL_CASE_FOLD;

/// Whether the tree is a package source tree or a separately declared WIT tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeHashPurpose {
    /// Excludes `.sai` and `out` at the tree root.
    Source,
    /// Applies the common Git path rules without source-only exclusions.
    Wit,
}

/// A flattened entry from a Git tree, with paths relative to the input tree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitTreeEntry<'a> {
    /// Git mode `100644`.
    RegularFile { path: &'a [u8], content: &'a [u8] },
    /// Git mode `100755`; hashed identically to a regular file.
    ExecutableFile { path: &'a [u8], content: &'a [u8] },
    /// Git mode `040000`; contributes no bytes of its own.
    Directory { path: &'a [u8] },
    /// Git mode `120000`; excluded and reported as a warning.
    Symlink { path: &'a [u8] },
    /// Git mode `160000`; excluded and reported as a warning.
    Gitlink { path: &'a [u8] },
}

impl GitTreeEntry<'_> {
    fn path_bytes(&self) -> &[u8] {
        match self {
            Self::RegularFile { path, .. }
            | Self::ExecutableFile { path, .. }
            | Self::Directory { path }
            | Self::Symlink { path }
            | Self::Gitlink { path } => path,
        }
    }
}

/// A skipped entry that the lock-file specification requires reporting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeHashWarning {
    /// The corresponding warning code.
    pub kind: TreeHashWarningKind,
    /// The original Git path bytes.
    pub path: Vec<u8>,
}

/// Kind of non-fatal entry excluded from a tree checksum.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeHashWarningKind {
    /// Git symbolic links are never materialized or hashed.
    SymlinkIgnored,
    /// Git submodules are never fetched or hashed.
    SubmoduleIgnored,
}

/// A tree checksum and the warnings produced while selecting its files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeHash {
    /// The canonical `sai1:` digest.
    pub checksum: String,
    /// Symlink and submodule warnings, sorted by raw path bytes.
    pub warnings: Vec<TreeHashWarning>,
}

/// A deterministic failure while validating or hashing a Git tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeHashError {
    /// A selected file path does not meet the portable path contract.
    InvalidPath {
        /// Original Git path bytes.
        path: Vec<u8>,
        /// The first violated path rule.
        reason: InvalidPathReason,
    },
    /// Two distinct paths have the same NFC and full-case-folded spelling.
    PathCollision {
        /// The first path in raw-byte order.
        first: Vec<u8>,
        /// The later colliding path in raw-byte order.
        second: Vec<u8>,
    },
    /// A malformed tree contains more than one non-directory entry at a path.
    DuplicatePath {
        /// The duplicated raw Git path.
        path: Vec<u8>,
    },
}

/// Path validation rule that failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidPathReason {
    /// Git paths must be valid UTF-8.
    InvalidUtf8,
    /// A path component is empty.
    EmptyComponent,
    /// A component is `.` or `..`.
    DotComponent,
    /// A path contains a forbidden control or bidirectional control.
    ControlCharacter,
    /// A path contains a character forbidden in portable package paths.
    ForbiddenCharacter,
    /// A component is a reserved Windows device name.
    ReservedDeviceName,
    /// A component ends in a dot or a space.
    TrailingDotOrSpace,
    /// A component exceeds 255 UTF-8 bytes.
    ComponentTooLong,
    /// The full path exceeds 4096 bytes.
    PathTooLong,
}

impl Display for InvalidPathReason {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::InvalidUtf8 => "path is not valid UTF-8",
            Self::EmptyComponent => "path contains an empty component",
            Self::DotComponent => "path contains a `.` or `..` component",
            Self::ControlCharacter => "path contains a control character",
            Self::ForbiddenCharacter => "path contains a forbidden character",
            Self::ReservedDeviceName => "path contains a reserved Windows device name",
            Self::TrailingDotOrSpace => "component ends with a dot or space",
            Self::ComponentTooLong => "component exceeds 255 UTF-8 bytes",
            Self::PathTooLong => "path exceeds 4096 bytes",
        };
        f.write_str(message)
    }
}

impl Display for TreeHashError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPath { path, reason } => write!(
                f,
                "invalid package file path `{}`: {reason}",
                diagnostic_path(path)
            ),
            Self::PathCollision { first, second } => write!(
                f,
                "package paths `{}` and `{}` collide after Unicode normalization and case folding",
                diagnostic_path(first),
                diagnostic_path(second)
            ),
            Self::DuplicatePath { path } => write!(
                f,
                "Git tree contains more than one entry at `{}`",
                diagnostic_path(path)
            ),
        }
    }
}

impl Error for TreeHashError {}

/// Hash a flattened Git tree using the normative `sai1:` tree encoding.
///
/// The caller walks Git tree objects and supplies each reachable entry once.
/// This function never reads the working tree or follows symlinks.
pub fn hash_git_tree<'a>(
    entries: impl IntoIterator<Item = GitTreeEntry<'a>>,
    purpose: TreeHashPurpose,
) -> Result<TreeHash, TreeHashError> {
    let mut files = Vec::new();
    let mut warnings = Vec::new();
    let mut seen_paths = BTreeMap::<Vec<u8>, ()>::new();

    for entry in entries {
        let path = entry.path_bytes();
        if should_skip_path(path, purpose) {
            continue;
        }

        match entry {
            GitTreeEntry::Directory { path } => {
                ensure_unique_path(&mut seen_paths, path)?;
            }
            GitTreeEntry::RegularFile { path, content }
            | GitTreeEntry::ExecutableFile { path, content } => {
                validate_tree_path(path)?;
                ensure_unique_path(&mut seen_paths, path)?;
                files.push((path, content));
            }
            GitTreeEntry::Symlink { path } => {
                ensure_unique_path(&mut seen_paths, path)?;
                warnings.push(TreeHashWarning {
                    kind: TreeHashWarningKind::SymlinkIgnored,
                    path: path.to_vec(),
                });
            }
            GitTreeEntry::Gitlink { path } => {
                ensure_unique_path(&mut seen_paths, path)?;
                warnings.push(TreeHashWarning {
                    kind: TreeHashWarningKind::SubmoduleIgnored,
                    path: path.to_vec(),
                });
            }
        }
    }

    files.sort_by_key(|(path, _)| *path);
    warnings.sort_by(|left, right| left.path.cmp(&right.path));
    reject_casefold_collisions(&files)?;

    let mut tree_hasher = Sha256::new();
    for (path, content) in files {
        let content_digest = Sha256::digest(content);
        let digest_hex = lowercase_hex(&content_digest);
        tree_hasher.update(digest_hex.as_bytes());
        tree_hasher.update(b"  ");
        tree_hasher.update(path);
        tree_hasher.update(b"\n");
    }

    Ok(TreeHash {
        checksum: format!("sai1:{}", lowercase_hex(&tree_hasher.finalize())),
        warnings,
    })
}

/// Hash the exact bytes of one artifact using the lock-file `sai1:` encoding.
pub fn artifact_checksum(content: &[u8]) -> String {
    format!("sai1:{}", lowercase_hex(&Sha256::digest(content)))
}

/// Validate a selected Git file path against the portable package-path rules.
pub fn validate_tree_path(path: &[u8]) -> Result<(), TreeHashError> {
    let invalid = |reason| TreeHashError::InvalidPath {
        path: path.to_vec(),
        reason,
    };
    let path_text =
        std::str::from_utf8(path).map_err(|_| invalid(InvalidPathReason::InvalidUtf8))?;

    for component in path_text.split('/') {
        if component.is_empty() {
            return Err(invalid(InvalidPathReason::EmptyComponent));
        }
        if component == "." || component == ".." {
            return Err(invalid(InvalidPathReason::DotComponent));
        }
        for character in component.chars() {
            let codepoint = character as u32;
            if (0x0000..=0x001F).contains(&codepoint)
                || (0x007F..=0x009F).contains(&codepoint)
                || (0x202A..=0x202E).contains(&codepoint)
                || (0x2066..=0x2069).contains(&codepoint)
            {
                return Err(invalid(InvalidPathReason::ControlCharacter));
            }
            if matches!(character, '\\' | '<' | '>' | ':' | '"' | '|' | '?' | '*') {
                return Err(invalid(InvalidPathReason::ForbiddenCharacter));
            }
        }
        if is_reserved_device_name(component) {
            return Err(invalid(InvalidPathReason::ReservedDeviceName));
        }
        if component.ends_with('.') || component.ends_with(' ') {
            return Err(invalid(InvalidPathReason::TrailingDotOrSpace));
        }
        if component.len() > 255 {
            return Err(invalid(InvalidPathReason::ComponentTooLong));
        }
    }
    if path.len() > 4096 {
        return Err(invalid(InvalidPathReason::PathTooLong));
    }
    Ok(())
}

fn should_skip_path(path: &[u8], purpose: TreeHashPurpose) -> bool {
    let components = path.split(|byte| *byte == b'/');
    if components.clone().any(|component| component == b".git") {
        return true;
    }
    purpose == TreeHashPurpose::Source
        && matches!(components.into_iter().next(), Some(b".sai" | b"out"))
}

fn ensure_unique_path(seen: &mut BTreeMap<Vec<u8>, ()>, path: &[u8]) -> Result<(), TreeHashError> {
    if seen.insert(path.to_vec(), ()).is_some() {
        return Err(TreeHashError::DuplicatePath {
            path: path.to_vec(),
        });
    }
    Ok(())
}

fn is_reserved_device_name(component: &str) -> bool {
    let stem = component
        .split_once('.')
        .map_or(component, |(stem, _)| stem);
    let stem = stem.to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                suffix.len() == 1
                    && suffix
                        .as_bytes()
                        .first()
                        .is_some_and(|byte| matches!(byte, b'1'..=b'9'))
            })
        })
}

fn reject_casefold_collisions(files: &[(&[u8], &[u8])]) -> Result<(), TreeHashError> {
    let mut folded_paths = BTreeMap::<String, &[u8]>::new();
    for (path, _) in files {
        let text = std::str::from_utf8(path)
            .expect("file paths were validated before collision detection");
        let normalized: String = text.nfc().collect();
        let folded = full_case_fold(&normalized);
        if let Some(first) = folded_paths.get(&folded) {
            if *first != *path {
                return Err(TreeHashError::PathCollision {
                    first: first.to_vec(),
                    second: path.to_vec(),
                });
            }
        } else {
            folded_paths.insert(folded, path);
        }
    }
    Ok(())
}

fn full_case_fold(text: &str) -> String {
    let mut folded = String::with_capacity(text.len());
    for character in text.chars() {
        let codepoint = character as u32;
        match FULL_CASE_FOLD.binary_search_by_key(&codepoint, |(key, _)| *key) {
            Ok(index) => folded.push_str(FULL_CASE_FOLD[index].1),
            Err(_) => folded.push(character),
        }
    }
    folded
}

fn lowercase_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn diagnostic_path(path: &[u8]) -> String {
    let mut rendered = String::new();
    let mut remaining = path;
    while !remaining.is_empty() {
        match std::str::from_utf8(remaining) {
            Ok(valid) => {
                append_escaped_text(&mut rendered, valid);
                break;
            }
            Err(error) => {
                let valid_length = error.valid_up_to();
                if valid_length > 0 {
                    let valid = std::str::from_utf8(&remaining[..valid_length])
                        .expect("the UTF-8 validator reported a valid prefix");
                    append_escaped_text(&mut rendered, valid);
                    remaining = &remaining[valid_length..];
                }
                let invalid_length = error.error_len().unwrap_or(remaining.len());
                for byte in &remaining[..invalid_length] {
                    write!(rendered, "\\x{byte:02X}").expect("writing to a String cannot fail");
                }
                remaining = &remaining[invalid_length..];
            }
        }
    }
    rendered
}

fn append_escaped_text(rendered: &mut String, text: &str) {
    for character in text.chars() {
        match character {
            '\\' => rendered.push_str("\\\\"),
            '`' => rendered.push_str("\\`"),
            _ if matches!(
                get_general_category(character),
                GeneralCategory::Control
                    | GeneralCategory::Format
                    | GeneralCategory::LineSeparator
                    | GeneralCategory::ParagraphSeparator
            ) =>
            {
                write!(rendered, "\\u{{{:x}}}", character as u32)
                    .expect("writing to a String cannot fail");
            }
            _ => rendered.push(character),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        GitTreeEntry, InvalidPathReason, TreeHashError, TreeHashPurpose, TreeHashWarningKind,
        artifact_checksum, hash_git_tree, validate_tree_path,
    };

    #[test]
    fn matches_the_lock_file_tree_hash_vector() {
        let entries = [
            GitTreeEntry::RegularFile {
                path: b"sai.package",
                content: b"package acme:hello @ 0.1.0\nsource \"src\"\ntargets { wasm32-core }\n",
            },
            GitTreeEntry::RegularFile {
                path: b"README.md",
                content: b"Hello\n",
            },
            GitTreeEntry::ExecutableFile {
                path: b"src/lib.sai",
                content: b"pub fn answer() -> i32 {\n    42\n}\n",
            },
            GitTreeEntry::RegularFile {
                path: b"src/empty.txt",
                content: b"",
            },
            GitTreeEntry::RegularFile {
                path: b"src/a.b",
                content: b"x\n",
            },
        ];
        let result = hash_git_tree(entries, TreeHashPurpose::Source).unwrap();
        assert_eq!(
            result.checksum,
            "sai1:9b4c1b8a5126574063c4c23803fe7eaa86a3d012b8d61b398f0980cc8abe8672"
        );
        assert!(result.warnings.is_empty());
    }

    #[test]
    fn source_and_wit_selection_differ_only_for_source_output_paths() {
        let entries = [
            GitTreeEntry::RegularFile {
                path: b".sai/state",
                content: b"state",
            },
            GitTreeEntry::RegularFile {
                path: b"out/module.wasm",
                content: b"output",
            },
            GitTreeEntry::RegularFile {
                path: b".git/config",
                content: b"ignored",
            },
            GitTreeEntry::RegularFile {
                path: b"wit/world.wit",
                content: b"world",
            },
        ];
        let source = hash_git_tree(entries, TreeHashPurpose::Source).unwrap();
        let wit = hash_git_tree(entries, TreeHashPurpose::Wit).unwrap();
        assert_ne!(source.checksum, wit.checksum);
        assert_eq!(
            source.checksum,
            hash_git_tree(
                [GitTreeEntry::RegularFile {
                    path: b"wit/world.wit",
                    content: b"world",
                }],
                TreeHashPurpose::Source
            )
            .unwrap()
            .checksum
        );
        assert_eq!(
            wit.checksum,
            hash_git_tree(
                [
                    GitTreeEntry::RegularFile {
                        path: b".sai/state",
                        content: b"state",
                    },
                    GitTreeEntry::RegularFile {
                        path: b"out/module.wasm",
                        content: b"output",
                    },
                    GitTreeEntry::RegularFile {
                        path: b"wit/world.wit",
                        content: b"world",
                    },
                ],
                TreeHashPurpose::Wit
            )
            .unwrap()
            .checksum
        );
    }

    #[test]
    fn symlinks_and_gitlinks_are_sorted_warnings() {
        let result = hash_git_tree(
            [
                GitTreeEntry::Gitlink {
                    path: b"z/submodule",
                },
                GitTreeEntry::Symlink { path: b"a/link" },
                GitTreeEntry::Symlink { path: b".sai/link" },
            ],
            TreeHashPurpose::Source,
        )
        .unwrap();
        assert_eq!(result.warnings.len(), 2);
        assert_eq!(result.warnings[0].kind, TreeHashWarningKind::SymlinkIgnored);
        assert_eq!(result.warnings[0].path, b"a/link");
        assert_eq!(
            result.warnings[1].kind,
            TreeHashWarningKind::SubmoduleIgnored
        );
        assert_eq!(result.warnings[1].path, b"z/submodule");
    }

    #[test]
    fn executable_bit_does_not_change_the_tree_hash() {
        let regular = hash_git_tree(
            [GitTreeEntry::RegularFile {
                path: b"file",
                content: b"bytes",
            }],
            TreeHashPurpose::Source,
        )
        .unwrap();
        let executable = hash_git_tree(
            [GitTreeEntry::ExecutableFile {
                path: b"file",
                content: b"bytes",
            }],
            TreeHashPurpose::Source,
        )
        .unwrap();
        assert_eq!(regular.checksum, executable.checksum);
    }

    #[test]
    fn rejects_case_and_normalization_collisions() {
        for (first, second) in [("Straße", "STRASSE"), ("Σ", "ς"), ("\u{00e9}", "e\u{0301}")] {
            let error = hash_git_tree(
                [
                    GitTreeEntry::RegularFile {
                        path: first.as_bytes(),
                        content: b"one",
                    },
                    GitTreeEntry::RegularFile {
                        path: second.as_bytes(),
                        content: b"two",
                    },
                ],
                TreeHashPurpose::Source,
            )
            .unwrap_err();
            assert!(
                matches!(error, TreeHashError::PathCollision { .. }),
                "{error}"
            );
        }
    }

    #[test]
    fn validates_portable_path_rules() {
        let cases: &[(&[u8], InvalidPathReason)] = &[
            (b"/absolute", InvalidPathReason::EmptyComponent),
            (b"a//b", InvalidPathReason::EmptyComponent),
            (b"a/./b", InvalidPathReason::DotComponent),
            (b"a/../b", InvalidPathReason::DotComponent),
            (b"bad\x80", InvalidPathReason::InvalidUtf8),
            (
                "a/\u{202e}b".as_bytes(),
                InvalidPathReason::ControlCharacter,
            ),
            (b"a:b", InvalidPathReason::ForbiddenCharacter),
            (b"NUL.txt", InvalidPathReason::ReservedDeviceName),
            (b"lpt9.sai", InvalidPathReason::ReservedDeviceName),
            (b"trailing.", InvalidPathReason::TrailingDotOrSpace),
            (b"trailing ", InvalidPathReason::TrailingDotOrSpace),
        ];
        for (path, expected) in cases {
            let error = validate_tree_path(path).unwrap_err();
            assert!(
                matches!(error, TreeHashError::InvalidPath { reason, .. } if reason == *expected),
                "path {:?}: {error}",
                String::from_utf8_lossy(path)
            );
        }

        assert!(matches!(
            validate_tree_path(&vec![b'a'; 256]),
            Err(TreeHashError::InvalidPath {
                reason: InvalidPathReason::ComponentTooLong,
                ..
            })
        ));
        let mut too_long_path = Vec::new();
        while too_long_path.len() < 4097 {
            if !too_long_path.is_empty() {
                too_long_path.push(b'/');
            }
            too_long_path.push(b'a');
        }
        assert!(matches!(
            validate_tree_path(&too_long_path),
            Err(TreeHashError::InvalidPath {
                reason: InvalidPathReason::PathTooLong,
                ..
            })
        ));
    }

    #[test]
    fn path_filtering_precedes_path_validation() {
        let result = hash_git_tree(
            [
                GitTreeEntry::RegularFile {
                    path: b"nested/.git/\xff",
                    content: b"ignored",
                },
                GitTreeEntry::RegularFile {
                    path: b".sai/\xff",
                    content: b"ignored",
                },
                GitTreeEntry::RegularFile {
                    path: b"valid",
                    content: b"kept",
                },
            ],
            TreeHashPurpose::Source,
        )
        .unwrap();
        assert_eq!(
            result.checksum,
            hash_git_tree(
                [GitTreeEntry::RegularFile {
                    path: b"valid",
                    content: b"kept",
                }],
                TreeHashPurpose::Source
            )
            .unwrap()
            .checksum
        );
    }

    #[test]
    fn rejects_duplicate_paths_across_entry_kinds() {
        let error = hash_git_tree(
            [
                GitTreeEntry::RegularFile {
                    path: b"same",
                    content: b"one",
                },
                GitTreeEntry::Symlink { path: b"same" },
            ],
            TreeHashPurpose::Source,
        )
        .unwrap_err();
        assert!(matches!(error, TreeHashError::DuplicatePath { .. }));

        let error = hash_git_tree(
            [
                GitTreeEntry::Directory { path: b"same" },
                GitTreeEntry::RegularFile {
                    path: b"same",
                    content: b"file",
                },
            ],
            TreeHashPurpose::Source,
        )
        .unwrap_err();
        assert!(matches!(error, TreeHashError::DuplicatePath { .. }));
    }

    #[test]
    fn error_paths_escape_controls_bidi_formatting_and_invalid_utf8() {
        for (path, escaped) in [
            (&b"clip\x1b]52;c;payload"[..], "\\u{1b}]52;c;payload"),
            ("name/\u{202e}txt".as_bytes(), "name/\\u{202e}txt"),
            (&b"bad\xffname"[..], "bad\\xFFname"),
        ] {
            let error = hash_git_tree(
                [GitTreeEntry::RegularFile {
                    path,
                    content: b"unreachable",
                }],
                TreeHashPurpose::Source,
            )
            .unwrap_err();
            let rendered = error.to_string();
            assert!(
                rendered.contains(escaped),
                "expected {escaped:?} in {rendered:?}"
            );
            assert!(!rendered.as_bytes().contains(&0x1b));
            assert!(!rendered.contains('\u{202e}'));
            assert!(!rendered.as_bytes().contains(&0xff));
        }
    }

    #[test]
    fn hashes_artifact_bytes_with_the_same_sai1_encoding() {
        assert_eq!(
            artifact_checksum(b"artifact"),
            "sai1:c7c5c1d70c5dec4416ab6158afd0b223ef40c29b1dc1f97ed9428b94d4cadb1c"
        );
    }
}
