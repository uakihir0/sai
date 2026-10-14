//! Canonical serialization for lock-locally valid `sai.lock` files.

use crate::lexer::PackageLexError;
use crate::lock::{
    LockFile, LockPackage, LockPackageField, LockParseError, LockParseOutput, LockRequirement,
    LockRootField, LockSource,
};
use crate::lock_validation::{LockValidationError, validate};
use crate::manifest::{RequirementSource, Spanned};
use crate::validation::capability_covers;

/// Why a parsed lock could not be serialized.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockSerializationError {
    InvalidSyntax {
        lexical_errors: Vec<PackageLexError>,
        syntax_errors: Vec<LockParseError>,
    },
    InvalidSemantics(Vec<LockValidationError>),
}

/// Reports whether a lock's bytes match their canonical serialization.
///
/// Returns an error for syntax or lock-local semantic failures. Callers must
/// validate graph-dependent capability names in the original parsed lock
/// before calling this function, since canonicalization may remove covered
/// capabilities.
pub fn is_canonical(source: &[u8]) -> Result<bool, LockSerializationError> {
    let parsed = crate::lock::parse(source);
    let canonical = serialize_canonical(&parsed)?;
    Ok(canonical.as_bytes() == source)
}

/// Serializes a syntactically and lock-locally semantically valid lock.
///
/// The serializer sorts blocks and fields, normalizes set-valued lists, and
/// emits the exact line layout specified by `docs/package/03-lock-file.md`.
/// Invalid parsed input is rejected so callers cannot accidentally serialize
/// a corrupt or unsupported lock. Checks that require the resolved package
/// graph, such as whether a WIT capability is known, remain the resolver's
/// responsibility. The resolver must validate capability names in the
/// original parsed lock before calling this function, since normalization can
/// remove a covered but unknown capability; callers must complete those
/// checks before persisting or using the serialized lock.
pub fn serialize_canonical(parsed: &LockParseOutput) -> Result<String, LockSerializationError> {
    let validation = validate(parsed);
    if !validation.is_valid() {
        return Err(LockSerializationError::InvalidSemantics(validation.errors));
    }

    if !parsed.is_syntax_valid() {
        return Err(LockSerializationError::InvalidSyntax {
            lexical_errors: parsed.lexical_errors.clone(),
            syntax_errors: parsed.errors.clone(),
        });
    }

    Ok(serialize_valid_lock(&parsed.file))
}

/// Serializes one previously validated package block in canonical field order.
///
/// Conflict-side reconciliation uses this only to choose a stable record when
/// the same Git content appears on both sides with different resolution
/// metadata. It does not validate or persist a lock.
pub(crate) fn serialize_package_for_ordering(package: &LockPackage) -> String {
    let mut output = String::new();
    write_package(&mut output, package);
    output
}

fn serialize_valid_lock(file: &LockFile) -> String {
    let format_version = file
        .format_version
        .expect("validated lock has a format version");
    let root = &file.root.as_ref().expect("validated lock has a root").value;
    let mut output = format!("lock {format_version}\n\n");

    output.push_str("root ");
    output.push_str(&root.package);
    output.push_str(" @ ");
    output.push_str(&root.version.to_string());
    output.push('\n');

    if let Some(targets) = root.fields.iter().find_map(|field| match &field.value {
        LockRootField::Targets(items) => Some(items),
        LockRootField::Requires(_) => None,
    }) {
        output.push_str("    targets ");
        write_name_list(&mut output, targets);
        output.push('\n');
    }

    let mut root_requirements = root
        .fields
        .iter()
        .filter_map(|field| match &field.value {
            LockRootField::Requires(requirement) => Some(requirement.as_ref()),
            LockRootField::Targets(_) => None,
        })
        .collect::<Vec<_>>();
    root_requirements
        .sort_unstable_by(|left, right| left.package.as_bytes().cmp(right.package.as_bytes()));
    for requirement in root_requirements {
        output.push_str("    ");
        write_requirement(&mut output, requirement);
        output.push('\n');
    }

    let mut packages = file.packages.iter().collect::<Vec<_>>();
    packages.sort_unstable_by(|left, right| {
        left.value
            .package
            .as_bytes()
            .cmp(right.value.package.as_bytes())
            .then_with(|| left.value.version.cmp(&right.value.version))
    });
    for package in packages {
        output.push('\n');
        write_package(&mut output, &package.value);
    }

    output
}

