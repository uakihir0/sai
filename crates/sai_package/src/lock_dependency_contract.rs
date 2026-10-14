//! Comparison of a dependency package contract with its `sai.lock` entry.
//!
//! This checks Mode A condition 5 for one package. The caller must first
//! validate the lock and establish condition 4 for the complete recorded
//! graph. In particular, this comparison intentionally does not compare a
//! locked edge's selected version with its version requirement.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use crate::lexer::PackageLexError;
use crate::lock::{LockPackage, LockPackageField, LockRequirement};
use crate::manifest::{
    PackageDeclaration, PackageParseError, PackageParseOutput, PackageRole, Requirement,
    RequirementSource, Spanned,
};
use crate::validation::{
    PackageValidationContext, PackageValidationOutput, validate as validate_package,
};
use crate::version::Version;

/// A source clause from a package dependency edge.
///
/// Git URLs and repository directories are canonicalized for comparison.
/// Path clauses retain their declared spelling because their interpretation
/// depends on the locked requiring package's source.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum DependencyContractSource {
    Git {
        url: Box<str>,
        directory: Option<Box<str>>,
    },
    Path(Box<str>),
}

/// The comparable portion of one included, non-test dependency edge.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct DependencyContractRequirement {
    pub package: Box<str>,
    /// Canonical spelling, preserving comparator order and partial precision.
    pub requirement: Box<str>,
    /// A sorted, duplicate-free set; `None` remains distinct from `Some`.
    pub only: Option<Vec<Box<str>>>,
    /// The originally declared source clause, before root overrides.
    pub source: Option<DependencyContractSource>,
    /// A sorted, duplicate-free set; `None` remains distinct from `Some`.
    pub grant: Option<Vec<Box<str>>>,
}

/// One edge involved in a dependency-contract mismatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DependencyContractEdge {
    pub requirement: DependencyContractRequirement,
    pub span: Range<usize>,
}

/// A mismatch between the current dependency manifest and its lock entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockDependencyContractMismatchKind {
    Identity {
        package: Box<str>,
        lock: Box<str>,
    },
    Version {
        package: Version,
        lock: Version,
    },
    Requirements {
        missing_from_lock: Vec<DependencyContractEdge>,
        unexpected_in_lock: Vec<DependencyContractEdge>,
    },
}

/// The result of comparing one dependency contract with one lock entry.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LockDependencyContractOutput {
    pub mismatches: Vec<LockDependencyContractMismatch>,
}

/// One mismatch and the source ranges that identify the compared package
/// contracts.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockDependencyContractMismatch {
    pub kind: LockDependencyContractMismatchKind,
    pub package_span: Range<usize>,
    pub lock_span: Range<usize>,
}

impl LockDependencyContractOutput {
    /// Reports whether the current dependency contract matches its lock entry.
    pub fn matches_contract(&self) -> bool {
        self.mismatches.is_empty()
    }
}

/// An input could not be used for dependency-contract comparison.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockDependencyContractInputError {
    PackageWasNotParsedAsDependency,
    InvalidPackageSyntax {
        lexical_errors: Vec<PackageLexError>,
        syntax_errors: Vec<PackageParseError>,
    },
    InvalidPackageSemantics(PackageValidationOutput),
    MissingPackageHeader,
    MissingDeclaredTargets,
    InvalidPackageGitSourceUrl {
        span: Range<usize>,
    },
    InvalidLockGitSourceUrl {
        span: Range<usize>,
    },
}

