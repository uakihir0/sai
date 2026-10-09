//! Comparison of a root `sai.package` contract with its lock root block.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use crate::lexer::PackageLexError;
use crate::lock::{LockParseError, LockParseOutput, LockRootField};
use crate::lock_validation::{LockValidationOutput, validate as validate_lock};
use crate::manifest::{
    PackageDeclaration, PackageParseError, PackageParseOutput, PackageRole, Requirement, Spanned,
};
use crate::validation::{
    PackageValidationContext, PackageValidationOutput, validate as validate_package,
};
use crate::version::Version;

/// The comparable portion of one included root edge.
///
/// Source clauses and capability grants are intentionally absent: the lock
/// root block does not store them.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct RootContractRequirement {
    pub package: Box<str>,
    /// Canonical spelling, preserving comparator order and partial precision.
    pub requirement: Box<str>,
    pub test: bool,
    /// A sorted, duplicate-free set; `None` remains distinct from `Some`.
    pub only: Option<Vec<Box<str>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RootContractEdge {
    pub requirement: RootContractRequirement,
    pub span: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockRootContractMismatch {
    pub kind: LockRootContractMismatchKind,
    pub package_span: Range<usize>,
    pub lock_span: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockRootContractMismatchKind {
    Identity {
        package: Box<str>,
        lock: Box<str>,
    },
    Version {
        package: Version,
        lock: Version,
    },
    Targets {
        package: Vec<Box<str>>,
        lock: Vec<Box<str>>,
    },
    Requirements {
        missing_from_lock: Vec<RootContractEdge>,
        unexpected_in_lock: Vec<RootContractEdge>,
    },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LockRootContractOutput {
    pub mismatches: Vec<LockRootContractMismatch>,
}

impl LockRootContractOutput {
    /// Reports whether Mode A conditions 2 and 3 match.
    ///
    /// This is not a complete Mode A decision. Callers must also validate
    /// recorded graph edges and reachability, path pins, current dependency
    /// contracts, and source selection under conditions 4 through 7.
    pub fn matches_contract(&self) -> bool {
        self.mismatches.is_empty()
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockRootContractInputError {
    PackageWasNotParsedAsRoot,
    InvalidPackageSyntax {
        lexical_errors: Vec<PackageLexError>,
        syntax_errors: Vec<PackageParseError>,
    },
    InvalidPackageSemantics(PackageValidationOutput),
    InvalidLockSemantics(LockValidationOutput),
    InvalidLockSyntax {
        lexical_errors: Vec<PackageLexError>,
        syntax_errors: Vec<LockParseError>,
    },
}

/// Compares the root identity, version, target set, and included edges.
///
/// Both inputs must be syntactically and locally semantically valid. The
/// package is validated in root role. Requirement equality uses canonical
/// comparator spelling and treats `only` and target lists as sets.
///
/// A matching result checks only Mode A conditions 2 and 3 from resolver
/// §2.10.2. It does not establish that the whole lock is usable in Mode A;
/// callers must separately check conditions 4 through 7, including graph
/// consistency, path pins, dependency contracts, and source selection.
pub fn compare(
    package: &PackageParseOutput,
    lock: &LockParseOutput,
) -> Result<LockRootContractOutput, LockRootContractInputError> {
    if package.role() != PackageRole::Root {
        return Err(LockRootContractInputError::PackageWasNotParsedAsRoot);
    }
    if !package.is_syntax_valid() {
        return Err(LockRootContractInputError::InvalidPackageSyntax {
            lexical_errors: package.lexical_errors.clone(),
            syntax_errors: package.errors.clone(),
        });
    }
    let package_validation = validate_package(package, PackageValidationContext::default());
    if !package_validation.is_valid() {
        return Err(LockRootContractInputError::InvalidPackageSemantics(
            package_validation,
        ));
    }

    // Keep unsupported-format diagnostics ahead of ordinary lock syntax
    // diagnostics, as the lock validator does for the other lock consumers.
    let lock_validation = validate_lock(lock);
    if !lock_validation.is_valid() {
        return Err(LockRootContractInputError::InvalidLockSemantics(
            lock_validation,
        ));
    }
    if !lock.is_syntax_valid() {
        return Err(LockRootContractInputError::InvalidLockSyntax {
            lexical_errors: lock.lexical_errors.clone(),
            syntax_errors: lock.errors.clone(),
        });
    }

    let package_header = package
        .file
        .header
        .as_ref()
        .expect("valid root package has a header");
    let lock_root = lock
        .file
        .root
        .as_ref()
        .expect("valid lock has a root block");
    let package_root_targets = package
        .file
        .declarations
        .iter()
        .find_map(|declaration| match &declaration.value {
            PackageDeclaration::Targets(targets) => Some((targets, declaration.span.clone())),
            _ => None,
        })
        .expect("valid root package has targets");
    let lock_root_targets = lock_root
        .value
        .fields
        .iter()
        .find_map(|field| match &field.value {
            LockRootField::Targets(targets) => Some((targets, field.span.clone())),
            LockRootField::Requires(_) => None,
        })
        .expect("valid lock root has targets");

    let package_targets = package_root_targets
        .0
        .iter()
        .map(|target| target.value.clone())
        .collect::<BTreeSet<_>>();
    let lock_targets = lock_root_targets
        .0
        .iter()
        .map(|target| target.value.clone())
        .collect::<BTreeSet<_>>();
    let package_edges = root_contract_edges(package, &package_targets);
    let lock_edges = lock_root_edges(&lock_root.value);

    let mut output = LockRootContractOutput::default();
    if package_header.value.identity.to_string() != lock_root.value.package.as_ref() {
        output.mismatches.push(LockRootContractMismatch {
            kind: LockRootContractMismatchKind::Identity {
                package: package_header.value.identity.to_string().into(),
                lock: lock_root.value.package.clone(),
            },
            package_span: package_header.span.clone(),
            lock_span: lock_root.span.clone(),
        });
    }
    if package_header.value.version != lock_root.value.version {
        output.mismatches.push(LockRootContractMismatch {
            kind: LockRootContractMismatchKind::Version {
                package: package_header.value.version.clone(),
                lock: lock_root.value.version.clone(),
            },
            package_span: package_header.span.clone(),
            lock_span: lock_root.span.clone(),
        });
    }
    if package_targets != lock_targets {
        output.mismatches.push(LockRootContractMismatch {
            kind: LockRootContractMismatchKind::Targets {
                package: package_targets.into_iter().collect(),
                lock: lock_targets.into_iter().collect(),
            },
            package_span: package_root_targets.1,
            lock_span: lock_root_targets.1,
        });
    }

    let missing_from_lock = package_edges
        .iter()
        .filter(|(key, _)| !lock_edges.contains_key(*key))
        .map(|(requirement, span)| RootContractEdge {
            requirement: requirement.clone(),
            span: span.clone(),
        })
        .collect::<Vec<_>>();
    let unexpected_in_lock = lock_edges
        .iter()
        .filter(|(key, _)| !package_edges.contains_key(*key))
        .map(|(requirement, span)| RootContractEdge {
            requirement: requirement.clone(),
            span: span.clone(),
        })
        .collect::<Vec<_>>();
    if !missing_from_lock.is_empty() || !unexpected_in_lock.is_empty() {
        output.mismatches.push(LockRootContractMismatch {
            kind: LockRootContractMismatchKind::Requirements {
                missing_from_lock,
                unexpected_in_lock,
            },
            package_span: package_header.span.clone(),
            lock_span: lock_root.span.clone(),
        });
    }

    Ok(output)
}

fn root_contract_edges(
    package: &PackageParseOutput,
    root_targets: &BTreeSet<Box<str>>,
) -> BTreeMap<RootContractRequirement, Range<usize>> {
    let mut edges = BTreeMap::new();
    for declaration in &package.file.declarations {
        let PackageDeclaration::Requires { test, entries } = &declaration.value else {
            continue;
        };
        for entry in entries {
            if !is_included(&entry.value, root_targets) {
                continue;
            }
            let requirement = requirement_key(&entry.value, *test, entry.span.clone());
            edges.insert(requirement.0, requirement.1);
        }
    }
    edges
}

fn lock_root_edges(
    root: &crate::lock::LockRoot,
) -> BTreeMap<RootContractRequirement, Range<usize>> {
    let mut edges = BTreeMap::new();
    for field in &root.fields {
        let LockRootField::Requires(requirement) = &field.value else {
            continue;
        };
        let key = RootContractRequirement {
            package: requirement.package.clone(),
            requirement: requirement.requirement.to_string().into(),
            test: requirement.test,
            only: requirement.only.as_ref().map(|targets| sorted_set(targets)),
        };
        edges.insert(key, field.span.clone());
    }
    edges
}

fn requirement_key(
    requirement: &Requirement,
    test: bool,
    span: Range<usize>,
) -> (RootContractRequirement, Range<usize>) {
    (
        RootContractRequirement {
            package: requirement.package.clone(),
            requirement: requirement.version.to_string().into(),
            test,
            only: requirement
                .only
                .as_ref()
                .map(|targets| sorted_set(&targets.value)),
        },
        span,
    )
}

fn sorted_set(values: &[Spanned<Box<str>>]) -> Vec<Box<str>> {
    values
        .iter()
        .map(|value| value.value.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn is_included(requirement: &Requirement, root_targets: &BTreeSet<Box<str>>) -> bool {
    requirement.only.as_ref().is_none_or(|only| {
        only.value
            .iter()
            .any(|target| root_targets.contains(target.value.as_ref()))
    })
}

#[cfg(test)]
mod tests {
    use super::{LockRootContractInputError, LockRootContractMismatchKind, compare};
    use crate::lock;
    use crate::manifest;

    fn package(edges: &str, targets: &str) -> manifest::PackageParseOutput {
        manifest::parse(
            format!(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets {{ {targets} }}\n\
                 {edges}"
            )
            .as_bytes(),
        )
    }

    fn lock(root: &str) -> lock::LockParseOutput {
        lock::parse(
            format!(
                "lock 1\n\
                 root acme:app @ 1.0.0\n\
                     targets {{ wasm32-wasi, wasm32-core }}\n\
                     {root}"
            )
            .as_bytes(),
        )
    }

    #[test]
    fn matches_canonical_root_metadata_and_edges() {
        let package = package(
            "requires {\n\
                 acme:a @ ^1.2 from path \"../a\" only { wasm32-wasi }\n\
                 acme:test-kit @ >=1.0, <2 from path \"../test-kit\"\n\
             }\n\
             requires test { acme:test-runner @ ~2 from path \"../test-runner\" }\n",
            "wasm32-core, wasm32-wasi",
        );
        let lock = lock(
            "requires acme:a @ ^1.2 -> 1.4.0 only { wasm32-wasi }\n\
             requires acme:test-kit @ >=1.0, <2 -> 1.9.0\n\
             requires test acme:test-runner @ ~2 -> 2.1.0\n",
        );
        let output = compare(&package, &lock).expect("valid inputs");
        assert!(output.matches_contract(), "{:?}", output.mismatches);
    }

    #[test]
    fn compares_target_and_only_lists_as_sets() {
        let package = package(
            "requires { acme:a @ ^1 from path \"../a\" only { wasm32-wasi, wasm32-core } }\n",
            "wasm32-core, wasm32-wasi, wasm32-wasi",
        );
        let lock =
            lock("requires acme:a @ ^1 -> 1.1.0 only { wasm32-core, wasm32-wasi, wasm32-core }\n");
        let output = compare(&package, &lock).expect("valid inputs");
        assert!(output.matches_contract(), "{:?}", output.mismatches);
    }

    #[test]
    fn reports_changed_root_identity_version_targets_and_requirements() {
        let package = manifest::parse(
            b"package acme:app @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-wasi }\n\
              requires { acme:a @ ^1 from path \"../a\" }\n",
        );
        let lock = lock(
            "requires acme:a @ ~1 -> 1.1.0\n\
             requires acme:extra @ ^2 -> 2.0.0\n",
        );
        let mut lock = lock;
        lock.file.root.as_mut().expect("root").value.package = "acme:other".into();
        lock.file.root.as_mut().expect("root").value.version =
            crate::version::Version::parse("2.0.0").expect("valid version");
        for field in &mut lock.file.root.as_mut().expect("root").value.fields {
            if let crate::lock::LockRootField::Targets(targets) = &mut field.value {
                targets[0].value = "wasm32-core".into();
            }
        }

        let output = compare(&package, &lock).expect("valid input semantics");
        assert!(output.mismatches.iter().any(|mismatch| matches!(
            mismatch.kind,
            LockRootContractMismatchKind::Identity { .. }
        )));
        assert!(
            output.mismatches.iter().any(|mismatch| matches!(
                mismatch.kind,
                LockRootContractMismatchKind::Version { .. }
            ))
        );
        assert!(
            output.mismatches.iter().any(|mismatch| matches!(
                mismatch.kind,
                LockRootContractMismatchKind::Targets { .. }
            ))
        );
        let requirements = output
            .mismatches
            .iter()
            .find_map(|mismatch| match &mismatch.kind {
                LockRootContractMismatchKind::Requirements {
                    missing_from_lock,
                    unexpected_in_lock,
                } => Some((missing_from_lock, unexpected_in_lock)),
                _ => None,
            })
            .expect("requirement mismatch");
        assert_eq!(requirements.0.len(), 1);
        assert_eq!(requirements.1.len(), 2);
    }

    #[test]
    fn reports_test_flag_and_only_set_changes() {
        let package = package(
            "requires test { acme:tool @ ^1 from path \"../tool\" only { wasm32-wasi } }\n",
            "wasm32-wasi, wasm32-core",
        );
        let lock = lock("requires acme:tool @ ^1 -> 1.0.0 only { wasm32-core }\n");
        let output = compare(&package, &lock).expect("valid inputs");
        let mismatch = output
            .mismatches
            .iter()
            .find_map(|mismatch| match &mismatch.kind {
                LockRootContractMismatchKind::Requirements {
                    missing_from_lock,
                    unexpected_in_lock,
                } => Some((missing_from_lock, unexpected_in_lock)),
                _ => None,
            })
            .expect("requirement mismatch");
        assert_eq!(mismatch.0.len(), 1);
        assert_eq!(mismatch.1.len(), 1);
        assert!(mismatch.0[0].requirement.test);
        assert!(!mismatch.1[0].requirement.test);
    }

    #[test]
    fn rejects_invalid_inputs_before_comparison() {
        let invalid_package = manifest::parse(b"package acme:app @ 1.0.0\n");
        let valid_lock = lock("");
        assert!(matches!(
            compare(&invalid_package, &valid_lock),
            Err(LockRootContractInputError::InvalidPackageSemantics(_))
        ));

        let valid_package = package("", "wasm32-wasi, wasm32-core");
        let invalid_lock = lock::parse(b"lock 2\n");
        assert!(matches!(
            compare(&valid_package, &invalid_lock),
            Err(LockRootContractInputError::InvalidLockSemantics(_))
        ));

        let dependency = manifest::parse_for_role(
            b"package acme:app @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-wasi, wasm32-core }\n\
              requires test { acme:first @ ^1 from path \"../first\" }\n\
              requires test { acme:second @ ^1 from path \"../second\" }\n",
            manifest::PackageRole::Dependency,
        );
        assert!(dependency.is_syntax_valid(), "{:?}", dependency.errors);
        assert_eq!(
            compare(&dependency, &valid_lock),
            Err(LockRootContractInputError::PackageWasNotParsedAsRoot)
        );
    }

    #[test]
    fn root_contract_match_does_not_certify_the_recorded_graph() {
        let package = package(
            "requires { acme:a @ ^1 from path \"../a\" }\n",
            "wasm32-wasi, wasm32-core",
        );
        let lock = lock("requires acme:a @ ^1 -> 1.0.0\n");

        assert!(
            compare(&package, &lock)
                .expect("valid inputs")
                .matches_contract()
        );
        let graph = crate::lock_graph_validation::validate(&lock).expect("lock is locally valid");
        assert!(!graph.is_consistent());
    }
}