fn write_package(output: &mut String, package: &LockPackage) {
    output.push_str("package ");
    output.push_str(&package.package);
    output.push_str(" @ ");
    output.push_str(&package.version.to_string());
    output.push('\n');

    if let Some(source) = package.fields.iter().find_map(|field| match &field.value {
        LockPackageField::Source(source) => Some(source),
        _ => None,
    }) {
        match source {
            LockSource::Git(url) => {
                output.push_str("    source git ");
                write_quoted(output, url);
            }
            LockSource::Path(path) => {
                output.push_str("    source path ");
                write_quoted(output, path);
            }
        }
        output.push('\n');
    }

    write_remaining_package_fields(output, package);
}

fn write_remaining_package_fields(output: &mut String, package: &LockPackage) {
    if let Some(value) = package.fields.iter().find_map(|field| match &field.value {
        LockPackageField::Directory(value) if value.as_ref() != "." => Some(value),
        _ => None,
    }) {
        output.push_str("    dir ");
        write_quoted(output, value);
        output.push('\n');
    }
    if let Some(value) = package.fields.iter().find_map(|field| match &field.value {
        LockPackageField::Commit(value) => Some(value),
        _ => None,
    }) {
        output.push_str("    commit ");
        write_quoted(output, value);
        output.push('\n');
    }
    if let Some(value) = package.fields.iter().find_map(|field| match &field.value {
        LockPackageField::Checksum(value) => Some(value),
        _ => None,
    }) {
        output.push_str("    checksum ");
        write_quoted(output, value);
        output.push('\n');
    }
    if let Some(value) = package.fields.iter().find_map(|field| match &field.value {
        LockPackageField::WitChecksum(value) => Some(value),
        _ => None,
    }) {
        output.push_str("    wit ");
        write_quoted(output, value);
        output.push('\n');
    }

    let mut artifacts = package
        .fields
        .iter()
        .filter_map(|field| match &field.value {
            LockPackageField::Artifact { target, checksum } => Some((target, checksum)),
            _ => None,
        })
        .collect::<Vec<_>>();
    artifacts.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    for (target, checksum) in artifacts {
        output.push_str("    artifact ");
        output.push_str(target);
        output.push(' ');
        write_quoted(output, checksum);
        output.push('\n');
    }

    if let Some(requirement) = package.fields.iter().find_map(|field| match &field.value {
        LockPackageField::Compiler(requirement) => Some(requirement),
        _ => None,
    }) {
        output.push_str("    compiler ");
        output.push_str(&requirement.to_string());
        output.push('\n');
    }
    if let Some(capabilities) = package.fields.iter().find_map(|field| match &field.value {
        LockPackageField::Capabilities(items) => Some(items),
        _ => None,
    }) {
        output.push_str("    capabilities ");
        write_capability_list(output, capabilities);
        output.push('\n');
    }

    let mut imports = package
        .fields
        .iter()
        .filter_map(|field| match &field.value {
            LockPackageField::Imports {
                target,
                capabilities,
            } => Some((target, capabilities)),
            _ => None,
        })
        .collect::<Vec<_>>();
    imports.sort_unstable_by(|left, right| left.0.as_bytes().cmp(right.0.as_bytes()));
    for (target, capabilities) in imports {
        output.push_str("    imports ");
        output.push_str(target);
        output.push(' ');
        write_name_list(output, capabilities);
        output.push('\n');
    }

    let mut requirements = package
        .fields
        .iter()
        .filter_map(|field| match &field.value {
            LockPackageField::Requires(requirement) => Some(requirement.as_ref()),
            _ => None,
        })
        .collect::<Vec<_>>();
    requirements
        .sort_unstable_by(|left, right| left.package.as_bytes().cmp(right.package.as_bytes()));
    for requirement in requirements {
        output.push_str("    ");
        write_requirement(output, requirement);
        output.push('\n');
    }
}

fn write_requirement(output: &mut String, requirement: &LockRequirement) {
    output.push_str("requires ");
    if requirement.test {
        output.push_str("test ");
    }
    output.push_str(&requirement.package);
    output.push_str(" @ ");
    output.push_str(&requirement.requirement.to_string());
    output.push_str(" -> ");
    output.push_str(&requirement.selected.to_string());
    if let Some(source) = &requirement.source {
        output.push_str(" from ");
        match &source.value {
            RequirementSource::Git { url, directory } => {
                output.push_str("git ");
                write_quoted(output, url);
                if let Some(directory) = directory.as_deref().filter(|directory| *directory != ".")
                {
                    output.push_str(" dir ");
                    write_quoted(output, directory);
                }
            }
            RequirementSource::Path(path) => {
                output.push_str("path ");
                write_quoted(output, path);
            }
        }
    }
    if let Some(only) = &requirement.only {
        output.push_str(" only ");
        write_name_list(output, only);
    }
    if let Some(grant) = &requirement.grant {
        output.push_str(" grant ");
        write_name_list(output, grant);
    }
}