/// Compares a valid dependency-role package contract to its lock entry.
///
/// The current manifest is syntax- and semantics-checked here. The lock
/// entry must come from a lock that the caller has already syntax- and
/// semantics-validated, and the caller must establish condition 4 before
/// relying on this result. Test dependency edges are ignored. Ordinary edges
/// are included when their `only` set intersects `root_targets`. The
/// dependency's own target declaration is still checked by semantic
/// validation, which requires every `only` target to be declared by the
/// dependency. `root_targets` must be the validated target set of the current
/// root contract. `only`, source, and grant are compared as part of edge
/// identity. Resolved versions are condition 4's responsibility.
pub fn compare(
    package: &PackageParseOutput,
    lock: &Spanned<LockPackage>,
    root_targets: &BTreeSet<Box<str>>,
) -> Result<LockDependencyContractOutput, LockDependencyContractInputError> {
    if package.role() != PackageRole::Dependency {
        return Err(LockDependencyContractInputError::PackageWasNotParsedAsDependency);
    }
    if !package.is_syntax_valid() {
        return Err(LockDependencyContractInputError::InvalidPackageSyntax {
            lexical_errors: package.lexical_errors().to_vec(),
            syntax_errors: package.errors().to_vec(),
        });
    }
    let package_validation = validate_package(package, PackageValidationContext::default());
    if !package_validation.is_valid() {
        return Err(LockDependencyContractInputError::InvalidPackageSemantics(
            package_validation,
        ));
    }

    let Some(package_header) = package.file().header.as_ref() else {
        return Err(LockDependencyContractInputError::MissingPackageHeader);
    };
    let has_declared_targets = package
        .file()
        .declarations
        .iter()
        .any(|declaration| matches!(declaration.value, PackageDeclaration::Targets(_)));
    if !has_declared_targets {
        return Err(LockDependencyContractInputError::MissingDeclaredTargets);
    }

    let package_edges = package_contract_edges(package, root_targets)?;
    let lock_edges = lock_contract_edges(&lock.value)?;
    let mut output = LockDependencyContractOutput::default();

    if package_header.value.identity.to_string() != lock.value.package.as_ref() {
        output.mismatches.push(LockDependencyContractMismatch {
            kind: LockDependencyContractMismatchKind::Identity {
                package: package_header.value.identity.to_string().into(),
                lock: lock.value.package.clone(),
            },
            package_span: package_header.span.clone(),
            lock_span: lock.span.clone(),
        });
    }
    if package_header.value.version != lock.value.version {
        output.mismatches.push(LockDependencyContractMismatch {
            kind: LockDependencyContractMismatchKind::Version {
                package: package_header.value.version.clone(),
                lock: lock.value.version.clone(),
            },
            package_span: package_header.span.clone(),
            lock_span: lock.span.clone(),
        });
    }

    let missing_from_lock = package_edges
        .iter()
        .filter(|(key, _)| !lock_edges.contains_key(*key))
        .map(|(requirement, span)| DependencyContractEdge {
            requirement: requirement.clone(),
            span: span.clone(),
        })
        .collect::<Vec<_>>();
    let unexpected_in_lock = lock_edges
        .iter()
        .filter(|(key, _)| !package_edges.contains_key(*key))
        .map(|(requirement, span)| DependencyContractEdge {
            requirement: requirement.clone(),
            span: span.clone(),
        })
        .collect::<Vec<_>>();
    if !missing_from_lock.is_empty() || !unexpected_in_lock.is_empty() {
        output.mismatches.push(LockDependencyContractMismatch {
            kind: LockDependencyContractMismatchKind::Requirements {
                missing_from_lock,
                unexpected_in_lock,
            },
            package_span: package_header.span.clone(),
            lock_span: lock.span.clone(),
        });
    }

    Ok(output)
}

fn package_contract_edges(
    package: &PackageParseOutput,
    root_targets: &BTreeSet<Box<str>>,
) -> Result<BTreeMap<DependencyContractRequirement, Range<usize>>, LockDependencyContractInputError>
{
    let mut edges = BTreeMap::new();
    for declaration in &package.file().declarations {
        let PackageDeclaration::Requires { test, entries } = &declaration.value else {
            continue;
        };
        if *test {
            continue;
        }
        for entry in entries {
            if !is_included(&entry.value, root_targets) {
                continue;
            }
            let requirement = package_requirement_key(&entry.value, entry.span.clone())?;
            edges.insert(requirement.0, requirement.1);
        }
    }
    Ok(edges)
}

fn lock_contract_edges(
    lock: &LockPackage,
) -> Result<BTreeMap<DependencyContractRequirement, Range<usize>>, LockDependencyContractInputError>
{
    let mut edges = BTreeMap::new();
    for field in &lock.fields {
        let LockPackageField::Requires(requirement) = &field.value else {
            continue;
        };
        if requirement.test {
            continue;
        }
        let key = lock_requirement_key(requirement, field.span.clone())?;
        edges.insert(key.0, key.1);
    }
    Ok(edges)
}

fn package_requirement_key(
    requirement: &Requirement,
    span: Range<usize>,
) -> Result<(DependencyContractRequirement, Range<usize>), LockDependencyContractInputError> {
    Ok((
        DependencyContractRequirement {
            package: requirement.package.clone(),
            requirement: requirement.version.to_string().into(),
            only: requirement
                .only
                .as_ref()
                .map(|targets| sorted_set(&targets.value)),
            source: requirement
                .source
                .as_ref()
                .map(|source| {
                    contract_source(&source.value, source.span.clone(), SourceInputKind::Package)
                })
                .transpose()?,
            grant: requirement
                .grant
                .as_ref()
                .map(|grant| sorted_set(&grant.value)),
        },
        span,
    ))
}

fn lock_requirement_key(
    requirement: &LockRequirement,
    span: Range<usize>,
) -> Result<(DependencyContractRequirement, Range<usize>), LockDependencyContractInputError> {
    Ok((
        DependencyContractRequirement {
            package: requirement.package.clone(),
            requirement: requirement.requirement.to_string().into(),
            only: requirement.only.as_ref().map(|targets| sorted_set(targets)),
            source: requirement
                .source
                .as_ref()
                .map(|source| {
                    contract_source(&source.value, source.span.clone(), SourceInputKind::Lock)
                })
                .transpose()?,
            grant: requirement.grant.as_ref().map(|grant| sorted_set(grant)),
        },
        span,
    ))
}

#[derive(Clone, Copy)]
enum SourceInputKind {
    Package,
    Lock,
}