fn write_name_list(output: &mut String, items: &[Spanned<Box<str>>]) {
    let values = sorted_unique_values(items);
    write_values(output, &values);
}

fn write_capability_list(output: &mut String, items: &[Spanned<Box<str>>]) {
    let values = sorted_unique_values(items);
    let normalized = values
        .iter()
        .enumerate()
        .filter(|(index, value)| {
            !values.iter().enumerate().any(|(other_index, other)| {
                index != &other_index && capability_covers(other, value)
            })
        })
        .map(|(_, value)| *value)
        .collect::<Vec<_>>();
    write_values(output, &normalized);
}

fn write_values(output: &mut String, values: &[&str]) {
    output.push('{');
    if !values.is_empty() {
        output.push(' ');
    }
    for (index, value) in values.iter().enumerate() {
        if index != 0 {
            output.push_str(", ");
        }
        output.push_str(value);
    }
    output.push_str(" }");
}

fn sorted_unique_values(items: &[Spanned<Box<str>>]) -> Vec<&str> {
    let mut values = items
        .iter()
        .map(|item| item.value.as_ref())
        .collect::<Vec<_>>();
    values.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
    values.dedup();
    values
}

fn write_quoted(output: &mut String, value: &str) {
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            _ => output.push(character),
        }
    }
    output.push('"');
}

#[cfg(test)]
mod tests {
    use super::{LockSerializationError, is_canonical, serialize_canonical};
    use crate::lock::parse;

    #[test]
    fn sorts_fields_and_normalizes_set_valued_lists() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 requires acme:z @ >=1.2, <2.0 -> 1.5.0 only { wasm32-wasi, wasm32-wasi }\n\
                 targets { wasm32-wasi, wasm32-core, wasm32-wasi }\n\
                 requires acme:a @ ^1 -> 1.1.0\n\
             package acme:dep @ 1.2.0\n\
                 capabilities { wasi:http/types, wasi:http, alloc, alloc }\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 requires acme:z @ ^1 -> 1.3.0\n\
                 source git \"https://github.com/acme/dep\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 requires acme:a @ >=1.2, <2.0 -> 1.4.0\n";

        let serialized = serialize_canonical(&parse(source.as_bytes())).unwrap();
        assert_eq!(
            serialized,
            r#"lock 1

root acme:app @ 1.0.0
    targets { wasm32-core, wasm32-wasi }
    requires acme:a @ ^1 -> 1.1.0
    requires acme:z @ >=1.2, <2.0 -> 1.5.0 only { wasm32-wasi }

package acme:dep @ 1.2.0
    source git "https://github.com/acme/dep"
    commit "0123456789012345678901234567890123456789"
    checksum "sai1:0123456789012345678901234567890123456789012345678901234567890123"
    capabilities { alloc, wasi:http }
    requires acme:a @ >=1.2, <2.0 -> 1.4.0
    requires acme:z @ ^1 -> 1.3.0
"#
        );
        assert!(serialized.ends_with('\n'));
        assert!(!serialized.ends_with("\n\n"));
        let reparsed = parse(serialized.as_bytes());
        assert_eq!(serialize_canonical(&reparsed).unwrap(), serialized);
    }

    #[test]
    fn serializes_v2_requirement_source_only_and_grant_in_canonical_order() {
        let source = "lock 2\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
                 requires acme:z @ ^1 -> 1.3.0 from git \"https://example.test/repo\" dir \".\" only { wasm32-wasi, wasm32-wasi } grant { wasi:http, alloc, alloc }\n\
                 requires acme:a @ ^1 -> 1.2.0 from path \"../a\" grant { }\n";

        let serialized = serialize_canonical(&parse(source.as_bytes())).unwrap();
        assert_eq!(
            serialized,
            r#"lock 2

root acme:app @ 1.0.0
    targets { wasm32-wasi }
    requires acme:a @ ^1 -> 1.2.0 from path "../a" grant { }
    requires acme:z @ ^1 -> 1.3.0 from git "https://example.test/repo" only { wasm32-wasi } grant { alloc, wasi:http }
"#
        );
        assert!(is_canonical(serialized.as_bytes()).unwrap());

        let reparsed = parse(serialized.as_bytes());
        let root = &reparsed.file().root.as_ref().unwrap().value;
        let empty_grant = root
            .fields
            .iter()
            .find_map(|field| match &field.value {
                crate::lock::LockRootField::Requires(requirement)
                    if requirement.package.as_ref() == "acme:a" =>
                {
                    Some(requirement.grant.as_ref().unwrap())
                }
                _ => None,
            })
            .unwrap();
        assert!(empty_grant.is_empty());
    }

    #[test]
    fn sorts_entries_and_artifact_fields_by_their_canonical_keys() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi, wasm64-wasi }\n\
             package acme:z @ 1.0.0\n\
                 source path \"deps/z\"\n\
             package acme:a @ 2.0.0\n\
                 source path \"deps/a2\"\n\
             package acme:a @ 1.0.0\n\
                 source path \"deps/a1\"\n\
             package acme:component @ 1.0.0\n\
                 imports wasm64-wasi { }\n\
                 artifact wasm64-wasi \"sai1:2123456789012345678901234567890123456789012345678901234567890123\"\n\
                 wit \"sai1:1123456789012345678901234567890123456789012345678901234567890123\"\n\
                 source git \"https://example.test/component\"\n\
                 dir \".\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 artifact wasm32-wasi \"sai1:3123456789012345678901234567890123456789012345678901234567890123\"\n\
                 imports wasm32-wasi { wasi:cli, wasi:http/types, wasi:http }\n";

        let serialized = serialize_canonical(&parse(source.as_bytes())).unwrap();
        let package_names = serialized
            .lines()
            .filter_map(|line| line.strip_prefix("package "))
            .collect::<Vec<_>>();
        assert_eq!(
            package_names,
            [
                "acme:a @ 1.0.0",
                "acme:a @ 2.0.0",
                "acme:component @ 1.0.0",
                "acme:z @ 1.0.0",
            ]
        );
        let artifact_start = serialized.find("    artifact wasm32-wasi").unwrap();
        let artifact_next = serialized.find("    artifact wasm64-wasi").unwrap();
        let imports_start = serialized.find("    imports wasm32-wasi").unwrap();
        let imports_next = serialized.find("    imports wasm64-wasi").unwrap();
        assert!(artifact_start < artifact_next);
        assert!(imports_start < imports_next);
        assert!(serialized.contains("imports wasm64-wasi { }\n"));
        assert!(
            serialized.contains("imports wasm32-wasi { wasi:cli, wasi:http, wasi:http/types }")
        );
        assert!(!serialized.contains("    dir \".\"\n"));
    }

    #[test]
    fn writes_empty_capability_sets_with_canonical_spacing() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package acme:dep @ 1.0.0\n\
                 source git \"https://example.test/dep\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 capabilities { }\n";

        let serialized = serialize_canonical(&parse(source.as_bytes())).unwrap();
        assert!(serialized.contains("    capabilities { }\n"));
        assert!(!serialized.contains("{  }"));
    }

    #[test]
    fn identifies_canonical_and_valid_noncanonical_locks() {
        let canonical = b"lock 1\n\nroot acme:app @ 1.0.0\n    targets { wasm32-wasi }\n";
        assert!(is_canonical(canonical).unwrap());

        let noncanonical = [
            b"lock 1\nroot acme:app @ 1.0.0\ntargets { wasm32-wasi }\n".as_slice(),
            b"lock 1\n\nroot acme:app @ 1.0.0\n    targets { wasm32-wasi, wasm32-wasi }\n",
            b"lock 1\n\nroot acme:app @ 1.0.0\n    targets { wasm32-wasi }\n// comment\n",
        ];
        for source in noncanonical {
            assert!(
                !is_canonical(source).unwrap(),
                "expected noncanonical: {source:?}"
            );
        }
    }

    #[test]
    fn canonicality_rejects_invalid_input() {
        assert!(matches!(
            is_canonical(b"lock 1\n"),
            Err(LockSerializationError::InvalidSyntax { .. })
        ));
    }

    #[test]
    fn refuses_syntactically_or_semantically_invalid_locks() {
        let syntax_error = serialize_canonical(&parse(b"lock 1\n")).unwrap_err();
        assert!(matches!(
            syntax_error,
            LockSerializationError::InvalidSyntax { .. }
        ));

        let unsupported_format = serialize_canonical(&parse(b"lock 3\n")).unwrap_err();
        assert!(matches!(
            unsupported_format,
            LockSerializationError::InvalidSemantics(errors)
                if errors.iter().any(|error| matches!(
                    error.kind,
                    crate::lock_validation::LockValidationErrorKind::UnsupportedFormatVersion(3)
                ))
        ));

        let semantic_error = serialize_canonical(&parse(
            b"lock 1\nroot acme:app @ 1.0.0\ntargets { unknown-target }\n",
        ))
        .unwrap_err();
        assert!(matches!(
            semantic_error,
            LockSerializationError::InvalidSemantics(_)
        ));
    }
}