fn contract_source(
    source: &RequirementSource,
    span: Range<usize>,
    input_kind: SourceInputKind,
) -> Result<DependencyContractSource, LockDependencyContractInputError> {
    match source {
        RequirementSource::Git { url, directory } => {
            let canonical_url =
                crate::lock_validation::canonical_git_url(url).map_err(|()| match input_kind {
                    SourceInputKind::Package => {
                        LockDependencyContractInputError::InvalidPackageGitSourceUrl { span }
                    }
                    SourceInputKind::Lock => {
                        LockDependencyContractInputError::InvalidLockGitSourceUrl { span }
                    }
                })?;
            Ok(DependencyContractSource::Git {
                url: canonical_url.into(),
                directory: directory
                    .as_deref()
                    .filter(|directory| *directory != ".")
                    .map(Into::into),
            })
        }
        RequirementSource::Path(path) => Ok(DependencyContractSource::Path(path.clone())),
    }
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
    use std::collections::BTreeSet;

    use super::{LockDependencyContractInputError, LockDependencyContractMismatchKind, compare};
    use crate::lock::{LockPackage, LockPackageField, LockRequirement};
    use crate::manifest::{PackageRole, RequirementSource, Spanned, parse_for_role};
    use crate::version::Version;
    use crate::version_req::VersionReq;

    fn package(edges: &str) -> crate::manifest::PackageParseOutput {
        parse_for_role(
            format!(
                "package acme:dep @ 1.0.0\n\
                 source \"src\"\n\
                 targets {{ wasm32-core }}\n\
                 {edges}"
            )
            .as_bytes(),
            PackageRole::Dependency,
        )
    }

    fn root_targets(targets: &[&str]) -> BTreeSet<Box<str>> {
        targets.iter().map(|target| (*target).into()).collect()
    }

    fn requirement(
        package: &str,
        version: &str,
        selected: &str,
        test: bool,
        source: Option<RequirementSource>,
        only: Option<&[&str]>,
        grant: Option<&[&str]>,
    ) -> LockRequirement {
        LockRequirement {
            test,
            package: package.into(),
            requirement: VersionReq::parse(version).expect("valid requirement"),
            selected: Version::parse(selected).expect("valid version"),
            source: source.map(|value| Spanned {
                value,
                span: 10..20,
            }),
            only: only.map(|targets| {
                targets
                    .iter()
                    .map(|target| Spanned {
                        value: (*target).into(),
                        span: 20..21,
                    })
                    .collect()
            }),
            grant: grant.map(|capabilities| {
                capabilities
                    .iter()
                    .map(|capability| Spanned {
                        value: (*capability).into(),
                        span: 21..22,
                    })
                    .collect()
            }),
        }
    }

    fn lock(requirements: Vec<LockRequirement>) -> Spanned<LockPackage> {
        Spanned {
            value: LockPackage {
                package: "acme:dep".into(),
                version: Version::parse("1.0.0").expect("valid version"),
                fields: requirements
                    .into_iter()
                    .enumerate()
                    .map(|(index, requirement)| Spanned {
                        value: LockPackageField::Requires(Box::new(requirement)),
                        span: index..index + 1,
                    })
                    .collect(),
            },
            span: 100..200,
        }
    }

    fn path(path: &str) -> RequirementSource {
        RequirementSource::Path(path.into())
    }

    fn requirements_mismatch(
        output: &super::LockDependencyContractOutput,
    ) -> (
        &[super::DependencyContractEdge],
        &[super::DependencyContractEdge],
    ) {
        output
            .mismatches
            .iter()
            .find_map(|mismatch| match &mismatch.kind {
                LockDependencyContractMismatchKind::Requirements {
                    missing_from_lock,
                    unexpected_in_lock,
                } => Some((missing_from_lock.as_slice(), unexpected_in_lock.as_slice())),
                _ => None,
            })
            .expect("requirements mismatch")
    }

    #[test]
    fn matches_canonical_dependency_metadata_and_included_edges() {
        let package = package(
            "requires {\n\
                 acme:core @ >=1.2, <2 from git \"https://example.test/repo\" dir \"packages/core\" only { wasm32-core } grant { wasi:http, alloc }\n\
             }\n",
        );
        let lock = lock(vec![requirement(
            "acme:core",
            ">=1.2, <2",
            "1.5.0",
            false,
            Some(RequirementSource::Git {
                url: "https://example.test/repo".into(),
                directory: Some("packages/core".into()),
            }),
            Some(&["wasm32-core"]),
            Some(&["alloc", "wasi:http"]),
        )]);

        let output =
            compare(&package, &lock, &root_targets(&["wasm32-core"])).expect("valid inputs");
        assert!(output.matches_contract(), "{:?}", output.mismatches);
    }

    #[test]
    fn reports_missing_and_unexpected_edges() {
        let package = package(
            "requires {\n\
                 acme:expected @ ^1 from path \"../expected\"\n\
             }\n",
        );
        let lock = lock(vec![requirement(
            "acme:unexpected",
            "^2",
            "2.1.0",
            false,
            Some(path("../unexpected")),
            None,
            None,
        )]);

        let output =
            compare(&package, &lock, &root_targets(&["wasm32-core"])).expect("valid inputs");
        let (missing, unexpected) = requirements_mismatch(&output);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0].requirement.package.as_ref(), "acme:expected");
        assert_eq!(unexpected.len(), 1);
        assert_eq!(
            unexpected[0].requirement.package.as_ref(),
            "acme:unexpected"
        );
    }

    #[test]
    fn compares_original_source_clause_and_grant() {
        let package = package(
            "requires {\n\
                 acme:core @ ^1 from path \"../current\" grant { wasi:http }\n\
             }\n",
        );
        let lock = lock(vec![requirement(
            "acme:core",
            "^1",
            "1.4.0",
            false,
            Some(path("../locked")),
            None,
            Some(&["wasi:random"]),
        )]);

        let output =
            compare(&package, &lock, &root_targets(&["wasm32-core"])).expect("valid inputs");
        let (missing, unexpected) = requirements_mismatch(&output);
        assert_eq!(missing.len(), 1);
        assert_eq!(unexpected.len(), 1);
    }

    #[test]
    fn omitted_grant_differs_from_explicit_empty_grant() {
        let package = package(
            "requires {\n\
                 acme:core @ ^1 from path \"../core\"\n\
             }\n",
        );
        let lock = lock(vec![requirement(
            "acme:core",
            "^1",
            "1.4.0",
            false,
            Some(path("../core")),
            None,
            Some(&[]),
        )]);

        let output =
            compare(&package, &lock, &root_targets(&["wasm32-core"])).expect("valid inputs");
        assert_eq!(requirements_mismatch(&output).0.len(), 1);
        assert_eq!(requirements_mismatch(&output).1.len(), 1);
    }

    #[test]
    fn ignores_test_edges() {
        let package = package(
            "requires {\n\
                 acme:core @ ^1 from path \"../core\"\n\
             }\n\
             requires test { acme:test @ ^1 from path \"../test\" }\n",
        );
        let lock = lock(vec![
            requirement(
                "acme:core",
                "^1",
                "1.4.0",
                false,
                Some(path("../core")),
                None,
                None,
            ),
            requirement(
                "acme:lock-test",
                "^1",
                "1.1.0",
                true,
                Some(path("../lock-test")),
                None,
                None,
            ),
        ]);

        let output =
            compare(&package, &lock, &root_targets(&["wasm32-core"])).expect("valid inputs");
        assert!(output.matches_contract(), "{:?}", output.mismatches);
    }

    #[test]
    fn filters_current_edges_but_rejects_excluded_edges_recorded_in_lock() {
        let package = parse_for_role(
            b"package acme:dep @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core, wasm32-wasi }\n\
              requires { acme:stale @ ^1 from path \"../stale\" only { wasm32-core } }\n",
            PackageRole::Dependency,
        );
        let lock = lock(vec![requirement(
            "acme:stale",
            "^1",
            "1.1.0",
            false,
            Some(path("../stale")),
            Some(&["wasm32-core"]),
            None,
        )]);

        // The current edge is excluded for a wasi-only root. A lock that
        // records that excluded edge is still stale and must not be accepted.
        let output =
            compare(&package, &lock, &root_targets(&["wasm32-wasi"])).expect("valid inputs");
        let (missing, unexpected) = requirements_mismatch(&output);
        assert!(missing.is_empty());
        assert_eq!(unexpected.len(), 1);
        assert_eq!(unexpected[0].requirement.package.as_ref(), "acme:stale");
    }

    #[test]
    fn excludes_current_edges_that_do_not_apply_to_root_targets() {
        let package = parse_for_role(
            b"package acme:dep @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core, wasm32-wasi }\n\
              requires { acme:core-only @ ^1 from path \"../core\" only { wasm32-core } }\n",
            PackageRole::Dependency,
        );
        let output = compare(&package, &lock(Vec::new()), &root_targets(&["wasm32-wasi"]))
            .expect("valid inputs");
        assert!(output.matches_contract(), "{:?}", output.mismatches);
    }

    #[test]
    fn reports_identity_and_version_mismatch() {
        let package = package("requires { acme:core @ ^1 from path \"../core\" }\n");
        let mut lock = lock(vec![requirement(
            "acme:core",
            "^1",
            "1.4.0",
            false,
            Some(path("../core")),
            None,
            None,
        )]);
        lock.value.package = "acme:other".into();
        lock.value.version = Version::parse("2.0.0").expect("valid version");

        let output =
            compare(&package, &lock, &root_targets(&["wasm32-core"])).expect("valid inputs");
        assert!(output.mismatches.iter().any(|mismatch| matches!(
            mismatch.kind,
            LockDependencyContractMismatchKind::Identity { .. }
        )));
        assert!(output.mismatches.iter().any(|mismatch| matches!(
            mismatch.kind,
            LockDependencyContractMismatchKind::Version { .. }
        )));
    }

    #[test]
    fn resolved_version_is_left_to_condition_four() {
        let package = package("requires { acme:core @ ^1 from path \"../core\" }\n");
        let lock = lock(vec![requirement(
            "acme:core",
            "^1",
            "1.9.9",
            false,
            Some(path("../core")),
            None,
            None,
        )]);

        let output =
            compare(&package, &lock, &root_targets(&["wasm32-core"])).expect("valid inputs");
        assert!(output.matches_contract(), "{:?}", output.mismatches);
    }

    #[test]
    fn rejects_invalid_current_manifest_syntax_and_semantics() {
        let syntax = parse_for_role(
            b"package acme:dep @ 1.0.0\nsource \"src\"\ntargets { wasm32-core }\nrequires {",
            PackageRole::Dependency,
        );
        assert!(matches!(
            compare(&syntax, &lock(Vec::new()), &root_targets(&["wasm32-core"])),
            Err(LockDependencyContractInputError::InvalidPackageSyntax { .. })
        ));

        let semantics = package("requires { acme:missing-source @ ^1 }\n");
        assert!(matches!(
            compare(
                &semantics,
                &lock(Vec::new()),
                &root_targets(&["wasm32-core"]),
            ),
            Err(LockDependencyContractInputError::InvalidPackageSemantics(_))
        ));
    }

    #[test]
    fn rejects_an_invalid_current_git_source_url() {
        let package = package("requires { acme:core @ ^1 from git \"ftp://example.test/core\" }\n");
        let error = compare(&package, &lock(Vec::new()), &root_targets(&["wasm32-core"]))
            .expect_err("URL must be supported");
        assert!(matches!(
            error,
            LockDependencyContractInputError::InvalidPackageGitSourceUrl { .. }
        ));
    }
}
