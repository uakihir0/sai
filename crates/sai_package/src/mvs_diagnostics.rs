//! Diagnostics produced from resolved package-graph facts.

use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;

use crate::git_remote::GitRemoteReleaseContractError;
use crate::lexer::PackageLexErrorKind;
use crate::local_source::{LocalPackageContractLoadError, LocalPackagePathIndexError};
use crate::manifest::{
    OverrideSource, PackageDeclaration, PackageParseError, PackageParseErrorKind, PackageRole,
    RequirementSource, parse_for_role,
};
use crate::mixed_source::{
    MixedPackageSourceIdentity, MixedSourceFinalAuditPlan, MixedSourceFinalIdentityError,
    MixedSourceFinalReloadError, MixedSourceMvsOutput,
};
use crate::mvs::{
    MvsPackageVersion, MvsRequirer, MvsSelectionError, MvsSelectionErrorKind, MvsSelectionOutput,
};
use crate::mvs_explanations::{
    MvsChainLink, MvsChainOrigin, MvsConflictExplanation, MvsExplanationError, MvsExplanationGraph,
    MvsRequirementChain, MvsSelectionReason,
};
use crate::mvs_graph_validation::{
    MvsDependencyCycle, MvsDuplicatePackageLines, MvsSelectedGraphInputError, MvsVersionConflict,
    check_selected_graph, duplicate_package_lines,
};
use crate::resolver_contract::{
    ResolverContractBuildError, ResolverContractComparisonError, ResolverContractEdge,
    ResolverContractSourceClause,
};
use crate::resolver_source::{
    ResolverPackageSource, ResolverSourceProvenance, ResolverSourceReason, ResolverSourceSelection,
};
use crate::resolver_source_provider::{
    ResolverFinalContractAudit, ResolverFinalContractAuditFailure,
    ResolverFinalContractAuditFailureKind, ResolverFinalContractPathResolutionError,
    ResolverReloadedContractComparison, ResolverSourceIdentityFailure,
    ResolverSourceIdentityFailureKind, ResolverSourceIdentitySide,
};
use crate::validation::{
    PackageValidationError, PackageValidationErrorKind, resolve_package_lint_settings_from_parsed,
};
use crate::version::Version;
use crate::version_req::VersionReq;
use sai_diagnostics::{
    ChainStep, Child, ChildKind, Code, Diagnostic, LintPolicies, LintResolution,
    LintResolutionContext, LintResolutionError, ManifestLintLevel, OverrideLintLevel, Related,
    Relation, Severity, SourceRef, TextRange, TextSize, Value, ValueObject,
    resolve_lint_occurrence,
};

const MAX_FINAL_MANIFEST_SYNTAX_ISSUES: usize = 32;
const MAX_FINAL_MANIFEST_SYNTAX_CANDIDATES: usize = (MAX_FINAL_MANIFEST_SYNTAX_ISSUES + 1) * 3;
const MAX_FINAL_MANIFEST_SEMANTIC_ISSUES: usize = 32;

/// Source identity for one parsed package manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsManifestLocation {
    source: SourceRef,
    file_id: sai_diagnostics::FileId,
    package_version: MvsPackageVersion,
    role: PackageRole,
    workspace_snapshot: sai_diagnostics::WorkspaceSnapshotId,
    manifest_digest: [u8; 32],
    source_len: usize,
    final_package_header: Option<FinalPackageHeader>,
    final_manifest_syntax_issues: Vec<FinalManifestSyntaxIssue>,
    final_manifest_syntax_issues_truncated: bool,
    requirements: BTreeMap<(usize, usize), ManifestRequirement>,
    overrides: BTreeMap<(usize, usize), ManifestOverride>,
    lint_policies: LintPolicies,
    manifest_lint_levels: BTreeMap<String, ManifestLintLevel>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestRequirement {
    package: Box<str>,
    version: VersionReq,
    source: Option<RequirementSource>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestOverride {
    package: Box<str>,
    source: OverrideSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FinalPackageHeader {
    package: Box<str>,
    version: Version,
    span: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FinalManifestSyntaxIssue {
    code: Code,
    message: String,
    span: Range<usize>,
    expected_tokens: Option<Vec<String>>,
}

fn push_bounded_final_manifest_syntax_issue(
    issues: &mut Vec<FinalManifestSyntaxIssue>,
    truncated: &mut bool,
    issue: FinalManifestSyntaxIssue,
) {
    if issues.contains(&issue) {
        return;
    }
    if issues.len() == MAX_FINAL_MANIFEST_SYNTAX_CANDIDATES {
        *truncated = true;
    } else {
        issues.push(issue);
    }
}

fn expected_tokens_from_description(description: &str) -> Option<Vec<String>> {
    let alternatives = description
        .split('`')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, token)| token.to_owned())
        .collect::<Vec<_>>();
    if !alternatives.is_empty() {
        return Some(alternatives);
    }

    let alternatives = match description {
        "version comparator" => vec!["^", "~", "=", ">=", ">", "<=", "<"],
        "version after comparator" => vec!["INTEGER", "VERSION"],
        "package identity" | "package path" => vec!["PACKAGE_PATH"],
        "full package version" => vec!["VERSION"],
        "size integer" => vec!["INTEGER"],
        "size unit" | "lint code" => vec!["UPPER_WORD"],
        "path string" | "repository directory" | "string" => vec!["STRING"],
        "capability name" => vec!["WORD", "PACKAGE_PATH"],
        "word" => vec!["WORD"],
        "at least one list item" => vec!["LIST_ITEM"],
        _ => return None,
    };
    Some(alternatives.into_iter().map(str::to_owned).collect())
}

fn final_manifest_semantic_diagnostic(error: &PackageValidationErrorKind) -> (Code, &'static str) {
    let (code, message) = match error {
        PackageValidationErrorKind::MissingTargets => {
            ("MISSING_TARGETS", "package has no targets declaration")
        }
        PackageValidationErrorKind::PackageHasNoContent => (
            "PACKAGE_HAS_NO_CONTENT",
            "package declares neither a source directory nor an artifact",
        ),
        PackageValidationErrorKind::UnknownTarget(_) => ("UNKNOWN_TARGET", "unknown target name"),
        PackageValidationErrorKind::OnlyTargetNotDeclared(_) => (
            "ONLY_TARGET_NOT_DECLARED",
            "dependency is restricted to a target not declared by this package",
        ),
        PackageValidationErrorKind::ArtifactTargetNotDeclared(_) => (
            "ARTIFACT_TARGET_NOT_DECLARED",
            "artifact names a target not declared by this package",
        ),
        PackageValidationErrorKind::ArtifactOnCoreTarget(_) => (
            "ARTIFACT_ON_CORE_TARGET",
            "artifact target must be a component target",
        ),
        PackageValidationErrorKind::MissingArtifactForTarget(_) => (
            "MISSING_ARTIFACT_FOR_TARGET",
            "package has no artifact for one of its declared targets",
        ),
        PackageValidationErrorKind::WitRequired => {
            ("WIT_REQUIRED", "artifact packages require a WIT directory")
        }
        PackageValidationErrorKind::MissingExportWorld => (
            "MISSING_EXPORT_WORLD",
            "artifact packages require an exported world",
        ),
        PackageValidationErrorKind::InvalidWorldName(_) => (
            "INVALID_WORLD_NAME",
            "exported world name is invalid for this package",
        ),
        PackageValidationErrorKind::MissingSource(_) => (
            "MISSING_SOURCE",
            "dependency requirement does not declare a source",
        ),
        PackageValidationErrorKind::InvalidPackageName(_) => {
            ("INVALID_PACKAGE_NAME", "invalid package identity")
        }
        PackageValidationErrorKind::InvalidDescription => (
            "INVALID_DESCRIPTION",
            "description must be a single line of at most 1024 UTF-8 bytes",
        ),
        PackageValidationErrorKind::ImplicitPackageRequired(_) => (
            "IMPLICIT_PACKAGE_REQUIRED",
            "implicit runtime packages cannot be used directly here",
        ),
        PackageValidationErrorKind::PackageRequiresItself(_) => {
            ("PACKAGE_REQUIRES_ITSELF", "package cannot require itself")
        }
        PackageValidationErrorKind::DuplicateRequirement(_) => (
            "DUPLICATE_REQUIREMENT",
            "package is required more than once",
        ),
        PackageValidationErrorKind::InvalidCapabilityName(_) => {
            ("INVALID_CAPABILITY_NAME", "invalid capability name")
        }
        PackageValidationErrorKind::CapabilityAllowDenyConflict(_) => (
            "CAPABILITY_ALLOW_DENY_CONFLICT",
            "capability appears in both allow and deny declarations",
        ),
        PackageValidationErrorKind::UnknownPolicyItem(_) => {
            ("UNKNOWN_POLICY_ITEM", "unknown package policy item")
        }
        PackageValidationErrorKind::PolicyItemIsCapability(_) => (
            "POLICY_ITEM_IS_CAPABILITY",
            "capability name cannot be used as a policy item",
        ),
        PackageValidationErrorKind::UnknownLint(_) => ("UNKNOWN_LINT", "unknown lint code"),
        PackageValidationErrorKind::NotALint(_) => (
            "NOT_A_LINT",
            "hard diagnostic codes cannot be used as lints",
        ),
        PackageValidationErrorKind::UnknownProfile(_) => {
            ("UNKNOWN_PROFILE", "unknown build profile")
        }
        PackageValidationErrorKind::ProfileAllowExceedsPackage(_) => (
            "PROFILE_ALLOW_EXCEEDS_PACKAGE",
            "profile allows a capability not allowed by the package",
        ),
        PackageValidationErrorKind::UnknownBudgetMetric(_) => {
            ("UNKNOWN_BUDGET_METRIC", "unknown size budget metric")
        }
        PackageValidationErrorKind::InvalidSize => {
            ("INVALID_SIZE", "invalid size budget amount or unit")
        }
        PackageValidationErrorKind::AllocatorNotRequired(_) => (
            "ALLOCATOR_NOT_REQUIRED",
            "allocator package is not required by the root package",
        ),
        PackageValidationErrorKind::DuplicateOverride { kind, .. } => match kind {
            crate::validation::OverrideKind::Source => (
                "DUPLICATE_OVERRIDE",
                "package has more than one source override for this identity",
            ),
            crate::validation::OverrideKind::Artifact => (
                "DUPLICATE_OVERRIDE",
                "package has more than one artifact override for this identity",
            ),
        },
        PackageValidationErrorKind::CompilerVersionMismatch { .. } => (
            "COMPILER_VERSION_MISMATCH",
            "package compiler requirement does not match the current compiler",
        ),
    };
    (
        Code::from_name(code).expect("package validation diagnostic code is registered"),
        message,
    )
}

fn final_manifest_semantic_validation_errors(
    error: &MixedSourceFinalReloadError,
) -> Option<&[PackageValidationError]> {
    let build_error = match error {
        MixedSourceFinalReloadError::LocalContract(error) => match error.as_ref() {
            LocalPackageContractLoadError::InvalidContract(error) => error,
            LocalPackageContractLoadError::PackageVersionMismatch { .. } => return None,
        },
        MixedSourceFinalReloadError::GitContract(error) => match error.as_ref() {
            GitRemoteReleaseContractError::InvalidContract(error) => error,
            GitRemoteReleaseContractError::ReleaseTagVersionMismatch { .. }
            | GitRemoteReleaseContractError::PackageVersionMismatch { .. } => return None,
        },
        _ => return None,
    };
    match build_error {
        ResolverContractBuildError::SemanticValidationErrors(errors) => Some(errors),
        _ => None,
    }
}

fn final_manifest_syntax_issues(
    parsed: &crate::manifest::PackageParseOutput,
    invalid_utf8_spans: &[Range<usize>],
    invalid_utf8_truncated: bool,
) -> (Vec<FinalManifestSyntaxIssue>, bool) {
    let issue_limit = MAX_FINAL_MANIFEST_SYNTAX_ISSUES + 1;
    let mut issues = Vec::with_capacity(MAX_FINAL_MANIFEST_SYNTAX_CANDIDATES);
    let mut truncated = false;
    let lexical_issue_count = parsed
        .lexical_errors()
        .iter()
        .filter(|error| error.kind != PackageLexErrorKind::InvalidUtf8)
        .count();
    truncated |= lexical_issue_count > issue_limit;
    for error in parsed
        .lexical_errors()
        .iter()
        .filter(|error| error.kind != PackageLexErrorKind::InvalidUtf8)
        .take(issue_limit)
    {
        let (code, message) = match error.kind {
            PackageLexErrorKind::InvalidUtf8 => unreachable!("invalid UTF-8 uses VFS spans"),
            PackageLexErrorKind::InvalidLineEnding => {
                ("INVALID_LINE_ENDING", "lone carriage return".to_owned())
            }
            PackageLexErrorKind::ForbiddenCharacter => (
                "FORBIDDEN_CHARACTER",
                "forbidden character in manifest".to_owned(),
            ),
            PackageLexErrorKind::UnexpectedCharacter => (
                "UNEXPECTED_CHARACTER",
                "unexpected character in manifest".to_owned(),
            ),
            PackageLexErrorKind::UnterminatedComment => {
                ("UNTERMINATED_COMMENT", "unterminated comment".to_owned())
            }
            PackageLexErrorKind::UnterminatedString => {
                ("UNTERMINATED_STRING", "unterminated string".to_owned())
            }
            PackageLexErrorKind::InvalidEscape => {
                ("INVALID_ESCAPE", "invalid string escape".to_owned())
            }
            PackageLexErrorKind::UnsupportedStringForm => (
                "UNSUPPORTED_STRING_FORM",
                "unsupported string form in manifest".to_owned(),
            ),
            PackageLexErrorKind::InvalidVersion => {
                ("INVALID_VERSION", "invalid package version".to_owned())
            }
            PackageLexErrorKind::BuildMetadataNotAllowed => (
                "BUILD_METADATA_NOT_ALLOWED",
                "build metadata is not allowed in a package version".to_owned(),
            ),
            PackageLexErrorKind::NoSemicolon => (
                "NO_SEMICOLON",
                "semicolons are not allowed in manifests".to_owned(),
            ),
        };
        push_bounded_final_manifest_syntax_issue(
            &mut issues,
            &mut truncated,
            FinalManifestSyntaxIssue {
                code: Code::from_name(code).expect("manifest syntax diagnostic code is registered"),
                message,
                span: error.span.clone(),
                expected_tokens: None,
            },
        );
    }
    truncated |= invalid_utf8_truncated;
    for span in invalid_utf8_spans.iter().take(issue_limit) {
        push_bounded_final_manifest_syntax_issue(
            &mut issues,
            &mut truncated,
            FinalManifestSyntaxIssue {
                code: Code::InvalidUtf8,
                message: "manifest is not valid UTF-8".to_owned(),
                span: span.clone(),
                expected_tokens: None,
            },
        );
    }
    truncated |= parsed.errors().len() > issue_limit;
    for error in parsed.errors().iter().take(issue_limit) {
        let (code, message, expected_tokens) = match &error.kind {
            PackageParseErrorKind::MissingPackageDeclaration => (
                "MISSING_PACKAGE_DECL",
                "missing `package` declaration".to_owned(),
                None,
            ),
            PackageParseErrorKind::PackageDeclarationNotFirst => (
                "PACKAGE_DECL_NOT_FIRST",
                "`package` declaration must be first".to_owned(),
                None,
            ),
            PackageParseErrorKind::DuplicateDeclaration(name) => (
                "DUPLICATE_DECLARATION",
                format!("duplicate `{name}` declaration"),
                None,
            ),
            PackageParseErrorKind::UnknownDeclaration { name, .. } => (
                "UNKNOWN_DECLARATION",
                format!("unknown package declaration `{name}`"),
                None,
            ),
            PackageParseErrorKind::UnknownArtifactKind(kind) => (
                "UNKNOWN_ARTIFACT_KIND",
                format!("unknown artifact kind `{kind}`"),
                None,
            ),
            PackageParseErrorKind::ExpectedToken(token)
                if token.as_ref() == "newline between declarations" =>
            {
                (
                    "EXPECTED_TOKEN",
                    "expected a newline between declarations".to_owned(),
                    Some(vec!["NEWLINE".to_owned()]),
                )
            }
            PackageParseErrorKind::ExpectedToken(token) => (
                "EXPECTED_TOKEN",
                format!("expected {token}"),
                expected_tokens_from_description(token),
            ),
            PackageParseErrorKind::ExpectedListSeparator(expected_tokens) => (
                "EXPECTED_TOKEN",
                "expected a list separator".to_owned(),
                Some(expected_tokens.iter().map(ToString::to_string).collect()),
            ),
            PackageParseErrorKind::ExpectedDeclaration => (
                "EXPECTED_TOKEN",
                "expected a package declaration".to_owned(),
                Some(
                    [
                        "package",
                        "compiler",
                        "source",
                        "wit",
                        "artifact",
                        "targets",
                        "export",
                        "requires",
                        "allow",
                        "deny",
                        "policy",
                        "lints",
                        "profile",
                        "allocator",
                        "overrides",
                        "description",
                        "license",
                        "authors",
                        "repository",
                    ]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                ),
            ),
            PackageParseErrorKind::ExpectedProfileItem => (
                "EXPECTED_TOKEN",
                "expected a profile item".to_owned(),
                Some(vec!["allow".to_owned(), "budget".to_owned()]),
            ),
            PackageParseErrorKind::InvalidPackageIdentity(_) => (
                "INVALID_PACKAGE_NAME",
                "invalid package identity".to_owned(),
                None,
            ),
            PackageParseErrorKind::InvalidPackageVersion(_) => (
                "INVALID_VERSION",
                "invalid package version".to_owned(),
                None,
            ),
            PackageParseErrorKind::InvalidContentPath(_)
            | PackageParseErrorKind::InvalidDependencyPath(_)
            | PackageParseErrorKind::InvalidRepositoryDirectory(_) => (
                "INVALID_PACKAGE_PATH",
                "invalid package path".to_owned(),
                None,
            ),
            PackageParseErrorKind::ConflictingLintLevel => (
                "CONFLICTING_LINT_LEVEL",
                "conflicting lint levels".to_owned(),
                None,
            ),
            PackageParseErrorKind::InvalidVersionRequirement(_) => (
                "INVALID_VERSION_REQUIREMENT",
                "invalid version requirement".to_owned(),
                None,
            ),
            PackageParseErrorKind::ManifestTooLarge => (
                "PACKAGE_TOO_LARGE",
                "package manifest exceeds the supported size limit".to_owned(),
                None,
            ),
        };
        push_bounded_final_manifest_syntax_issue(
            &mut issues,
            &mut truncated,
            FinalManifestSyntaxIssue {
                code: Code::from_name(code).expect("manifest syntax diagnostic code is registered"),
                message,
                span: error.span.clone(),
                expected_tokens,
            },
        );
    }
    issues.sort_by(|left, right| {
        left.span
            .start
            .cmp(&right.span.start)
            .then_with(|| left.span.end.cmp(&right.span.end))
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| left.message.cmp(&right.message))
            .then_with(|| left.expected_tokens.cmp(&right.expected_tokens))
    });
    issues.dedup();
    if issues.len() > MAX_FINAL_MANIFEST_SYNTAX_ISSUES {
        issues.truncate(MAX_FINAL_MANIFEST_SYNTAX_ISSUES);
        truncated = true;
    }
    (issues, truncated)
}

impl MvsManifestLocation {
    /// Binds a manifest location to one syntax-valid file in a workspace snapshot.
    ///
    /// Package identity, version, source path, snapshot, file ID, and
    /// requirement spans are taken from the snapshot and parsed source. The
    /// caller cannot provide an unrelated source reference or span table.
    pub fn from_snapshot(
        snapshot: &sai_diagnostics::WorkspaceSnapshot,
        file_id: sai_diagnostics::FileId,
        package_version: &MvsPackageVersion,
        role: PackageRole,
    ) -> Result<Self, MvsDuplicatePackageLineDiagnosticError> {
        let snapshot_file = snapshot.file(file_id).ok_or(
            MvsDuplicatePackageLineDiagnosticError::MissingSnapshotFile(file_id),
        )?;
        let source_file = snapshot_file.source();
        if source_file.id() != file_id {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::SnapshotFileIdMismatch {
                    expected: file_id,
                    found: source_file.id(),
                },
            );
        }

        let expected_path = manifest_path_expectation(package_version, role);
        if !is_manifest_path(snapshot_file.path(), package_version, role) {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::ManifestPathMismatch {
                    file_id,
                    expected: expected_path,
                    found: snapshot_file.path().to_owned(),
                },
            );
        }

        let expected_package = package_version_name(package_version);
        let found_package = snapshot_file.package().map(str::to_owned);
        if found_package.as_deref() != Some(expected_package.as_str()) {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::ManifestPackageMismatch {
                    requirer: Box::new(MvsRequirer::Package(package_version.clone())),
                    expected: expected_package,
                    found: found_package,
                },
            );
        }

        let parsed = parse_for_role(source_file.raw_bytes(), role);
        if !parsed.is_syntax_valid() {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::InvalidManifestSyntax {
                    file_id,
                    lexical_errors: parsed.lexical_errors().to_vec(),
                    parse_errors: parsed.errors().to_vec(),
                },
            );
        }
        let found_header = parsed
            .file()
            .header
            .as_ref()
            .map(|header| format!("{}@{}", header.value.identity, header.value.version));
        if found_header.as_deref() != Some(expected_package.as_str()) {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::ManifestHeaderMismatch {
                    file_id,
                    expected: Box::new(package_version.clone()),
                    found: found_header,
                },
            );
        }

        let mut requirements = BTreeMap::new();
        let mut overrides = BTreeMap::new();
        for declaration in &parsed.file().declarations {
            match &declaration.value {
                PackageDeclaration::Requires { test, entries } => {
                    if *test && role == PackageRole::Dependency {
                        continue;
                    }
                    for entry in entries {
                        let key = (entry.span.start, entry.span.end);
                        requirements.insert(
                            key,
                            ManifestRequirement {
                                package: entry.value.package.clone(),
                                version: entry.value.version.clone(),
                                source: entry
                                    .value
                                    .source
                                    .as_ref()
                                    .map(|source| source.value.clone()),
                            },
                        );
                    }
                }
                PackageDeclaration::Overrides(entries) => {
                    for entry in entries {
                        let key = (entry.span.start, entry.span.end);
                        overrides.insert(
                            key,
                            ManifestOverride {
                                package: entry.value.package.clone(),
                                source: entry.value.source.clone(),
                            },
                        );
                    }
                }
                _ => {}
            }
        }

        let source = SourceRef {
            path: snapshot_file.path().to_owned(),
            package: found_package.clone(),
            snapshot: snapshot_file.document_version(),
        };
        let (lint_policies, manifest_lint_levels) =
            resolve_package_lint_settings_from_parsed(&parsed, &source, file_id).map_err(
                |error| MvsDuplicatePackageLineDiagnosticError::LintConfiguration(Box::new(error)),
            )?;

        Ok(Self {
            source,
            file_id,
            package_version: package_version.clone(),
            role,
            workspace_snapshot: snapshot.id(),
            manifest_digest: *parsed
                .manifest_digest()
                .expect("syntax-valid manifest is within the parser size limit"),
            source_len: source_file.raw_bytes().len(),
            final_package_header: None,
            final_manifest_syntax_issues: Vec::new(),
            final_manifest_syntax_issues_truncated: false,
            requirements,
            overrides,
            lint_policies,
            manifest_lint_levels,
        })
    }

    /// Binds a final-source manifest to a synthetic, snapshot-owned location.
    ///
    /// Unlike a read-time MVS manifest, a final source may have changed its
    /// header or contain syntax errors. The caller supplies the exact retained
    /// bytes; this constructor records the changed header and syntax issues,
    /// and records path-requirement spans only when parsing succeeds.
    pub fn from_final_snapshot(
        snapshot: &sai_diagnostics::WorkspaceSnapshot,
        file_id: sai_diagnostics::FileId,
        package_version: &MvsPackageVersion,
    ) -> Result<Self, MvsDuplicatePackageLineDiagnosticError> {
        let snapshot_file = snapshot.file(file_id).ok_or(
            MvsDuplicatePackageLineDiagnosticError::MissingSnapshotFile(file_id),
        )?;
        let source_file = snapshot_file.source();
        if source_file.id() != file_id {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::SnapshotFileIdMismatch {
                    expected: file_id,
                    found: source_file.id(),
                },
            );
        }

        if !is_final_manifest_path(snapshot_file.path(), package_version) {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::ManifestPathMismatch {
                    file_id,
                    expected: format!(
                        "dep:{}/.sai/final-source-<ordinal>/sai.package",
                        package_version_name(package_version)
                    ),
                    found: snapshot_file.path().to_owned(),
                },
            );
        }

        let expected_package = package_version_name(package_version);
        let found_package = snapshot_file.package().map(str::to_owned);
        if found_package.as_deref() != Some(expected_package.as_str()) {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::ManifestPackageMismatch {
                    requirer: Box::new(MvsRequirer::Package(package_version.clone())),
                    expected: expected_package,
                    found: found_package,
                },
            );
        }

        let parsed = parse_for_role(source_file.raw_bytes(), PackageRole::Dependency);
        let manifest_digest = *parsed
            .manifest_digest()
            .ok_or(MvsDuplicatePackageLineDiagnosticError::FinalManifestTooLarge(file_id))?;
        let final_package_header = parsed
            .file()
            .header
            .as_ref()
            .map(|header| FinalPackageHeader {
                package: header.value.identity.to_string().into(),
                version: header.value.version.clone(),
                span: header.span.clone(),
            });
        let issue_limit = MAX_FINAL_MANIFEST_SYNTAX_ISSUES + 1;
        let invalid_utf8_sequences = source_file.invalid_utf8_sequences();
        let invalid_utf8_truncated = invalid_utf8_sequences.len() > issue_limit;
        let invalid_utf8_spans = invalid_utf8_sequences
            .iter()
            .take(issue_limit)
            .filter_map(|sequence| source_file.to_original_range(sequence.decoded_range()))
            .map(|span| span.start().to_usize()..span.end().to_usize())
            .collect::<Vec<_>>();
        let (final_manifest_syntax_issues, final_manifest_syntax_issues_truncated) =
            final_manifest_syntax_issues(&parsed, &invalid_utf8_spans, invalid_utf8_truncated);
        let mut requirements = BTreeMap::new();
        if parsed.is_syntax_valid() {
            for declaration in &parsed.file().declarations {
                let PackageDeclaration::Requires { test, entries } = &declaration.value else {
                    continue;
                };
                if *test {
                    continue;
                }
                for entry in entries {
                    requirements.insert(
                        (entry.span.start, entry.span.end),
                        ManifestRequirement {
                            package: entry.value.package.clone(),
                            version: entry.value.version.clone(),
                            source: entry
                                .value
                                .source
                                .as_ref()
                                .map(|source| source.value.clone()),
                        },
                    );
                }
            }
        }

        Ok(Self {
            source: SourceRef {
                path: snapshot_file.path().to_owned(),
                package: found_package,
                snapshot: snapshot_file.document_version(),
            },
            file_id,
            package_version: package_version.clone(),
            role: PackageRole::Dependency,
            workspace_snapshot: snapshot.id(),
            manifest_digest,
            source_len: source_file.raw_bytes().len(),
            final_package_header,
            final_manifest_syntax_issues,
            final_manifest_syntax_issues_truncated,
            requirements,
            overrides: BTreeMap::new(),
            lint_policies: LintPolicies::default(),
            manifest_lint_levels: BTreeMap::new(),
        })
    }

    /// Returns the canonical synthetic path used for a final-source manifest.
    pub fn final_snapshot_path(package_version: &MvsPackageVersion, ordinal: u64) -> String {
        format!(
            "dep:{}/.sai/final-source-{ordinal}/sai.package",
            package_version_name(package_version)
        )
    }

    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    /// SHA-256 digest of the exact manifest bytes in the workspace snapshot.
    pub const fn manifest_digest(&self) -> &[u8; 32] {
        &self.manifest_digest
    }

    pub const fn file_id(&self) -> sai_diagnostics::FileId {
        self.file_id
    }

    pub fn lint_policies(&self) -> &LintPolicies {
        &self.lint_policies
    }

    pub fn manifest_lint_levels(&self) -> &BTreeMap<String, ManifestLintLevel> {
        &self.manifest_lint_levels
    }
}

/// Manifest locations needed to show MVS requirement chains.
///
/// `packages` includes every contract visited during selection, including
/// candidate versions that were not selected. `root` is separate because the
/// root package is not a dependency entry and can share an identity/version
/// spelling with an entry from another source.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MvsManifestLocations {
    root: Option<MvsManifestLocation>,
    packages: BTreeMap<MvsPackageVersion, MvsManifestLocation>,
    final_packages: BTreeMap<MvsPackageVersion, MvsManifestLocation>,
    workspace_snapshot: Option<sai_diagnostics::WorkspaceSnapshotId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct FinalPathRequirement<'a> {
    requirement_package: &'a str,
    path: &'a str,
    requiring_package: &'a MvsPackageVersion,
    span: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ReportableFinalReloadFailure<'a> {
    package_version: &'a MvsPackageVersion,
    code: Code,
    detail: String,
    final_path_requirement: Option<FinalPathRequirement<'a>>,
    final_package_identity_mismatch: Option<MvsPackageVersion>,
}

impl MvsManifestLocations {
    pub fn set_root(
        &mut self,
        manifest: MvsManifestLocation,
    ) -> Result<(), MvsDuplicatePackageLineDiagnosticError> {
        if manifest.role != PackageRole::Root {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::ManifestRoleMismatch {
                    expected: PackageRole::Root,
                    found: manifest.role,
                },
            );
        }
        if self.root.is_some() {
            return Err(MvsDuplicatePackageLineDiagnosticError::DuplicateRootManifest);
        }
        self.register_snapshot(&manifest)?;
        self.root = Some(manifest);
        Ok(())
    }

    pub fn insert_package(
        &mut self,
        manifest: MvsManifestLocation,
    ) -> Result<(), MvsDuplicatePackageLineDiagnosticError> {
        if manifest.role != PackageRole::Dependency {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::ManifestRoleMismatch {
                    expected: PackageRole::Dependency,
                    found: manifest.role,
                },
            );
        }
        if self.packages.contains_key(&manifest.package_version) {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::DuplicatePackageManifest(Box::new(
                    manifest.package_version.clone(),
                )),
            );
        }
        self.register_snapshot(&manifest)?;
        self.packages
            .insert(manifest.package_version.clone(), manifest);
        Ok(())
    }

    /// Adds one changed final-source manifest without replacing its read-time
    /// MVS manifest, which remains the authority for requirement chains.
    pub fn insert_final_package(
        &mut self,
        manifest: MvsManifestLocation,
    ) -> Result<(), MvsDuplicatePackageLineDiagnosticError> {
        if manifest.role != PackageRole::Dependency {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::ManifestRoleMismatch {
                    expected: PackageRole::Dependency,
                    found: manifest.role,
                },
            );
        }
        if self.final_packages.contains_key(&manifest.package_version) {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::DuplicateFinalPackageManifest(Box::new(
                    manifest.package_version.clone(),
                )),
            );
        }
        self.register_snapshot(&manifest)?;
        self.final_packages
            .insert(manifest.package_version.clone(), manifest);
        Ok(())
    }

    pub fn root(&self) -> Option<&MvsManifestLocation> {
        self.root.as_ref()
    }

    pub fn packages(&self) -> &BTreeMap<MvsPackageVersion, MvsManifestLocation> {
        &self.packages
    }

    pub fn final_package(
        &self,
        package_version: &MvsPackageVersion,
    ) -> Option<&MvsManifestLocation> {
        self.final_packages.get(package_version)
    }

    /// Returns a final-source edge location only when the retained manifest
    /// has the exact requirement and path recorded by the reload failure.
    pub fn final_path_requirement_location(
        &self,
        requiring_package: &MvsPackageVersion,
        requirement_package: &str,
        path: &str,
        span: &Range<usize>,
    ) -> Result<sai_diagnostics::Location, MvsDuplicatePackageLineDiagnosticError> {
        let manifest = self.final_packages.get(requiring_package).ok_or_else(|| {
            MvsDuplicatePackageLineDiagnosticError::MissingFinalManifestLocation(Box::new(
                requiring_package.clone(),
            ))
        })?;
        let Some(requirement) = manifest.requirements.get(&(span.start, span.end)) else {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::FinalManifestRequirementMismatch {
                    requiring_package: Box::new(requiring_package.clone()),
                    span: span.clone(),
                },
            );
        };
        if requirement.package.as_ref() != requirement_package
            || !matches!(
                requirement.source.as_ref(),
                Some(RequirementSource::Path(found_path)) if found_path.as_ref() == path
            )
        {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::FinalManifestRequirementMismatch {
                    requiring_package: Box::new(requiring_package.clone()),
                    span: span.clone(),
                },
            );
        }

        manifest_span_location(
            manifest,
            &MvsRequirer::Package(requiring_package.clone()),
            span,
        )
    }

    /// Returns the package-header location only when the retained final
    /// manifest declares the identity and version captured by the failure.
    pub fn final_package_identity_location(
        &self,
        expected: &MvsPackageVersion,
        found: &MvsPackageVersion,
    ) -> Result<sai_diagnostics::Location, MvsDuplicatePackageLineDiagnosticError> {
        let manifest = self.final_packages.get(expected).ok_or_else(|| {
            MvsDuplicatePackageLineDiagnosticError::MissingFinalManifestLocation(Box::new(
                expected.clone(),
            ))
        })?;
        let Some(header) = manifest.final_package_header.as_ref() else {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::FinalManifestIdentityMismatch {
                    expected: Box::new(expected.clone()),
                    found: Box::new(found.clone()),
                },
            );
        };
        if header.package.as_ref() != found.package() || &header.version != found.version() {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::FinalManifestIdentityMismatch {
                    expected: Box::new(expected.clone()),
                    found: Box::new(found.clone()),
                },
            );
        }

        manifest_span_location(
            manifest,
            &MvsRequirer::Package(expected.clone()),
            &header.span,
        )
    }

    fn register_snapshot(
        &mut self,
        manifest: &MvsManifestLocation,
    ) -> Result<(), MvsDuplicatePackageLineDiagnosticError> {
        let found = manifest.workspace_snapshot;
        if let Some(expected) = self.workspace_snapshot {
            if found != expected {
                return Err(
                    MvsDuplicatePackageLineDiagnosticError::WorkspaceSnapshotMismatch {
                        expected,
                        found,
                    },
                );
            }
        } else {
            self.workspace_snapshot = Some(found);
        }
        Ok(())
    }

    fn for_requirer(&self, requirer: &MvsRequirer) -> Option<&MvsManifestLocation> {
        match requirer {
            MvsRequirer::Root(_) => self.root.as_ref(),
            MvsRequirer::Package(package_version) => self.packages.get(package_version),
        }
    }
}

/// Configuration and source locations for a graph-wide duplicate-line lint.
pub struct MvsDuplicatePackageLineDiagnosticContext<'a> {
    pub manifests: &'a MvsManifestLocations,
    pub cli: Option<OverrideLintLevel>,
    pub deny_warnings: bool,
}

/// Invalid input while constructing a duplicate package-line diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsDuplicatePackageLineDiagnosticError {
    MixedSourceAuditLoadMismatch,
    InvalidExplanation(Box<MvsExplanationError>),
    InvalidSelectedGraph(Box<MvsSelectedGraphInputError>),
    CycleNotInSelectedGraph(Box<MvsDependencyCycle>),
    RootCycleNotInSelection(Box<MvsSelectionError>),
    LintResolution(LintResolutionError),
    LintConfiguration(Box<crate::validation::PackageLintPolicyError>),
    MissingRootManifest,
    MissingManifestLocation(Box<MvsRequirer>),
    MissingFinalManifestLocation(Box<MvsPackageVersion>),
    FinalManifestTooLarge(sai_diagnostics::FileId),
    FinalManifestRequirementMismatch {
        requiring_package: Box<MvsPackageVersion>,
        span: Range<usize>,
    },
    FinalManifestIdentityMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    MissingSnapshotFile(sai_diagnostics::FileId),
    SnapshotFileIdMismatch {
        expected: sai_diagnostics::FileId,
        found: sai_diagnostics::FileId,
    },
    InvalidManifestSyntax {
        file_id: sai_diagnostics::FileId,
        lexical_errors: Vec<crate::lexer::PackageLexError>,
        parse_errors: Vec<PackageParseError>,
    },
    ManifestHeaderMismatch {
        file_id: sai_diagnostics::FileId,
        expected: Box<MvsPackageVersion>,
        found: Option<String>,
    },
    ManifestPathMismatch {
        file_id: sai_diagnostics::FileId,
        expected: String,
        found: String,
    },
    ContractNotDivergent(Box<MvsPackageVersion>),
    DivergentContractPackageVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    DivergentSourcePackageVersionMismatch {
        side: &'static str,
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    DivergentPathComparisonPackageVersionMismatch {
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    DivergentPathComparisonDoesNotMatchEdges(Box<MvsPackageVersion>),
    DivergentSourcesHaveSameIdentity(Box<MvsPackageVersion>),
    ManifestPackageMismatch {
        requirer: Box<MvsRequirer>,
        expected: String,
        found: Option<String>,
    },
    ManifestVersionMismatch {
        requirer: Box<MvsRequirer>,
        expected: Box<MvsPackageVersion>,
        found: Box<MvsPackageVersion>,
    },
    ManifestRoleMismatch {
        expected: PackageRole,
        found: PackageRole,
    },
    WorkspaceSnapshotMismatch {
        expected: sai_diagnostics::WorkspaceSnapshotId,
        found: sai_diagnostics::WorkspaceSnapshotId,
    },
    DuplicateRootManifest,
    DuplicatePackageManifest(Box<MvsPackageVersion>),
    DuplicateFinalPackageManifest(Box<MvsPackageVersion>),
    RequirementNotInManifest {
        requirer: Box<MvsRequirer>,
        span: Range<usize>,
    },
    RequirementMismatch {
        requirer: Box<MvsRequirer>,
        span: Range<usize>,
        expected_package: Box<str>,
        expected_requirement: Box<VersionReq>,
        found_package: Box<str>,
        found_requirement: Box<VersionReq>,
    },
    SourceSpanOutOfRange {
        requirer: Box<MvsRequirer>,
        span: Range<usize>,
    },
    SourceSelectionDoesNotMatchManifest {
        requirer: Box<MvsRequirer>,
        span: Range<usize>,
        expected: String,
        found: Option<String>,
    },
}

/// Builds the hard `VERSION_CONFLICT` diagnostic and its provenance chains.
pub fn version_conflict_diagnostic(
    explanations: &MvsExplanationGraph<'_>,
    conflict: &MvsVersionConflict,
    manifests: &MvsManifestLocations,
) -> Result<Diagnostic, MvsDuplicatePackageLineDiagnosticError> {
    validate_root_manifest(explanations, manifests)?;
    let explanation = explanations.explain_conflict(conflict).map_err(|error| {
        MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
    })?;
    let primary_link = explanation
        .requirement_chain
        .links
        .last()
        .expect("a validated conflict explanation ends with its rejecting requirement");
    let primary = chain_link_location(
        manifests,
        &primary_link.requirer,
        &primary_link.named_target,
        &primary_link.requirement,
        &primary_link.span,
    )?;
    let node = conflict.target.node();
    let message = format!(
        "{}#{} is selected at {}, but {} requires `{}`",
        node.package(),
        node.line(),
        conflict.target.version(),
        requirer_display(&conflict.requirer),
        conflict.requirement
    );
    let mut diagnostic = Diagnostic::new(
        Code::from_name("VERSION_CONFLICT")
            .expect("VERSION_CONFLICT is a registered hard diagnostic"),
        Severity::Error,
        message,
        Some(primary),
    );
    diagnostic.primary_label = format!(
        "this requirement rejects selected version {}",
        conflict.target.version()
    );
    diagnostic.data = version_conflict_data(conflict, &explanation, manifests)?;
    add_selection_reason_children(&mut diagnostic, &explanation.selected_reason, manifests)?;
    diagnostic
        .push_child(Child {
            kind: ChildKind::Note,
            message: "the conflicting requirement is reached through:".to_owned(),
            location: None,
        })
        .expect("a new diagnostic has no snapshot-bound locations");
    add_requirement_chain_children(&mut diagnostic, &explanation.requirement_chain, manifests)?;

    let selected_exceeds_conflicting_minimum =
        conflict.target.version() >= conflict.requirement.minimum();
    if let MvsRequirer::Package(requirer) = &conflict.requirer
        && selected_exceeds_conflicting_minimum
    {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Help,
                message: format!(
                    "a newer release of {} may accept {} {}; try `sai update {}`",
                    requirer.package(),
                    conflict.target.package(),
                    conflict.target.version(),
                    requirer.package()
                ),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations");
    }
    let root_named_selected_version = explanations
        .is_named_by_root_requirement(&conflict.target)
        .map_err(|error| {
            MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
        })?;
    if root_named_selected_version && selected_exceeds_conflicting_minimum {
        let conflicting_package = conflict.requirer.package_version().package();
        diagnostic
            .push_child(Child {
                kind: ChildKind::Help,
                message: format!(
                    "lower the root requirement for {} to a version accepted by {} ({})",
                    conflict.target.package(),
                    conflicting_package,
                    conflict.requirement
                ),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations");
    }
    let package_to_override = if selected_exceeds_conflicting_minimum {
        match &conflict.requirer {
            MvsRequirer::Root(_) => conflict.target.package(),
            MvsRequirer::Package(requirer) => requirer.package(),
        }
    } else {
        conflict.target.package()
    };
    diagnostic
        .push_child(Child {
            kind: ChildKind::Help,
            message: format!(
                "or override {} with a patched copy using `use path` or `use git`",
                package_to_override
            ),
            location: None,
        })
        .expect("a new diagnostic has no snapshot-bound locations");

    diagnostic
        .push_related(Related::RequirementChain {
            steps: requirement_chain_steps(&explanation.requirement_chain, manifests)?,
        })
        .expect("a new diagnostic has no snapshot-bound locations");
    Ok(diagnostic)
}

/// Builds every `VERSION_CONFLICT` diagnostic in selected-graph order.
pub fn version_conflict_diagnostics(
    selection: &MvsSelectionOutput,
    manifests: &MvsManifestLocations,
) -> Result<Vec<Diagnostic>, MvsDuplicatePackageLineDiagnosticError> {
    let selected_graph =
        check_selected_graph(selection.root_contract(), selection).map_err(|error| {
            MvsDuplicatePackageLineDiagnosticError::InvalidSelectedGraph(Box::new(error))
        })?;
    if selected_graph.conflicts.is_empty() {
        return Ok(Vec::new());
    }
    let explanations = MvsExplanationGraph::new(selection).map_err(|error| {
        MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
    })?;
    selected_graph
        .conflicts
        .iter()
        .map(|conflict| version_conflict_diagnostic(&explanations, conflict, manifests))
        .collect()
}

/// Builds the hard `DIVERGENT_SOURCES` diagnostic for one selected version.
///
/// Both source-selection provenances are shown so a user can see which
/// requirement edges selected the read-time and final sources. The included
/// contract edges are listed in diagnostic data and notes, including source
/// differences that require source-bound path resolution.
pub fn divergent_sources_diagnostic<K, E>(
    comparison: &ResolverReloadedContractComparison<K, E>,
    manifests: &MvsManifestLocations,
) -> Result<Diagnostic, MvsDuplicatePackageLineDiagnosticError>
where
    K: Eq,
{
    let expected_package_version = &comparison.reload.package_version;
    let edge_package_version = comparison.edges.package_version();
    if edge_package_version != expected_package_version {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::DivergentContractPackageVersionMismatch {
                expected: Box::new(expected_package_version.clone()),
                found: Box::new(edge_package_version.clone()),
            },
        );
    }
    for (side, selection) in [
        ("read-time", &comparison.reload.read_source),
        ("final", &comparison.reload.final_source),
    ] {
        if selection.package_version() != expected_package_version {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::DivergentSourcePackageVersionMismatch {
                    side,
                    expected: Box::new(expected_package_version.clone()),
                    found: Box::new(selection.package_version().clone()),
                },
            );
        }
    }
    let path_package_version = comparison.paths.package_version();
    if path_package_version != expected_package_version {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::DivergentPathComparisonPackageVersionMismatch {
                expected: Box::new(expected_package_version.clone()),
                found: Box::new(path_package_version.clone()),
            },
        );
    }
    if !comparison.paths.matches_edge_comparison(&comparison.edges) {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::DivergentPathComparisonDoesNotMatchEdges(
                Box::new(expected_package_version.clone()),
            ),
        );
    }
    if comparison.reload.read_identity == comparison.reload.final_identity {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::DivergentSourcesHaveSameIdentity(Box::new(
                expected_package_version.clone(),
            )),
        );
    }
    if comparison.status() != crate::resolver_contract::ResolverContractComparisonStatus::Different
    {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::ContractNotDivergent(Box::new(
                expected_package_version.clone(),
            )),
        );
    }

    let read_location =
        resolver_source_selection_location(&comparison.reload.read_source, manifests)?;
    let final_location =
        resolver_source_selection_location(&comparison.reload.final_source, manifests)?;
    let read_source = resolver_package_source_display(comparison.reload.read_source.source());
    let final_source = resolver_package_source_display(comparison.reload.final_source.source());
    let package_version = &comparison.reload.package_version;
    let primary = final_location.clone().or_else(|| read_location.clone());
    let mut diagnostic = Diagnostic::new(
        Code::from_name("DIVERGENT_SOURCES")
            .expect("DIVERGENT_SOURCES is a registered hard diagnostic"),
        Severity::Error,
        format!(
            "{}@{} has different included contracts in its read-time and final sources",
            package_version.package(),
            package_version.version()
        ),
        primary,
    );
    diagnostic.primary_label = if final_location.is_some() {
        "the final source selected here provides a different contract".to_owned()
    } else {
        "these sources provide different contracts for the same version".to_owned()
    };
    diagnostic.data = divergent_sources_data(comparison, &read_source, &final_source);

    diagnostic
        .push_child(Child {
            kind: ChildKind::Note,
            message: format!(
                "the contract was read from {read_source} (the read-time source tree); {}",
                resolver_source_selection_reason(&comparison.reload.read_source)
            ),
            location: read_location,
        })
        .expect("both source locations belong to one manifest snapshot");
    diagnostic
        .push_child(Child {
            kind: ChildKind::Note,
            message: format!(
                "after Check, the final source is {final_source} (a different resolved source tree); {}",
                resolver_source_selection_reason(&comparison.reload.final_source)
            ),
            location: final_location,
        })
        .expect("both source locations belong to one manifest snapshot");

    for edge in comparison.edges.expected_edges() {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: format!(
                    "read-time contract includes {}",
                    resolver_contract_edge_display(edge)
                ),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations");
    }
    for edge in comparison.edges.actual_edges() {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: format!(
                    "final-source contract includes {}",
                    resolver_contract_edge_display(edge)
                ),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations");
    }
    if comparison.edges.status()
        == crate::resolver_contract::ResolverContractComparisonStatus::RequiresSourceBoundPathResolution
        && comparison.paths.status()
            == crate::resolver_contract::ResolverContractComparisonStatus::Different
    {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: "one or more path clauses resolve to different package sources when based on the read-time and final source trees".to_owned(),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations");
    }
    for difference in comparison.paths.path_differences() {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: format!(
                    "path dependency {} resolves differently: read-time {} versus final-source {}",
                    difference.package,
                    resolver_path_request_display(&difference.expected),
                    resolver_path_request_display(&difference.actual)
                ),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations");
    }
    diagnostic
        .push_child(Child {
            kind: ChildKind::Help,
            message: format!(
                "make the source choice explicit with a root override for {} using `use path \"…\"` or `use git \"…\"`",
                package_version.package()
            ),
            location: None,
        })
        .expect("a new diagnostic has no snapshot-bound locations");
    Ok(diagnostic)
}

/// Builds diagnostics for every selected version whose final source differs
/// from the source used during selection.
pub fn divergent_sources_diagnostics<K, IE, PE>(
    audit: &ResolverFinalContractAudit<'_, K, IE, PE>,
    manifests: &MvsManifestLocations,
) -> Result<Vec<Diagnostic>, MvsDuplicatePackageLineDiagnosticError>
where
    K: Eq,
{
    audit
        .comparisons()
        .iter()
        .filter(|comparison| {
            comparison.status()
                == crate::resolver_contract::ResolverContractComparisonStatus::Different
        })
        .map(|comparison| divergent_sources_diagnostic(comparison, manifests))
        .collect()
}

fn reportable_final_source_identity_failure(
    failure: &ResolverSourceIdentityFailure<MixedSourceFinalIdentityError>,
) -> Option<(Code, String)> {
    if failure.side != ResolverSourceIdentitySide::Final {
        return None;
    }
    let ResolverSourceIdentityFailureKind::Canonicalization(error) = &failure.error else {
        return None;
    };
    Some((error.diagnostic_code()?, error.diagnostic_message()?))
}

/// Builds source-located diagnostics for reportable final-source identity
/// failures. Internal provenance mismatches remain on the audit plan for the
/// package workflow to handle as compiler errors.
pub fn mixed_source_identity_failure_diagnostics(
    selection: &MvsSelectionOutput,
    failures: &[ResolverSourceIdentityFailure<MixedSourceFinalIdentityError>],
    manifests: &MvsManifestLocations,
) -> Result<Vec<Diagnostic>, MvsDuplicatePackageLineDiagnosticError> {
    let mut reportable_failures = failures
        .iter()
        .filter_map(|failure| {
            reportable_final_source_identity_failure(failure)
                .map(|(code, detail)| (failure, code, detail))
        })
        .collect::<Vec<_>>();
    reportable_failures
        .sort_by(|(left, ..), (right, ..)| left.package_version.cmp(&right.package_version));
    if reportable_failures.is_empty() {
        return Ok(Vec::new());
    }
    let explanations = explanation_graph_for_mvs_diagnostics(selection)?;
    validate_root_manifest(&explanations, manifests)?;

    let mut diagnostics = Vec::new();
    for (failure, code, detail) in reportable_failures {
        let chain = explanations
            .chain_to_package_version(&failure.package_version)
            .map_err(|error| {
                MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
            })?;
        let source_location = failure
            .source
            .as_ref()
            .map(|source| resolver_source_selection_location(source, manifests))
            .transpose()?
            .flatten();
        let chain_location = chain
            .links
            .last()
            .map(|link| {
                chain_link_location(
                    manifests,
                    &link.requirer,
                    &link.named_target,
                    &link.requirement,
                    &link.span,
                )
            })
            .transpose()?;
        let package_version = &failure.package_version;
        let mut diagnostic = Diagnostic::new(
            code,
            Severity::Error,
            format!(
                "could not resolve the final source for {}@{}: {detail}",
                package_version.package(),
                package_version.version()
            ),
            source_location.or(chain_location),
        );
        diagnostic.primary_label = "this selected source could not be resolved".to_owned();

        let mut data = ValueObject::new();
        data.insert(
            "package".to_owned(),
            Value::String(package_version.package().to_owned()),
        );
        data.insert(
            "version".to_owned(),
            Value::String(package_version.version().to_string()),
        );
        data.insert("side".to_owned(), Value::String("final_source".to_owned()));
        data.insert("failure".to_owned(), Value::String(detail));
        if let Some(source) = &failure.source {
            data.insert(
                "source".to_owned(),
                Value::String(resolver_package_source_display(source.source())),
            );
        }
        data.insert(
            "requirement_chain".to_owned(),
            requirement_chain_data(&chain, manifests)?,
        );
        diagnostic.data = data;
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: "the final source is reached through:".to_owned(),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations");
        add_requirement_chain_children(&mut diagnostic, &chain, manifests)?;
        diagnostics.push(diagnostic);
    }
    Ok(diagnostics)
}

/// Builds source-located diagnostics for final reload failures that have a
/// safe, package-level classification. Local and Git path-requirement failures
/// point into their retained final manifest, package identity mismatches point
/// to the changed final header, and retained lexical/parser errors use their
/// final-source byte ranges. The original MVS chain remains bound to the
/// read-time manifests.
pub fn mixed_source_reload_failure_diagnostics(
    mixed_source: &MixedSourceMvsOutput,
    audit_plan: &MixedSourceFinalAuditPlan,
    manifests: &MvsManifestLocations,
) -> Result<Vec<Diagnostic>, MvsDuplicatePackageLineDiagnosticError> {
    if !audit_plan.belongs_to(mixed_source) {
        return Err(MvsDuplicatePackageLineDiagnosticError::MixedSourceAuditLoadMismatch);
    }
    let selection = mixed_source.provider().output().selection();
    let workspace_member_packages = workspace_member_source_packages(audit_plan, manifests);
    let mut reportable_failures = audit_plan
        .reload_failures()
        .iter()
        .filter_map(|failure| {
            let package_version = failure.package_version.as_ref()?;
            audit_plan
                .final_sources()
                .entries()
                .iter()
                .any(|entry| entry.package_version() == package_version)
                .then_some(())?;
            let final_path_requirement = match &failure.error {
                MixedSourceFinalReloadError::LocalPathIndex(error) => match error.as_ref() {
                    LocalPackagePathIndexError::PathRequirementDiscovery {
                        package,
                        path,
                        requiring_package,
                        span,
                        ..
                    } => Some(FinalPathRequirement {
                        requirement_package: package.as_ref(),
                        path: path.as_ref(),
                        requiring_package,
                        span: span.clone(),
                    }),
                    _ => None,
                },
                MixedSourceFinalReloadError::GitPathDiscovery(error) => {
                    error.requirement_edge().map(|edge| FinalPathRequirement {
                        requirement_package: edge.package(),
                        path: edge.path(),
                        requiring_package: package_version,
                        span: edge.span().clone(),
                    })
                }
                _ => None,
            };
            let final_package_identity_mismatch =
                final_package_identity_mismatch_found(&failure.error);
            Some(ReportableFinalReloadFailure {
                package_version,
                code: failure.error.diagnostic_code()?,
                detail: failure.error.diagnostic_message()?,
                final_path_requirement,
                final_package_identity_mismatch,
            })
        })
        .collect::<Vec<_>>();
    sort_and_deduplicate_final_reload_failure_diagnostics(&mut reportable_failures);
    let final_syntax_packages = audit_plan
        .final_manifest_snapshots()
        .keys()
        .filter(|package_version| {
            audit_plan
                .final_sources()
                .entries()
                .iter()
                .any(|entry| entry.package_version() == *package_version)
                && manifests
                    .final_packages
                    .get(*package_version)
                    .is_some_and(|manifest| !manifest.final_manifest_syntax_issues.is_empty())
        })
        .collect::<Vec<_>>();
    let final_semantic_failures = audit_plan
        .reload_failures()
        .iter()
        .filter_map(|failure| {
            let package_version = failure.package_version.as_ref()?;
            audit_plan
                .final_sources()
                .entries()
                .iter()
                .any(|entry| entry.package_version() == package_version)
                .then_some(())?;
            let errors = final_manifest_semantic_validation_errors(&failure.error)?;
            (!errors.is_empty()).then_some((package_version, errors))
        })
        .collect::<Vec<_>>();
    let audit = audit_plan.audit();
    let reportable_identity_packages = audit
        .source_identities()
        .failures()
        .iter()
        .filter_map(|failure| {
            reportable_final_source_identity_failure(failure)
                .map(|_| failure.package_version.clone())
        })
        .collect::<BTreeSet<_>>();
    let reportable_reload_packages = reportable_failures
        .iter()
        .map(|failure| failure.package_version.clone())
        .chain(
            final_syntax_packages
                .iter()
                .map(|package| (*package).clone()),
        )
        .chain(
            final_semantic_failures
                .iter()
                .map(|(package, _)| (*package).clone()),
        )
        .collect::<BTreeSet<_>>();
    let missing_path_source_identities = audit
        .comparisons()
        .iter()
        .flat_map(|comparison| comparison.paths().failures())
        .filter_map(|failure| {
            let ResolverFinalContractPathResolutionError::MissingSourceIdentity(package_version) =
                failure.error.as_ref()
            else {
                return None;
            };
            (!reportable_identity_packages.contains(package_version.as_ref())).then_some(())
        })
        .count();
    let unexplained_audit_failures = audit.failures().iter().filter(|failure| {
        final_contract_audit_failure_is_unexplained(
            failure,
            &reportable_reload_packages,
            &reportable_identity_packages,
        )
    });
    let internal_audit_error = final_contract_audit_internal_error(
        unexplained_audit_failures,
        missing_path_source_identities,
    );
    if reportable_failures.is_empty()
        && final_syntax_packages.is_empty()
        && final_semantic_failures.is_empty()
        && internal_audit_error.is_none()
    {
        return Ok(Vec::new());
    }
    let explanations = explanation_graph_for_mvs_diagnostics(selection)?;
    validate_root_manifest(&explanations, manifests)?;

    let mut diagnostics = Vec::new();
    for package_version in final_syntax_packages {
        let manifest = manifests
            .final_packages
            .get(package_version)
            .expect("syntax-reportable final packages have bound manifests");
        let chain = explanations
            .chain_to_package_version(package_version)
            .map_err(|error| {
                MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
            })?;
        for (issue_index, issue) in manifest.final_manifest_syntax_issues.iter().enumerate() {
            let primary = manifest_span_location(
                manifest,
                &MvsRequirer::Package(package_version.clone()),
                &issue.span,
            )?;
            let mut diagnostic = Diagnostic::new(
                issue.code,
                Severity::Error,
                format!(
                    "could not reload selected package {}@{}: {}",
                    package_version.package(),
                    package_version.version(),
                    issue.message
                ),
                Some(primary),
            );
            diagnostic.primary_label = issue.message.clone();
            diagnostic.package = Some(package_version.package().to_owned());
            let mut data = ValueObject::new();
            data.insert(
                "package".to_owned(),
                Value::String(package_version.package().to_owned()),
            );
            data.insert(
                "version".to_owned(),
                Value::String(package_version.version().to_string()),
            );
            data.insert(
                "stage".to_owned(),
                Value::String("final_source_reload".to_owned()),
            );
            data.insert(
                "requirement_chain".to_owned(),
                requirement_chain_data(&chain, manifests)?,
            );
            if let Some(expected_tokens) = &issue.expected_tokens {
                data.insert(
                    "expected_tokens".to_owned(),
                    Value::Array(expected_tokens.iter().cloned().map(Value::String).collect()),
                );
            }
            if manifest.final_manifest_syntax_issues_truncated
                && issue_index + 1 == manifest.final_manifest_syntax_issues.len()
            {
                data.insert(
                    "additional_syntax_errors_omitted".to_owned(),
                    Value::Bool(true),
                );
            }
            diagnostic.data = data;
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: "the selected package is reached through:".to_owned(),
                    location: None,
                })
                .expect("a new diagnostic has no snapshot-bound locations");
            add_requirement_chain_children(&mut diagnostic, &chain, manifests)?;
            if manifest.final_manifest_syntax_issues_truncated
                && issue_index + 1 == manifest.final_manifest_syntax_issues.len()
            {
                diagnostic
                    .push_child(Child {
                        kind: ChildKind::Note,
                        message: format!(
                            "additional final manifest syntax errors were omitted after the limit of {MAX_FINAL_MANIFEST_SYNTAX_ISSUES}"
                        ),
                        location: None,
                    })
                    .expect("a new diagnostic has no snapshot-bound locations");
            }
            diagnostics.push(diagnostic);
        }
    }
    for (package_version, errors) in final_semantic_failures {
        let manifest = manifests.final_package(package_version).ok_or_else(|| {
            MvsDuplicatePackageLineDiagnosticError::MissingFinalManifestLocation(Box::new(
                package_version.clone(),
            ))
        })?;
        let chain = explanations
            .chain_to_package_version(package_version)
            .map_err(|error| {
                MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
            })?;
        let error_count = errors.len().min(MAX_FINAL_MANIFEST_SEMANTIC_ISSUES);
        let omitted_errors = errors.len() > error_count;
        for (issue_index, error) in errors.iter().take(error_count).enumerate() {
            let (code, message) = final_manifest_semantic_diagnostic(&error.kind);
            let primary = manifest_span_location(
                manifest,
                &MvsRequirer::Package(package_version.clone()),
                &error.span,
            )?;
            let mut diagnostic = Diagnostic::new(
                code,
                Severity::Error,
                format!(
                    "could not reload selected package {}@{}: {message}",
                    package_version.package(),
                    package_version.version()
                ),
                Some(primary),
            );
            diagnostic.primary_label = message.to_owned();
            diagnostic.package = Some(package_version.package().to_owned());
            let mut data = ValueObject::new();
            data.insert(
                "package".to_owned(),
                Value::String(package_version.package().to_owned()),
            );
            data.insert(
                "version".to_owned(),
                Value::String(package_version.version().to_string()),
            );
            data.insert(
                "stage".to_owned(),
                Value::String("final_source_reload".to_owned()),
            );
            data.insert(
                "requirement_chain".to_owned(),
                requirement_chain_data(&chain, manifests)?,
            );
            if omitted_errors && issue_index + 1 == error_count {
                data.insert(
                    "additional_semantic_errors_omitted".to_owned(),
                    Value::Bool(true),
                );
            }
            diagnostic.data = data;
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: "the selected package is reached through:".to_owned(),
                    location: None,
                })
                .expect("a new diagnostic has no snapshot-bound locations");
            add_requirement_chain_children(&mut diagnostic, &chain, manifests)?;
            if omitted_errors && issue_index + 1 == error_count {
                diagnostic
                    .push_child(Child {
                        kind: ChildKind::Note,
                        message: format!(
                            "additional final manifest semantic errors were omitted after the limit of {MAX_FINAL_MANIFEST_SEMANTIC_ISSUES}"
                        ),
                        location: None,
                    })
                    .expect("a new diagnostic has no snapshot-bound locations");
            }
            diagnostics.push(diagnostic);
        }
    }
    for failure in reportable_failures {
        let package_version = failure.package_version;
        let detail = failure.detail;
        let source = audit_plan
            .final_sources()
            .entries()
            .iter()
            .find(|entry| entry.package_version() == package_version)
            .expect("reportable failures have a final source entry")
            .final_source();
        let chain = explanations
            .chain_to_package_version(package_version)
            .map_err(|error| {
                MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
            })?;
        let source_location = resolver_source_selection_location(source, manifests)?;
        let chain_location = chain
            .links
            .last()
            .map(|link| {
                chain_link_location(
                    manifests,
                    &link.requirer,
                    &link.named_target,
                    &link.requirement,
                    &link.span,
                )
            })
            .transpose()?;
        let final_path_location = failure
            .final_path_requirement
            .map(|edge| {
                if edge.requiring_package != package_version {
                    return Err(
                        MvsDuplicatePackageLineDiagnosticError::FinalManifestRequirementMismatch {
                            requiring_package: Box::new(edge.requiring_package.clone()),
                            span: edge.span,
                        },
                    );
                }
                manifests.final_path_requirement_location(
                    edge.requiring_package,
                    edge.requirement_package,
                    edge.path,
                    &edge.span,
                )
            })
            .transpose()?;
        let final_identity_location = failure
            .final_package_identity_mismatch
            .as_ref()
            .map(|found| manifests.final_package_identity_location(package_version, found))
            .transpose()?;
        let mut diagnostic = Diagnostic::new(
            failure.code,
            Severity::Error,
            format!(
                "could not reload selected package {}@{}: {detail}",
                package_version.package(),
                package_version.version()
            ),
            final_path_location
                .clone()
                .or(final_identity_location.clone())
                .or(source_location)
                .or(chain_location),
        );
        diagnostic.primary_label = if final_path_location.is_some() {
            "this path dependency could not be resolved in the final source".to_owned()
        } else if final_identity_location.is_some() {
            "the final source declares a different package identity or version".to_owned()
        } else {
            "the selected package's final source could not be reloaded".to_owned()
        };

        let mut data = ValueObject::new();
        data.insert(
            "package".to_owned(),
            Value::String(package_version.package().to_owned()),
        );
        data.insert(
            "version".to_owned(),
            Value::String(package_version.version().to_string()),
        );
        data.insert(
            "stage".to_owned(),
            Value::String("final_source_reload".to_owned()),
        );
        data.insert("failure".to_owned(), Value::String(detail));
        data.insert(
            "requirement_chain".to_owned(),
            requirement_chain_data(&chain, manifests)?,
        );
        diagnostic.data = data;
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: "the selected package is reached through:".to_owned(),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations");
        add_requirement_chain_children(&mut diagnostic, &chain, manifests)?;
        diagnostics.push(diagnostic);
    }
    sort_final_source_diagnostics(
        &mut diagnostics,
        manifests,
        &workspace_member_packages,
        internal_audit_error,
    );
    Ok(diagnostics)
}

fn workspace_member_source_packages(
    audit_plan: &MixedSourceFinalAuditPlan,
    manifests: &MvsManifestLocations,
) -> BTreeSet<String> {
    let identities = audit_plan.source_identities();
    let Some(MixedPackageSourceIdentity::Local(root_directory)) =
        identities.read_source_identities().root_identity()
    else {
        return BTreeSet::new();
    };

    let mut package_identities = identities
        .read_source_identities()
        .package_identities()
        .iter()
        .map(|(package_version, identity)| (package_version.clone(), identity))
        .collect::<BTreeMap<_, _>>();
    for same_identity in identities.same_identity() {
        package_identities.insert(
            same_identity.package_version.clone(),
            &same_identity.identity,
        );
    }
    for reload in identities.reloads() {
        package_identities.insert(reload.package_version.clone(), &reload.final_identity);
    }

    package_identities
        .into_iter()
        .filter_map(|(package_version, identity)| {
            let MixedPackageSourceIdentity::Local(directory) = identity else {
                return None;
            };
            (directory.path() != root_directory.path()
                && directory.path().starts_with(root_directory.path()))
            .then_some(package_version)
        })
        .filter_map(|package_version| {
            manifests
                .packages
                .get(&package_version)
                .and_then(|manifest| manifest.source.package.clone())
        })
        .collect()
}

fn sort_final_source_diagnostics(
    diagnostics: &mut Vec<Diagnostic>,
    manifests: &MvsManifestLocations,
    workspace_member_packages: &BTreeSet<String>,
    internal_error: Option<Diagnostic>,
) {
    diagnostics.sort_by(|left, right| {
        let left_primary = left.primary();
        let right_primary = right.primary();
        left_primary
            .is_some()
            .cmp(&right_primary.is_some())
            .then_with(|| {
                final_diagnostic_package_rank(left_primary, manifests, workspace_member_packages)
                    .cmp(&final_diagnostic_package_rank(
                        right_primary,
                        manifests,
                        workspace_member_packages,
                    ))
            })
            .then_with(|| {
                final_diagnostic_file_rank(left_primary)
                    .cmp(&final_diagnostic_file_rank(right_primary))
            })
            .then_with(|| {
                left_primary
                    .map(|location| location.file.path.as_str())
                    .cmp(&right_primary.map(|location| location.file.path.as_str()))
            })
            .then_with(|| {
                left_primary
                    .map(|location| location.span.range.start())
                    .cmp(&right_primary.map(|location| location.span.range.start()))
            })
            .then_with(|| {
                left_primary
                    .map(|location| location.span.range.end())
                    .cmp(&right_primary.map(|location| location.span.range.end()))
            })
            .then_with(|| left.severity.cmp(&right.severity))
            .then_with(|| left.code.as_str().cmp(right.code.as_str()))
            .then_with(|| left.message.cmp(&right.message))
            .then_with(|| format!("{left:?}").cmp(&format!("{right:?}")))
    });
    if let Some(internal_error) = internal_error {
        diagnostics.push(internal_error);
    }
}

fn final_diagnostic_package_rank(
    location: Option<&sai_diagnostics::Location>,
    manifests: &MvsManifestLocations,
    workspace_member_packages: &BTreeSet<String>,
) -> (u8, String) {
    let Some(location) = location else {
        return (0, String::new());
    };
    let package = location.file.package.as_deref().unwrap_or_default();
    if manifests
        .root
        .as_ref()
        .is_some_and(|root| root.source.package.as_deref() == Some(package))
    {
        (0, String::new())
    } else if workspace_member_packages.contains(package) {
        (1, package.to_owned())
    } else {
        (2, package.to_owned())
    }
}

fn final_diagnostic_file_rank(location: Option<&sai_diagnostics::Location>) -> (u8, String) {
    let Some(path) = location.map(|location| location.file.path.as_str()) else {
        return (0, String::new());
    };
    let file_name = path.rsplit(['/', '\\']).next().unwrap_or(path);
    let rank = match file_name {
        "sai.package" => 0,
        "sai.lock" => 1,
        _ => 2,
    };
    (rank, path.to_owned())
}

fn final_contract_audit_internal_error<'a>(
    failures: impl IntoIterator<
        Item = &'a ResolverFinalContractAuditFailure<MixedPackageSourceIdentity>,
    >,
    missing_path_source_identities: usize,
) -> Option<Diagnostic> {
    let failures = failures.into_iter().collect::<Vec<_>>();
    if failures.is_empty() && missing_path_source_identities == 0 {
        return None;
    }

    let mut categories = failures
        .iter()
        .map(|failure| final_contract_audit_failure_name(&failure.error))
        .collect::<BTreeSet<_>>();
    if missing_path_source_identities > 0 {
        categories.insert("missing_path_source_identity");
    }
    let finding_count = failures.len() + missing_path_source_identities;
    let description = format!(
        "final-source contract audit found {finding_count} internal inconsistencies ({})",
        categories.into_iter().collect::<Vec<_>>().join(", ")
    );
    let panic_message = format!("{description} at {}:{}:{}", file!(), line!(), column!());
    let mut diagnostic = Diagnostic::new(
        Code::InternalCompilerError,
        Severity::Error,
        format!("internal compiler error: {description}"),
        None,
    );
    let mut data = ValueObject::new();
    data.insert(
        "component".to_owned(),
        Value::String("sai_package".to_owned()),
    );
    data.insert("panic_message".to_owned(), Value::String(panic_message));
    data.insert("query_stack".to_owned(), Value::Array(Vec::new()));
    data.insert(
        "compiler_version".to_owned(),
        Value::String(concat!(env!("CARGO_PKG_VERSION"), "+", env!("SAI_BUILD_ID")).to_owned()),
    );
    diagnostic.data = data;
    Some(diagnostic)
}

fn final_contract_audit_failure_is_unexplained(
    failure: &ResolverFinalContractAuditFailure<MixedPackageSourceIdentity>,
    reportable_reload_packages: &BTreeSet<MvsPackageVersion>,
    reportable_identity_packages: &BTreeSet<MvsPackageVersion>,
) -> bool {
    !matches!(
        failure.error,
        ResolverFinalContractAuditFailureKind::MissingReloadedContract
    ) || (!reportable_reload_packages.contains(&failure.package_version)
        && !reportable_identity_packages.contains(&failure.package_version))
}

fn final_contract_audit_failure_name(
    error: &ResolverFinalContractAuditFailureKind<MixedPackageSourceIdentity>,
) -> &'static str {
    match error {
        ResolverFinalContractAuditFailureKind::MissingReloadedContract => {
            "missing_reloaded_contract"
        }
        ResolverFinalContractAuditFailureKind::UnexpectedReloadedContract => {
            "unexpected_reloaded_contract"
        }
        ResolverFinalContractAuditFailureKind::IdentifiedLoadMismatch => "identified_load_mismatch",
        ResolverFinalContractAuditFailureKind::ReloadedSourceIdentityMismatch { .. } => {
            "reloaded_source_identity_mismatch"
        }
        ResolverFinalContractAuditFailureKind::MissingIdentityResult => "missing_identity_result",
        ResolverFinalContractAuditFailureKind::UnexpectedIdentityResult => {
            "unexpected_identity_result"
        }
        ResolverFinalContractAuditFailureKind::IdentityPlanMismatch => "identity_plan_mismatch",
        ResolverFinalContractAuditFailureKind::ContractComparison(error) => match error.as_ref() {
            ResolverContractComparisonError::DependencyRoleMismatch { .. } => {
                "contract_dependency_role_mismatch"
            }
            ResolverContractComparisonError::PackageVersionMismatch { .. } => {
                "contract_package_version_mismatch"
            }
            ResolverContractComparisonError::ContractContextMismatch(_) => {
                "contract_context_mismatch"
            }
            ResolverContractComparisonError::ContractMetadataMismatch(_) => {
                "contract_metadata_mismatch"
            }
            ResolverContractComparisonError::RootContextMismatch => {
                "contract_root_context_mismatch"
            }
            ResolverContractComparisonError::InvalidGitUrl { .. } => "contract_invalid_git_url",
            ResolverContractComparisonError::InvalidGitDirectory { .. } => {
                "contract_invalid_git_directory"
            }
        },
    }
}

fn final_package_identity_mismatch_found(
    error: &MixedSourceFinalReloadError,
) -> Option<MvsPackageVersion> {
    match error {
        MixedSourceFinalReloadError::LocalDirectoryIndex(error) => match error.as_ref() {
            crate::local_source::LocalPackageDirectoryIndexError::PackageVersionMismatch {
                found_package,
                found_version,
                ..
            } => MvsPackageVersion::new(found_package.clone(), (**found_version).clone()).ok(),
            _ => None,
        },
        MixedSourceFinalReloadError::LocalContract(error) => match error.as_ref() {
            crate::local_source::LocalPackageContractLoadError::PackageVersionMismatch {
                found,
                ..
            } => Some((**found).clone()),
            _ => None,
        },
        MixedSourceFinalReloadError::GitContract(error) => match error.as_ref() {
            crate::git_remote::GitRemoteReleaseContractError::PackageVersionMismatch {
                found,
                ..
            } => Some((**found).clone()),
            _ => None,
        },
        MixedSourceFinalReloadError::GitPathDiscovery(error) => match error.as_ref() {
            crate::git_path_requirements::GitPathRequirementDiscoveryError::RequirerVersionMismatch {
                found,
                ..
            } => Some((**found).clone()),
            _ => None,
        },
        _ => None,
    }
}

fn sort_and_deduplicate_final_reload_failure_diagnostics(
    failures: &mut Vec<ReportableFinalReloadFailure<'_>>,
) {
    failures.sort_by(|left, right| {
        left.package_version
            .cmp(right.package_version)
            .then_with(|| left.code.cmp(&right.code))
            .then_with(|| {
                left.final_package_identity_mismatch
                    .cmp(&right.final_package_identity_mismatch)
            })
            .then_with(|| left.detail.cmp(&right.detail))
    });
    failures.dedup_by(|left, right| {
        let same_identity_mismatch = left.package_version == right.package_version
            && left.code == right.code
            && left.final_package_identity_mismatch.is_some()
            && left.final_package_identity_mismatch == right.final_package_identity_mismatch;
        same_identity_mismatch || *left == *right
    });
}

#[cfg(test)]
fn sort_and_deduplicate_reload_failure_diagnostics(
    failures: &mut Vec<(&MvsPackageVersion, Code, String)>,
) {
    failures.sort();
    failures.dedup();
}

fn explanation_graph_for_mvs_diagnostics(
    selection: &MvsSelectionOutput,
) -> Result<MvsExplanationGraph<'_>, MvsDuplicatePackageLineDiagnosticError> {
    let explanations = if selection.is_complete() {
        MvsExplanationGraph::new(selection)
    } else {
        MvsExplanationGraph::new_for_root_cycle_errors(selection)
    };
    explanations.map_err(|error| {
        MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
    })
}

/// Builds the hard `DEPENDENCY_CYCLE` diagnostic for one selected-graph cycle.
pub fn dependency_cycle_diagnostic(
    selection: &MvsSelectionOutput,
    cycle: &MvsDependencyCycle,
    manifests: &MvsManifestLocations,
) -> Result<Diagnostic, MvsDuplicatePackageLineDiagnosticError> {
    let selected_graph =
        check_selected_graph(selection.root_contract(), selection).map_err(|error| {
            MvsDuplicatePackageLineDiagnosticError::InvalidSelectedGraph(Box::new(error))
        })?;
    if selected_graph.cycle.as_ref() != Some(cycle) {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::CycleNotInSelectedGraph(Box::new(
                cycle.clone(),
            )),
        );
    }
    let explanations = MvsExplanationGraph::new(selection).map_err(|error| {
        MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
    })?;
    validate_root_manifest(&explanations, manifests)?;

    let mut edges = Vec::with_capacity(cycle.spans.len());
    let mut closing_chain = None;
    for edge_index in 0..cycle.spans.len() {
        let chain = explanations
            .chain_for_cycle_edge(cycle, edge_index)
            .map_err(|error| {
                MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
            })?;
        let link = chain
            .links
            .last()
            .expect("a validated cycle edge explanation ends with its edge")
            .clone();
        let location = chain_link_location(
            manifests,
            &link.requirer,
            &link.named_target,
            &link.requirement,
            &link.span,
        )?;
        edges.push((link, location));
        if edge_index + 1 == cycle.spans.len() {
            closing_chain = Some(chain);
        }
    }
    let closing_chain = closing_chain.expect("a selected dependency cycle has a closing edge");

    let message = format!(
        "selected package versions form a dependency cycle: {}",
        cycle
            .nodes
            .iter()
            .map(|package_version| format!(
                "{} {}",
                package_version.package(),
                package_version.version()
            ))
            .collect::<Vec<_>>()
            .join(" -> ")
    );
    let primary = edges
        .first()
        .expect("a selected dependency cycle contains at least one edge")
        .1
        .clone();
    let mut diagnostic = Diagnostic::new(
        Code::from_name("DEPENDENCY_CYCLE")
            .expect("DEPENDENCY_CYCLE is a registered hard diagnostic"),
        Severity::Error,
        message,
        Some(primary),
    );
    diagnostic.primary_label = "this requirement is part of the dependency cycle".to_owned();
    diagnostic.data = dependency_cycle_data(selection, cycle, &edges);
    diagnostic.data.insert(
        "requirement_chain".to_owned(),
        requirement_chain_data(&closing_chain, manifests)?,
    );
    for (link, location) in &edges {
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: format!(
                    "{} requires {} @ {} (selected {})",
                    requirer_display(&link.requirer),
                    link.named_target.package(),
                    link.requirement,
                    cycle_target_for_edge(cycle, link).version()
                ),
                location: Some(location.clone()),
            })
            .expect("a new diagnostic has no snapshot-bound locations");
    }
    diagnostic
        .push_child(Child {
            kind: ChildKind::Note,
            message: "the cycle-closing requirement is reached from the root through:".to_owned(),
            location: None,
        })
        .expect("a new diagnostic has no snapshot-bound locations");
    add_requirement_chain_children(&mut diagnostic, &closing_chain, manifests)?;
    diagnostic
        .push_related(Related::RequirementChain {
            steps: requirement_chain_steps(&closing_chain, manifests)?,
        })
        .expect("a new diagnostic has no snapshot-bound locations");
    Ok(diagnostic)
}

/// Builds the selected graph's deterministic cycle diagnostic, if one exists.
pub fn dependency_cycle_diagnostics(
    selection: &MvsSelectionOutput,
    manifests: &MvsManifestLocations,
) -> Result<Vec<Diagnostic>, MvsDuplicatePackageLineDiagnosticError> {
    let selected_graph =
        check_selected_graph(selection.root_contract(), selection).map_err(|error| {
            MvsDuplicatePackageLineDiagnosticError::InvalidSelectedGraph(Box::new(error))
        })?;
    let Some(cycle) = selected_graph.cycle.as_ref() else {
        return Ok(Vec::new());
    };
    Ok(vec![dependency_cycle_diagnostic(
        selection, cycle, manifests,
    )?])
}

/// Builds `DEPENDENCY_CYCLE` for an indirect dependency on the root's line.
pub fn root_dependency_cycle_diagnostic(
    selection: &MvsSelectionOutput,
    selection_error: &MvsSelectionError,
    manifests: &MvsManifestLocations,
) -> Result<Diagnostic, MvsDuplicatePackageLineDiagnosticError> {
    if !selection.errors().contains(selection_error) {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::RootCycleNotInSelection(Box::new(
                selection_error.clone(),
            )),
        );
    }
    let MvsSelectionErrorKind::DependencyCycleToRoot { requirer, root } = &selection_error.kind
    else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::RootCycleNotInSelection(Box::new(
                selection_error.clone(),
            )),
        );
    };
    let Some(span) = selection_error.span.clone() else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::RootCycleNotInSelection(Box::new(
                selection_error.clone(),
            )),
        );
    };
    if root != selection.root() {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::RootCycleNotInSelection(Box::new(
                selection_error.clone(),
            )),
        );
    }
    let explanations =
        MvsExplanationGraph::new_for_root_cycle_errors(selection).map_err(|error| {
            MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
        })?;
    validate_root_manifest(&explanations, manifests)?;
    let edge_contracts = match requirer {
        MvsRequirer::Root(_) => Some(selection.root_contract()),
        MvsRequirer::Package(package_version) => selection.read_contracts().get(package_version),
    };
    let Some(named_target) = edge_contracts
        .into_iter()
        .flat_map(|contract| contract.edges())
        .find(|edge| edge.span() == span && edge.target().node() == root.node())
        .map(|edge| edge.target())
    else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::RootCycleNotInSelection(Box::new(
                selection_error.clone(),
            )),
        );
    };
    let chain = explanations
        .chain_to_edge(requirer, &named_target, span)
        .map_err(|error| {
            MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
        })?;
    let mut edges = Vec::with_capacity(chain.links.len());
    for link in &chain.links {
        let location = chain_link_location(
            manifests,
            &link.requirer,
            &link.named_target,
            &link.requirement,
            &link.span,
        )?;
        edges.push((link.clone(), location));
    }
    let mut nodes = Vec::with_capacity(edges.len() + 1);
    nodes.push(root.clone());
    nodes.extend(
        edges
            .iter()
            .take(edges.len().saturating_sub(1))
            .map(|(link, _)| link.named_target.clone()),
    );
    nodes.push(root.clone());
    let cycle = MvsDependencyCycle {
        spans: edges.iter().map(|(link, _)| link.span.clone()).collect(),
        nodes,
    };
    let message = format!(
        "dependency cycle reaches the root package: {}",
        cycle
            .nodes
            .iter()
            .map(|package_version| format!(
                "{} {}",
                package_version.package(),
                package_version.version()
            ))
            .collect::<Vec<_>>()
            .join(" -> ")
    );
    let primary = edges
        .last()
        .expect("a root cycle error includes its requiring edge")
        .1
        .clone();
    let mut diagnostic = Diagnostic::new(
        Code::from_name("DEPENDENCY_CYCLE")
            .expect("DEPENDENCY_CYCLE is a registered hard diagnostic"),
        Severity::Error,
        message,
        Some(primary),
    );
    diagnostic.primary_label = "this requirement reaches the root package line".to_owned();
    diagnostic.data = dependency_cycle_data(selection, &cycle, &edges);
    diagnostic.data.insert(
        "requirement_chain".to_owned(),
        requirement_chain_data(&chain, manifests)?,
    );
    diagnostic
        .push_child(Child {
            kind: ChildKind::Note,
            message: "the dependency path reaches the root through:".to_owned(),
            location: None,
        })
        .expect("a new diagnostic has no snapshot-bound locations");
    add_requirement_chain_children(&mut diagnostic, &chain, manifests)?;
    diagnostic
        .push_related(Related::RequirementChain {
            steps: requirement_chain_steps(&chain, manifests)?,
        })
        .expect("a new diagnostic has no snapshot-bound locations");
    Ok(diagnostic)
}

/// Builds diagnostics for indirect selected-candidate edges back to the root.
pub fn root_dependency_cycle_diagnostics(
    selection: &MvsSelectionOutput,
    manifests: &MvsManifestLocations,
) -> Result<Vec<Diagnostic>, MvsDuplicatePackageLineDiagnosticError> {
    selection
        .errors()
        .iter()
        .filter(|error| {
            matches!(
                &error.kind,
                MvsSelectionErrorKind::DependencyCycleToRoot { .. }
            )
        })
        .map(|error| root_dependency_cycle_diagnostic(selection, error, manifests))
        .collect()
}

fn validate_root_manifest(
    explanations: &MvsExplanationGraph<'_>,
    manifests: &MvsManifestLocations,
) -> Result<(), MvsDuplicatePackageLineDiagnosticError> {
    let root_manifest = manifests
        .root
        .as_ref()
        .ok_or(MvsDuplicatePackageLineDiagnosticError::MissingRootManifest)?;
    let selection_root = explanations.root_package_version();
    if root_manifest.package_version != *selection_root {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::ManifestVersionMismatch {
                requirer: Box::new(MvsRequirer::Root(selection_root.clone())),
                expected: Box::new(selection_root.clone()),
                found: Box::new(root_manifest.package_version.clone()),
            },
        );
    }
    Ok(())
}

fn cycle_target_for_edge<'a>(
    cycle: &'a MvsDependencyCycle,
    link: &MvsChainLink,
) -> &'a MvsPackageVersion {
    cycle
        .nodes
        .windows(2)
        .find_map(|pair| {
            (pair[0] == *link.requirer.package_version()
                && pair[1].node() == link.named_target.node())
            .then_some(&pair[1])
        })
        .expect("cycle edge explanations match one selected cycle edge")
}

fn selected_target_for_edge(
    selection: &MvsSelectionOutput,
    link: &MvsChainLink,
) -> MvsPackageVersion {
    let node = link.named_target.node();
    if node == selection.root().node() {
        return selection.root().clone();
    }
    let version = selection
        .selected()
        .get(&node)
        .expect("a reported cycle edge has a selected target version");
    MvsPackageVersion::new(node.package().to_owned(), version.clone())
        .expect("selected package identities were validated by MVS input construction")
}

fn dependency_cycle_data(
    selection: &MvsSelectionOutput,
    cycle: &MvsDependencyCycle,
    edges: &[(MvsChainLink, sai_diagnostics::Location)],
) -> ValueObject {
    let nodes = cycle
        .nodes
        .iter()
        .map(|package_version| {
            let mut fields = ValueObject::new();
            fields.insert(
                "package".to_owned(),
                Value::String(package_version.package().to_owned()),
            );
            fields.insert(
                "version".to_owned(),
                Value::String(package_version.version().to_string()),
            );
            Value::Object(fields)
        })
        .collect();
    let edge_values = edges
        .iter()
        .map(|(link, location)| {
            let target = selected_target_for_edge(selection, link);
            let mut fields = ValueObject::new();
            fields.insert(
                "requirer".to_owned(),
                Value::String(link.requirer.package_version().package().to_owned()),
            );
            fields.insert(
                "requirer_version".to_owned(),
                Value::String(link.requirer.package_version().version().to_string()),
            );
            fields.insert(
                "requirer_is_root".to_owned(),
                Value::Bool(matches!(&link.requirer, MvsRequirer::Root(_))),
            );
            fields.insert(
                "target".to_owned(),
                Value::String(target.package().to_owned()),
            );
            fields.insert(
                "target_version".to_owned(),
                Value::String(target.version().to_string()),
            );
            fields.insert(
                "named_target_version".to_owned(),
                Value::String(link.named_target.version().to_string()),
            );
            fields.insert(
                "requirement".to_owned(),
                Value::String(link.requirement.to_string()),
            );
            fields.insert("location".to_owned(), location_data(location));
            Value::Object(fields)
        })
        .collect();
    let mut data = ValueObject::new();
    data.insert("nodes".to_owned(), Value::Array(nodes));
    data.insert("edges".to_owned(), Value::Array(edge_values));
    data
}

fn version_conflict_data(
    conflict: &MvsVersionConflict,
    explanation: &MvsConflictExplanation,
    manifests: &MvsManifestLocations,
) -> Result<ValueObject, MvsDuplicatePackageLineDiagnosticError> {
    let node = conflict.target.node();
    let (selected_reason, selected_chain) = match &explanation.selected_reason {
        MvsSelectionReason::RootOverridePin { chain } => (
            "root_override_pin",
            requirement_chain_data(chain, manifests)?,
        ),
        MvsSelectionReason::PathPin { chain } => {
            ("path_pin", requirement_chain_data(chain, manifests)?)
        }
        MvsSelectionReason::RequirementEdge { chain } => {
            ("requirement", requirement_chain_data(chain, manifests)?)
        }
        MvsSelectionReason::LockFloor { package_version } => {
            ("lock_floor", lock_floor_chain_data(package_version))
        }
    };
    let mut data = ValueObject::new();
    data.insert(
        "package".to_owned(),
        Value::String(node.package().to_owned()),
    );
    data.insert(
        "compatibility_line".to_owned(),
        Value::String(node.line().to_string()),
    );
    data.insert(
        "selected_version".to_owned(),
        Value::String(conflict.target.version().to_string()),
    );
    data.insert(
        "named_version".to_owned(),
        Value::String(conflict.named_target.version().to_string()),
    );
    data.insert(
        "requirement".to_owned(),
        Value::String(conflict.requirement.to_string()),
    );
    data.insert(
        "requirer".to_owned(),
        Value::String(conflict.requirer.package_version().package().to_owned()),
    );
    data.insert(
        "requirer_version".to_owned(),
        Value::String(conflict.requirer.package_version().version().to_string()),
    );
    data.insert(
        "requirer_is_root".to_owned(),
        Value::Bool(matches!(&conflict.requirer, MvsRequirer::Root(_))),
    );
    data.insert(
        "selected_reason".to_owned(),
        Value::String(selected_reason.to_owned()),
    );
    data.insert("selected_chain".to_owned(), selected_chain);
    data.insert(
        "requirement_chain".to_owned(),
        requirement_chain_data(&explanation.requirement_chain, manifests)?,
    );
    Ok(data)
}

fn lock_floor_chain_data(package_version: &MvsPackageVersion) -> Value {
    let mut origin = ValueObject::new();
    origin.insert("kind".to_owned(), Value::String("lock_floor".to_owned()));
    origin.insert(
        "package".to_owned(),
        Value::String(package_version.package().to_owned()),
    );
    origin.insert(
        "version".to_owned(),
        Value::String(package_version.version().to_string()),
    );
    let mut chain = ValueObject::new();
    chain.insert("origin".to_owned(), Value::Object(origin));
    chain.insert("steps".to_owned(), Value::Array(Vec::new()));
    Value::Object(chain)
}

fn requirement_chain_data(
    chain: &MvsRequirementChain,
    manifests: &MvsManifestLocations,
) -> Result<Value, MvsDuplicatePackageLineDiagnosticError> {
    let mut origin = ValueObject::new();
    match &chain.origin {
        MvsChainOrigin::Root(package_version) => {
            origin.insert("kind".to_owned(), Value::String("root".to_owned()));
            origin.insert(
                "package".to_owned(),
                Value::String(package_version.package().to_owned()),
            );
            origin.insert(
                "version".to_owned(),
                Value::String(package_version.version().to_string()),
            );
        }
        MvsChainOrigin::LockFloor(package_version) => {
            origin.insert("kind".to_owned(), Value::String("lock_floor".to_owned()));
            origin.insert(
                "package".to_owned(),
                Value::String(package_version.package().to_owned()),
            );
            origin.insert(
                "version".to_owned(),
                Value::String(package_version.version().to_string()),
            );
        }
    }
    let steps = requirement_chain_indexes(chain.links.len())
        .into_iter()
        .map(|index| {
            let Some(index) = index else {
                let mut omitted = ValueObject::new();
                omitted.insert("kind".to_owned(), Value::String("omitted".to_owned()));
                return Ok(Value::Object(omitted));
            };
            let link = &chain.links[index];
            let location = chain_link_location(
                manifests,
                &link.requirer,
                &link.named_target,
                &link.requirement,
                &link.span,
            )?;
            let mut fields = ValueObject::new();
            fields.insert("kind".to_owned(), Value::String("requirement".to_owned()));
            fields.insert(
                "requirer".to_owned(),
                Value::String(link.requirer.package_version().package().to_owned()),
            );
            fields.insert(
                "requirer_version".to_owned(),
                Value::String(link.requirer.package_version().version().to_string()),
            );
            fields.insert(
                "requirer_is_root".to_owned(),
                Value::Bool(matches!(&link.requirer, MvsRequirer::Root(_))),
            );
            fields.insert(
                "requirer_selected".to_owned(),
                Value::Bool(link.requirer_selected),
            );
            fields.insert(
                "target".to_owned(),
                Value::String(link.named_target.package().to_owned()),
            );
            fields.insert(
                "target_version".to_owned(),
                Value::String(link.named_target.version().to_string()),
            );
            fields.insert(
                "requirement".to_owned(),
                Value::String(link.requirement.to_string()),
            );
            fields.insert("location".to_owned(), location_data(&location));
            Ok(Value::Object(fields))
        })
        .collect::<Result<Vec<_>, MvsDuplicatePackageLineDiagnosticError>>()?;
    let mut fields = ValueObject::new();
    fields.insert("origin".to_owned(), Value::Object(origin));
    fields.insert("steps".to_owned(), Value::Array(steps));
    Ok(Value::Object(fields))
}

fn requirer_display(requirer: &MvsRequirer) -> String {
    let package_version = requirer.package_version();
    format!(
        "{} {}{}",
        package_version.package(),
        package_version.version(),
        if matches!(requirer, MvsRequirer::Root(_)) {
            " (root)"
        } else {
            ""
        }
    )
}

fn add_selection_reason_children(
    diagnostic: &mut Diagnostic,
    reason: &MvsSelectionReason,
    manifests: &MvsManifestLocations,
) -> Result<(), MvsDuplicatePackageLineDiagnosticError> {
    match reason {
        MvsSelectionReason::RootOverridePin { chain } => {
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: "the selected version is pinned by a root path override:".to_owned(),
                    location: None,
                })
                .expect("a new diagnostic has no snapshot-bound locations");
            add_requirement_chain_children(diagnostic, chain, manifests)?;
        }
        MvsSelectionReason::PathPin { chain } => {
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: "the selected version is pinned by this path requirement:".to_owned(),
                    location: None,
                })
                .expect("a new diagnostic has no snapshot-bound locations");
            add_requirement_chain_children(diagnostic, chain, manifests)?;
        }
        MvsSelectionReason::RequirementEdge { chain } => {
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: "the selected version is the highest minimum named by:".to_owned(),
                    location: None,
                })
                .expect("a new diagnostic has no snapshot-bound locations");
            add_requirement_chain_children(diagnostic, chain, manifests)?;
        }
        MvsSelectionReason::LockFloor { package_version } => {
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: format!(
                        "the selected version comes from lock entry {} {} (`sai.lock`)",
                        package_version.package(),
                        package_version.version()
                    ),
                    location: None,
                })
                .expect("a new diagnostic has no snapshot-bound locations");
        }
    }
    Ok(())
}

fn add_requirement_chain_children(
    diagnostic: &mut Diagnostic,
    chain: &MvsRequirementChain,
    manifests: &MvsManifestLocations,
) -> Result<(), MvsDuplicatePackageLineDiagnosticError> {
    match &chain.origin {
        MvsChainOrigin::Root(root) => diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: format!(
                    "chain starts at {} {} (root)",
                    root.package(),
                    root.version()
                ),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations"),
        MvsChainOrigin::LockFloor(floor) => diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: format!(
                    "chain starts at lock entry {} {} (`sai.lock`)",
                    floor.package(),
                    floor.version()
                ),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations"),
    }
    let indexes = requirement_chain_indexes(chain.links.len());
    for index in indexes {
        let Some(index) = index else {
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: "…".to_owned(),
                    location: None,
                })
                .expect("a new diagnostic has no snapshot-bound locations");
            continue;
        };
        let link = &chain.links[index];
        let location = chain_link_location(
            manifests,
            &link.requirer,
            &link.named_target,
            &link.requirement,
            &link.span,
        )?;
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: format!(
                    "{}{} requires {} @ {} (names {})",
                    requirer_display(&link.requirer),
                    if link.requirer_selected {
                        ""
                    } else {
                        " (not selected)"
                    },
                    link.named_target.package(),
                    link.requirement,
                    link.named_target.version()
                ),
                location: Some(location),
            })
            .expect("a new diagnostic has no snapshot-bound locations");
    }
    Ok(())
}

fn requirement_chain_steps(
    chain: &MvsRequirementChain,
    manifests: &MvsManifestLocations,
) -> Result<Vec<ChainStep>, MvsDuplicatePackageLineDiagnosticError> {
    let mut steps = Vec::with_capacity(chain.links.len() + 1);
    for (index, link) in chain.links.iter().enumerate() {
        let location = chain_link_location(
            manifests,
            &link.requirer,
            &link.named_target,
            &link.requirement,
            &link.span,
        )?;
        let name = if link.requirer_selected {
            requirer_display(&link.requirer)
        } else {
            format!("{} (not selected)", requirer_display(&link.requirer))
        };
        steps.push(ChainStep {
            name,
            symbol: None,
            location: Some(location),
            relation: Relation::Requires,
        });
        if index + 1 < chain.links.len() {
            debug_assert_eq!(
                link.named_target,
                *chain.links[index + 1].requirer.package_version()
            );
        }
    }
    let final_link = chain
        .links
        .last()
        .expect("a validated requirement chain ends with an edge");
    steps.push(ChainStep {
        name: format!(
            "{} @ {} (names {})",
            final_link.named_target.package(),
            final_link.requirement,
            final_link.named_target.version()
        ),
        symbol: None,
        location: None,
        relation: Relation::Requires,
    });

    if steps.len() <= 32 {
        return Ok(steps);
    }
    let mut bounded = steps[..16].to_vec();
    bounded.push(ChainStep {
        name: "…".to_owned(),
        symbol: None,
        location: None,
        relation: Relation::Calls,
    });
    bounded.extend_from_slice(&steps[steps.len() - 15..]);
    Ok(bounded)
}

fn requirement_chain_indexes(len: usize) -> Vec<Option<usize>> {
    if len <= 32 {
        return (0..len).map(Some).collect();
    }
    (0..16)
        .map(Some)
        .chain(std::iter::once(None))
        .chain((len - 15..len).map(Some))
        .collect()
}

/// Builds the graph-wide `DUPLICATE_PACKAGE_LINE` lint and its MVS chains.
///
/// A suppressed lint returns `Ok(None)`. Each edge location uses the original
/// byte span from the package parser and the matching source/file identity
/// supplied in `context.manifests`.
pub fn duplicate_package_line_diagnostic(
    explanations: &MvsExplanationGraph<'_>,
    duplicate: &MvsDuplicatePackageLines,
    context: &MvsDuplicatePackageLineDiagnosticContext<'_>,
) -> Result<Option<Diagnostic>, MvsDuplicatePackageLineDiagnosticError> {
    let root_manifest = context
        .manifests
        .root
        .as_ref()
        .ok_or(MvsDuplicatePackageLineDiagnosticError::MissingRootManifest)?;
    let selection_root = explanations.root_package_version();
    if root_manifest.package_version != *selection_root {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::ManifestVersionMismatch {
                requirer: Box::new(MvsRequirer::Root(selection_root.clone())),
                expected: Box::new(selection_root.clone()),
                found: Box::new(root_manifest.package_version.clone()),
            },
        );
    }

    let explanation = explanations
        .explain_duplicate_lines(duplicate)
        .map_err(|error| {
            MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
        })?;

    let no_attributes = [];
    let root_policy = root_manifest.lint_policies();
    let manifest_level = root_manifest
        .manifest_lint_levels
        .get("DUPLICATE_PACKAGE_LINE");
    let resolution = resolve_lint_occurrence(
        "DUPLICATE_PACKAGE_LINE",
        &LintResolutionContext {
            primary: None,
            attributes: &no_attributes,
            cli: context.cli,
            manifest: manifest_level,
            package_policy: root_policy,
            root_policy,
            is_root_or_workspace_member: true,
            deny_warnings: context.deny_warnings,
        },
    )
    .map_err(MvsDuplicatePackageLineDiagnosticError::LintResolution)?;

    let LintResolution::Emitted { severity, level } = resolution else {
        return Ok(None);
    };

    let mut diagnostic = Diagnostic::new(
        Code::from_name("DUPLICATE_PACKAGE_LINE")
            .expect("DUPLICATE_PACKAGE_LINE is registered as a lint"),
        severity,
        "one package is present on multiple compatibility lines in the graph",
        None,
    );
    diagnostic.data = duplicate_lines_data(&explanation, context.manifests)?;
    add_line_children(&mut diagnostic, &explanation, context.manifests)?;
    diagnostic
        .set_level(Some(level))
        .expect("a new diagnostic has no snapshot-bound locations");

    Ok(Some(diagnostic))
}

/// Builds every graph-wide `DUPLICATE_PACKAGE_LINE` diagnostic for a selection.
///
/// The selected graph and duplicate findings are derived from the same
/// selection used to build explanations, so callers cannot accidentally pair
/// a finding with a different graph.
pub fn duplicate_package_line_diagnostics(
    selection: &MvsSelectionOutput,
    context: &MvsDuplicatePackageLineDiagnosticContext<'_>,
) -> Result<Vec<Diagnostic>, MvsDuplicatePackageLineDiagnosticError> {
    if let Some(root_manifest) = context.manifests.root.as_ref() {
        if root_manifest.package_version != *selection.root() {
            return Err(
                MvsDuplicatePackageLineDiagnosticError::ManifestVersionMismatch {
                    requirer: Box::new(MvsRequirer::Root(selection.root().clone())),
                    expected: Box::new(selection.root().clone()),
                    found: Box::new(root_manifest.package_version.clone()),
                },
            );
        }
    }
    let explanations = MvsExplanationGraph::new(selection).map_err(|error| {
        MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
    })?;
    let selected_graph =
        check_selected_graph(selection.root_contract(), selection).map_err(|error| {
            MvsDuplicatePackageLineDiagnosticError::InvalidSelectedGraph(Box::new(error))
        })?;
    duplicate_package_lines(&selected_graph)
        .iter()
        .map(|duplicate| duplicate_package_line_diagnostic(&explanations, duplicate, context))
        .collect::<Result<Vec<_>, _>>()
        .map(|diagnostics| diagnostics.into_iter().flatten().collect())
}

fn package_version_name(package_version: &MvsPackageVersion) -> String {
    format!(
        "{}@{}",
        package_version.package(),
        package_version.version()
    )
}

fn manifest_path_expectation(package_version: &MvsPackageVersion, role: PackageRole) -> String {
    match role {
        PackageRole::Root => "workspace-relative path ending in `sai.package`".to_owned(),
        PackageRole::Dependency => {
            format!("dep:{}/sai.package", package_version_name(package_version))
        }
    }
}

fn is_manifest_path(path: &str, package_version: &MvsPackageVersion, role: PackageRole) -> bool {
    match role {
        PackageRole::Root => {
            !path.starts_with("dep:")
                && path.rsplit('/').next() == Some("sai.package")
                && path != "<stdin>"
        }
        PackageRole::Dependency => {
            path == format!("dep:{}/sai.package", package_version_name(package_version))
        }
    }
}

fn is_final_manifest_path(path: &str, package_version: &MvsPackageVersion) -> bool {
    let prefix = format!(
        "dep:{}/.sai/final-source-",
        package_version_name(package_version)
    );
    let Some(ordinal) = path
        .strip_prefix(&prefix)
        .and_then(|path| path.strip_suffix("/sai.package"))
    else {
        return false;
    };
    !ordinal.is_empty()
        && ordinal.bytes().all(|byte| byte.is_ascii_digit())
        && (ordinal == "0" || !ordinal.starts_with('0'))
}

fn duplicate_lines_data(
    explanation: &crate::mvs_explanations::MvsDuplicatePackageLinesExplanation,
    manifests: &MvsManifestLocations,
) -> Result<ValueObject, MvsDuplicatePackageLineDiagnosticError> {
    let lines = explanation
        .lines
        .iter()
        .map(|line| {
            let chain = line
                .chain
                .links
                .iter()
                .map(|link| {
                    let location = chain_link_location(
                        manifests,
                        &link.requirer,
                        &link.named_target,
                        &link.requirement,
                        &link.span,
                    )?;
                    let is_root = matches!(&link.requirer, MvsRequirer::Root(_));
                    let mut fields = ValueObject::new();
                    fields.insert(
                        "requirer".to_owned(),
                        Value::String(link.requirer.package_version().package().to_owned()),
                    );
                    fields.insert(
                        "requirer_version".to_owned(),
                        Value::String(link.requirer.package_version().version().to_string()),
                    );
                    fields.insert("requirer_is_root".to_owned(), Value::Bool(is_root));
                    fields.insert(
                        "requirer_selected".to_owned(),
                        Value::Bool(link.requirer_selected),
                    );
                    fields.insert(
                        "target".to_owned(),
                        Value::String(link.named_target.package().to_owned()),
                    );
                    fields.insert(
                        "target_version".to_owned(),
                        Value::String(link.named_target.version().to_string()),
                    );
                    fields.insert(
                        "requirement".to_owned(),
                        Value::String(link.requirement.to_string()),
                    );
                    fields.insert("location".to_owned(), location_data(&location));
                    Ok(Value::Object(fields))
                })
                .collect::<Result<Vec<_>, MvsDuplicatePackageLineDiagnosticError>>()?;

            let mut fields = ValueObject::new();
            fields.insert(
                "version".to_owned(),
                Value::String(line.package_version.version().to_string()),
            );
            match &line.chain.origin {
                MvsChainOrigin::Root(origin) => {
                    fields.insert("origin".to_owned(), Value::String("root".to_owned()));
                    fields.insert(
                        "origin_package".to_owned(),
                        Value::String(origin.package().to_owned()),
                    );
                    fields.insert(
                        "origin_version".to_owned(),
                        Value::String(origin.version().to_string()),
                    );
                }
                MvsChainOrigin::LockFloor(origin) => {
                    fields.insert("origin".to_owned(), Value::String("lock_entry".to_owned()));
                    fields.insert(
                        "origin_package".to_owned(),
                        Value::String(origin.package().to_owned()),
                    );
                    fields.insert(
                        "origin_version".to_owned(),
                        Value::String(origin.version().to_string()),
                    );
                }
            }
            fields.insert("chain".to_owned(), Value::Array(chain));
            Ok(Value::Object(fields))
        })
        .collect::<Result<Vec<_>, MvsDuplicatePackageLineDiagnosticError>>()?;

    let mut data = ValueObject::new();
    data.insert(
        "package".to_owned(),
        Value::String(explanation.package.to_string()),
    );
    data.insert("lines".to_owned(), Value::Array(lines));
    Ok(data)
}

fn add_line_children(
    diagnostic: &mut Diagnostic,
    explanation: &crate::mvs_explanations::MvsDuplicatePackageLinesExplanation,
    manifests: &MvsManifestLocations,
) -> Result<(), MvsDuplicatePackageLineDiagnosticError> {
    for line in &explanation.lines {
        let is_root_line = matches!(
            &line.chain.origin,
            MvsChainOrigin::Root(root) if root == &line.package_version
        );
        diagnostic
            .push_child(Child {
                kind: ChildKind::Note,
                message: format!(
                    "{} {}{}",
                    line.package_version.package(),
                    line.package_version.version(),
                    if is_root_line { " (root)" } else { "" }
                ),
                location: None,
            })
            .expect("a new diagnostic has no snapshot-bound locations");

        if let MvsChainOrigin::LockFloor(origin) = &line.chain.origin {
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: format!(
                        "lock entry {} {} (`sai.lock`)",
                        origin.package(),
                        origin.version()
                    ),
                    location: None,
                })
                .expect("a new diagnostic has no snapshot-bound locations");
        }

        for link in &line.chain.links {
            let location = chain_link_location(
                manifests,
                &link.requirer,
                &link.named_target,
                &link.requirement,
                &link.span,
            )?;
            let requirer = match &link.requirer {
                MvsRequirer::Root(package_version) => format!(
                    "{} {} (root)",
                    package_version.package(),
                    package_version.version()
                ),
                MvsRequirer::Package(package_version) => format!(
                    "{} {}",
                    package_version.package(),
                    package_version.version()
                ),
            };
            let not_selected = if link.requirer_selected {
                ""
            } else {
                " (not selected)"
            };
            diagnostic
                .push_child(Child {
                    kind: ChildKind::Note,
                    message: format!(
                        "{requirer}{not_selected} requires {} @ {}",
                        link.named_target.package(),
                        link.requirement
                    ),
                    location: Some(location),
                })
                .expect("a new diagnostic has no snapshot-bound locations");
        }
    }
    Ok(())
}

fn chain_link_location(
    manifests: &MvsManifestLocations,
    requirer: &MvsRequirer,
    named_target: &MvsPackageVersion,
    requirement: &VersionReq,
    range: &Range<usize>,
) -> Result<sai_diagnostics::Location, MvsDuplicatePackageLineDiagnosticError> {
    let Some(manifest) = manifests.for_requirer(requirer) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::MissingManifestLocation(Box::new(
                requirer.clone(),
            )),
        );
    };
    let package_version = requirer.package_version();
    let expected_package = format!(
        "{}@{}",
        package_version.package(),
        package_version.version()
    );
    if manifest.source.package.as_deref() != Some(expected_package.as_str()) {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::ManifestPackageMismatch {
                requirer: Box::new(requirer.clone()),
                expected: expected_package,
                found: manifest.source.package.clone(),
            },
        );
    }
    if manifest.package_version != *package_version {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::ManifestVersionMismatch {
                requirer: Box::new(requirer.clone()),
                expected: Box::new(package_version.clone()),
                found: Box::new(manifest.package_version.clone()),
            },
        );
    }
    let expected_role = match requirer {
        MvsRequirer::Root(_) => PackageRole::Root,
        MvsRequirer::Package(_) => PackageRole::Dependency,
    };
    if manifest.role != expected_role {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::ManifestRoleMismatch {
                expected: expected_role,
                found: manifest.role,
            },
        );
    }
    if range.start > range.end || range.end > manifest.source_len {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::SourceSpanOutOfRange {
                requirer: Box::new(requirer.clone()),
                span: range.clone(),
            },
        );
    }
    let Some(source_requirement) = manifest.requirements.get(&(range.start, range.end)) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::RequirementNotInManifest {
                requirer: Box::new(requirer.clone()),
                span: range.clone(),
            },
        );
    };
    if source_requirement.package.as_ref() != named_target.package()
        || source_requirement.version != *requirement
    {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::RequirementMismatch {
                requirer: Box::new(requirer.clone()),
                span: range.clone(),
                expected_package: named_target.package().into(),
                expected_requirement: Box::new(requirement.clone()),
                found_package: source_requirement.package.clone(),
                found_requirement: Box::new(source_requirement.version.clone()),
            },
        );
    }
    let Some(start) = TextSize::from_usize(range.start) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::SourceSpanOutOfRange {
                requirer: Box::new(requirer.clone()),
                span: range.clone(),
            },
        );
    };
    let Some(end) = TextSize::from_usize(range.end) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::SourceSpanOutOfRange {
                requirer: Box::new(requirer.clone()),
                span: range.clone(),
            },
        );
    };
    let Some(range) = TextRange::new(start, end) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::SourceSpanOutOfRange {
                requirer: Box::new(requirer.clone()),
                span: range.clone(),
            },
        );
    };
    Ok(sai_diagnostics::Location {
        file: manifest.source.clone(),
        span: sai_diagnostics::Span::new(manifest.file_id, range),
    })
}

fn divergent_sources_data<K, E>(
    comparison: &ResolverReloadedContractComparison<K, E>,
    read_source: &str,
    final_source: &str,
) -> ValueObject {
    let package_version = &comparison.reload.package_version;
    let mut data = ValueObject::new();
    data.insert(
        "package".to_owned(),
        Value::String(package_version.package().to_owned()),
    );
    data.insert(
        "version".to_owned(),
        Value::String(package_version.version().to_string()),
    );
    data.insert(
        "read_source".to_owned(),
        Value::String(read_source.to_owned()),
    );
    data.insert(
        "final_source".to_owned(),
        Value::String(final_source.to_owned()),
    );
    data.insert("source_identities_differ".to_owned(), Value::Bool(true));
    data.insert(
        "read_contract_edges".to_owned(),
        Value::Array(
            comparison
                .edges
                .expected_edges()
                .iter()
                .map(resolver_contract_edge_data)
                .collect(),
        ),
    );
    data.insert(
        "final_contract_edges".to_owned(),
        Value::Array(
            comparison
                .edges
                .actual_edges()
                .iter()
                .map(resolver_contract_edge_data)
                .collect(),
        ),
    );
    data.insert(
        "edge_comparison".to_owned(),
        Value::String(comparison_status_name(comparison.edges.status()).to_owned()),
    );
    data.insert(
        "path_comparison".to_owned(),
        Value::String(comparison_status_name(comparison.paths.status()).to_owned()),
    );
    data.insert(
        "path_source_differences".to_owned(),
        Value::Array(
            comparison
                .paths
                .path_differences()
                .iter()
                .map(|difference| {
                    let mut fields = ValueObject::new();
                    fields.insert(
                        "package".to_owned(),
                        Value::String(difference.package.to_string()),
                    );
                    fields.insert(
                        "read_time".to_owned(),
                        resolver_path_request_data(&difference.expected),
                    );
                    fields.insert(
                        "final_source".to_owned(),
                        resolver_path_request_data(&difference.actual),
                    );
                    Value::Object(fields)
                })
                .collect(),
        ),
    );
    data
}

fn comparison_status_name(
    status: crate::resolver_contract::ResolverContractComparisonStatus,
) -> &'static str {
    use crate::resolver_contract::ResolverContractComparisonStatus;
    match status {
        ResolverContractComparisonStatus::Equivalent => "equivalent",
        ResolverContractComparisonStatus::Different => "different",
        ResolverContractComparisonStatus::RequiresSourceBoundPathResolution => {
            "requires_source_bound_path_resolution"
        }
        ResolverContractComparisonStatus::PathResolutionFailed => "path_resolution_failed",
    }
}

fn resolver_contract_edge_data(edge: &ResolverContractEdge) -> Value {
    let mut fields = ValueObject::new();
    fields.insert(
        "package".to_owned(),
        Value::String(edge.package.to_string()),
    );
    fields.insert(
        "requirement".to_owned(),
        Value::String(edge.requirement.to_string()),
    );
    fields.insert(
        "named_version".to_owned(),
        Value::String(edge.named_version.to_string()),
    );
    fields.insert(
        "source".to_owned(),
        edge.source.as_ref().map_or(Value::Null, |source| {
            Value::String(resolver_contract_source_display(source))
        }),
    );
    fields.insert(
        "only".to_owned(),
        string_set_value(
            edge.only
                .as_ref()
                .map(|items| items.iter().map(AsRef::as_ref)),
        ),
    );
    fields.insert(
        "grant".to_owned(),
        string_set_value(
            edge.grant
                .as_ref()
                .map(|items| items.iter().map(AsRef::as_ref)),
        ),
    );
    Value::Object(fields)
}

fn resolver_path_request_data(
    request: &crate::resolver_contract::ResolverContractPathSourceRequest,
) -> Value {
    let mut fields = ValueObject::new();
    fields.insert(
        "requiring_package".to_owned(),
        Value::String(request.requiring_package.package().to_owned()),
    );
    fields.insert(
        "requiring_version".to_owned(),
        Value::String(request.requiring_package.version().to_string()),
    );
    fields.insert(
        "root_relative".to_owned(),
        Value::Bool(request.root_relative),
    );
    fields.insert("path".to_owned(), Value::String(request.path.to_string()));
    Value::Object(fields)
}

fn resolver_path_request_display(
    request: &crate::resolver_contract::ResolverContractPathSourceRequest,
) -> String {
    format!(
        "path {:?} relative to {}@{}{}",
        request.path,
        request.requiring_package.package(),
        request.requiring_package.version(),
        if request.root_relative {
            " in the root source tree"
        } else {
            ""
        }
    )
}

fn string_set_value<'a>(values: Option<impl Iterator<Item = &'a str>>) -> Value {
    values.map_or(Value::Null, |values| {
        Value::Array(
            values
                .map(|value| Value::String(value.to_owned()))
                .collect(),
        )
    })
}

fn resolver_contract_source_display(source: &ResolverContractSourceClause) -> String {
    match source {
        ResolverContractSourceClause::Git {
            canonical_url,
            directory,
        } => directory.as_ref().map_or_else(
            || format!("git {canonical_url:?}"),
            |directory| format!("git {canonical_url:?} dir {directory:?}"),
        ),
        ResolverContractSourceClause::Path {
            relative_to,
            root_relative,
            path,
        } => format!(
            "path {path:?} relative to {}@{}{}",
            relative_to.package(),
            relative_to.version(),
            if *root_relative { " (root)" } else { "" }
        ),
    }
}

fn resolver_contract_edge_display(edge: &ResolverContractEdge) -> String {
    let source = edge.source.as_ref().map_or_else(
        || "no source clause".to_owned(),
        resolver_contract_source_display,
    );
    let only = edge.only.as_ref().map_or_else(
        || "omitted".to_owned(),
        |items| {
            items
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
    let grant = edge.grant.as_ref().map_or_else(
        || "omitted".to_owned(),
        |items| {
            items
                .iter()
                .map(AsRef::as_ref)
                .collect::<Vec<_>>()
                .join(", ")
        },
    );
    format!(
        "{} @ {} (names {}; source: {}; only: {}; grant: {})",
        edge.package, edge.requirement, edge.named_version, source, only, grant
    )
}

fn resolver_package_source_display(source: &ResolverPackageSource) -> String {
    match source {
        ResolverPackageSource::Git { url, directory } => directory.as_ref().map_or_else(
            || format!("git {url:?}"),
            |directory| format!("git {url:?} dir {directory:?}"),
        ),
        ResolverPackageSource::Path { relative_to, path } => format!(
            "path {path:?} relative to {}@{}",
            relative_to.package(),
            relative_to.version()
        ),
    }
}

fn resolver_source_selection_reason(selection: &ResolverSourceSelection) -> String {
    match (selection.reason(), selection.provenance()) {
        (
            ResolverSourceReason::RootPathOverride,
            ResolverSourceProvenance::RootPathOverride { .. },
        ) => format!(
            "the root path override for {}@{} selected this source",
            selection.package_version().package(),
            selection.package_version().version()
        ),
        (
            ResolverSourceReason::RootGitOverride,
            ResolverSourceProvenance::RootGitOverride { .. },
        ) => format!(
            "the root Git override for {} selected this source",
            selection.package_version().package()
        ),
        (ResolverSourceReason::PathPin, ResolverSourceProvenance::PathPin { pin }) => format!(
            "the path requirement edge from {} to {}@{} selected it",
            requirer_display(&pin.requirer),
            selection.package_version().package(),
            selection.package_version().version()
        ),
        (
            ResolverSourceReason::NamedRequirement,
            ResolverSourceProvenance::NamedRequirement { edge },
        ) => {
            format!(
                "the requirement edge from {} to {}@{} selected it",
                requirer_display(&edge.requirer),
                selection.package_version().package(),
                selection.package_version().version()
            )
        }
        (ResolverSourceReason::LockFloor, ResolverSourceProvenance::LockFloor { .. }) => {
            format!(
                "the lock-file floor for {}@{} selected this source",
                selection.package_version().package(),
                selection.package_version().version()
            )
        }
        _ => "the source-selection provenance is inconsistent".to_owned(),
    }
}

fn resolver_source_selection_location(
    selection: &ResolverSourceSelection,
    manifests: &MvsManifestLocations,
) -> Result<Option<sai_diagnostics::Location>, MvsDuplicatePackageLineDiagnosticError> {
    let location = match selection.provenance() {
        ResolverSourceProvenance::RootPathOverride { root_package, span }
        | ResolverSourceProvenance::RootGitOverride { root_package, span } => {
            let Some(manifest) = manifests.root() else {
                return Err(MvsDuplicatePackageLineDiagnosticError::MissingRootManifest);
            };
            let requirer = MvsRequirer::Root(root_package.clone());
            let location = manifest_span_location(manifest, &requirer, span)?;
            let expected = format!(
                "{} from {}",
                selection.package_version().package(),
                resolver_package_source_display(selection.source())
            );
            let found_override = manifest.overrides.get(&(span.start, span.end));
            let found_source = found_override
                .and_then(|item| resolver_package_source_from_override(root_package, item));
            if !found_override.is_some_and(|item| {
                item.package.as_ref() == selection.package_version().package()
                    && found_source.as_ref() == Some(selection.source())
            }) {
                return Err(
                    MvsDuplicatePackageLineDiagnosticError::SourceSelectionDoesNotMatchManifest {
                        requirer: Box::new(requirer.clone()),
                        span: span.clone(),
                        expected,
                        found: found_override.map(|item| {
                            format!(
                                "{} from {}",
                                item.package,
                                found_source.as_ref().map_or_else(
                                    || "an artifact or unsupported source".to_owned(),
                                    resolver_package_source_display
                                )
                            )
                        }),
                    },
                );
            }
            Some(location)
        }
        ResolverSourceProvenance::PathPin { pin } => Some(named_source_requirement_location(
            manifests,
            &pin.requirer,
            selection.package_version(),
            selection.source(),
            &pin.span,
        )?),
        ResolverSourceProvenance::NamedRequirement { edge } => {
            Some(named_source_requirement_location(
                manifests,
                &edge.requirer,
                selection.package_version(),
                selection.source(),
                &edge.span,
            )?)
        }
        ResolverSourceProvenance::LockFloor { .. } => None,
    };
    Ok(location)
}

fn named_source_requirement_location(
    manifests: &MvsManifestLocations,
    requirer: &MvsRequirer,
    target: &MvsPackageVersion,
    selected_source: &ResolverPackageSource,
    span: &Range<usize>,
) -> Result<sai_diagnostics::Location, MvsDuplicatePackageLineDiagnosticError> {
    let Some(manifest) = manifests.for_requirer(requirer) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::MissingManifestLocation(Box::new(
                requirer.clone(),
            )),
        );
    };
    let Some(requirement) = manifest.requirements.get(&(span.start, span.end)) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::RequirementNotInManifest {
                requirer: Box::new(requirer.clone()),
                span: span.clone(),
            },
        );
    };
    let location = chain_link_location(manifests, requirer, target, &requirement.version, span)?;
    let found_source = requirement
        .source
        .as_ref()
        .map(|source| resolver_package_source_from_requirement(requirer.package_version(), source));
    if found_source.as_ref() != Some(selected_source) {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::SourceSelectionDoesNotMatchManifest {
                requirer: Box::new(requirer.clone()),
                span: span.clone(),
                expected: format!(
                    "{} from {}",
                    target.package(),
                    resolver_package_source_display(selected_source)
                ),
                found: found_source.as_ref().map(|source| {
                    format!(
                        "{} from {}",
                        requirement.package,
                        resolver_package_source_display(source)
                    )
                }),
            },
        );
    }
    Ok(location)
}

fn resolver_package_source_from_requirement(
    requiring_package: &MvsPackageVersion,
    source: &RequirementSource,
) -> ResolverPackageSource {
    match source {
        RequirementSource::Git { url, directory } => ResolverPackageSource::Git {
            url: url.clone(),
            directory: directory.clone(),
        },
        RequirementSource::Path(path) => ResolverPackageSource::Path {
            relative_to: requiring_package.clone(),
            path: path.clone(),
        },
    }
}

fn resolver_package_source_from_override(
    root_package: &MvsPackageVersion,
    item: &ManifestOverride,
) -> Option<ResolverPackageSource> {
    match &item.source {
        OverrideSource::Artifact => None,
        OverrideSource::Path(path) => Some(ResolverPackageSource::Path {
            relative_to: root_package.clone(),
            path: path.clone(),
        }),
        OverrideSource::Git { url, directory } => Some(ResolverPackageSource::Git {
            url: url.clone(),
            directory: directory.clone(),
        }),
    }
}

fn manifest_span_location(
    manifest: &MvsManifestLocation,
    requirer: &MvsRequirer,
    span: &Range<usize>,
) -> Result<sai_diagnostics::Location, MvsDuplicatePackageLineDiagnosticError> {
    let package_version = requirer.package_version();
    let expected_package = format!(
        "{}@{}",
        package_version.package(),
        package_version.version()
    );
    if manifest.source.package.as_deref() != Some(expected_package.as_str()) {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::ManifestPackageMismatch {
                requirer: Box::new(requirer.clone()),
                expected: expected_package,
                found: manifest.source.package.clone(),
            },
        );
    }
    if manifest.package_version != *package_version {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::ManifestVersionMismatch {
                requirer: Box::new(requirer.clone()),
                expected: Box::new(package_version.clone()),
                found: Box::new(manifest.package_version.clone()),
            },
        );
    }
    let expected_role = match requirer {
        MvsRequirer::Root(_) => PackageRole::Root,
        MvsRequirer::Package(_) => PackageRole::Dependency,
    };
    if manifest.role != expected_role {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::ManifestRoleMismatch {
                expected: expected_role,
                found: manifest.role,
            },
        );
    }
    if span.start > span.end || span.end > manifest.source_len {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::SourceSpanOutOfRange {
                requirer: Box::new(requirer.clone()),
                span: span.clone(),
            },
        );
    }
    let Some(start) = TextSize::from_usize(span.start) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::SourceSpanOutOfRange {
                requirer: Box::new(requirer.clone()),
                span: span.clone(),
            },
        );
    };
    let Some(end) = TextSize::from_usize(span.end) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::SourceSpanOutOfRange {
                requirer: Box::new(requirer.clone()),
                span: span.clone(),
            },
        );
    };
    let Some(range) = TextRange::new(start, end) else {
        return Err(
            MvsDuplicatePackageLineDiagnosticError::SourceSpanOutOfRange {
                requirer: Box::new(requirer.clone()),
                span: span.clone(),
            },
        );
    };
    Ok(sai_diagnostics::Location {
        file: manifest.source.clone(),
        span: sai_diagnostics::Span::new(manifest.file_id, range),
    })
}

fn location_data(location: &sai_diagnostics::Location) -> Value {
    let mut fields = ValueObject::new();
    fields.insert("path".to_owned(), Value::String(location.file.path.clone()));
    fields.insert(
        "package".to_owned(),
        location
            .file
            .package
            .as_ref()
            .map_or(Value::Null, |package| Value::String(package.clone())),
    );
    fields.insert(
        "snapshot".to_owned(),
        location
            .file
            .snapshot
            .map_or(Value::Null, |snapshot| Value::Integer(snapshot.into())),
    );
    fields.insert(
        "start".to_owned(),
        Value::Integer(location.span.range.start().to_usize() as u128),
    );
    fields.insert(
        "end".to_owned(),
        Value::Integer(location.span.range.end().to_usize() as u128),
    );
    Value::Object(fields)
}

#[cfg(test)]
mod tests {
    use std::collections::{BTreeSet, HashMap};
    use std::ops::Range;
    use std::sync::Arc;

    use sai_diagnostics::{
        ChildKind, Code, Diagnostic, LevelSource, LintLevel, Location, Related, Relation, Severity,
        SourceRef, Span, TextRange, TextSize, Value, ValueObject,
    };
    use sai_vfs::{SnapshotFile, SourceFile, WorkspaceSnapshot};

    use super::{
        MAX_FINAL_MANIFEST_SYNTAX_ISSUES, MvsDuplicatePackageLineDiagnosticContext,
        MvsDuplicatePackageLineDiagnosticError, MvsManifestLocation, MvsManifestLocations,
        divergent_sources_diagnostic, duplicate_package_line_diagnostic,
        duplicate_package_line_diagnostics, final_contract_audit_failure_is_unexplained,
        final_contract_audit_internal_error, final_manifest_semantic_diagnostic,
        final_manifest_syntax_issues, mixed_source_identity_failure_diagnostics,
        resolver_source_selection_location, sort_and_deduplicate_reload_failure_diagnostics,
        sort_final_source_diagnostics, version_conflict_diagnostic, version_conflict_diagnostics,
    };
    use crate::manifest::{PackageRole, parse, parse_for_role};
    use crate::mvs::{
        MvsContract, MvsContractRequest, MvsEdge, MvsNamedEdge, MvsPackageVersion, MvsRequirer,
        MvsSelectionOutput, select,
    };
    use crate::mvs_explanations::MvsExplanationGraph;
    use crate::mvs_graph_validation::{
        MvsDuplicatePackageLines, check_selected_graph, duplicate_package_lines,
    };
    use crate::resolver_contract::{
        ResolverContractContext, ResolverPathKey, build_resolver_contract,
        compare_resolver_contract_edges,
    };
    use crate::resolver_source::select_resolver_source;
    use crate::resolver_source_provider::{
        ResolverFinalContractAuditFailure, ResolverFinalContractAuditFailureKind,
        ResolverFinalContractPathResolutionError, ResolverReloadedContractComparison,
        ResolverSourceIdentityFailure, ResolverSourceIdentityFailureKind,
        ResolverSourceIdentitySide, ResolverSourceReload,
    };
    use crate::validation::{
        OverrideKind, PackageValidationContext, PackageValidationErrorKind, validate,
    };
    use crate::version::Version;
    use crate::version_req::VersionReq;

    fn version(value: &str) -> Version {
        Version::parse(value).expect("valid test version")
    }

    fn package_version(package: &str, version_: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(package, version(version_)).expect("valid package version")
    }

    #[test]
    fn reload_failure_diagnostics_deduplicate_only_identical_mismatches() {
        let first = package_version("acme:first", "1.0.0");
        let second = package_version("acme:second", "1.0.0");
        let mut failures = vec![
            (
                &first,
                Code::PackageIdentityMismatch,
                "expected 1.0.0, found 2.0.0".to_owned(),
            ),
            (
                &first,
                Code::PackageIdentityMismatch,
                "expected 1.0.0, found 3.0.0".to_owned(),
            ),
            (
                &first,
                Code::PackageIdentityMismatch,
                "expected 1.0.0, found 2.0.0".to_owned(),
            ),
            (
                &second,
                Code::PackageIdentityMismatch,
                "expected 1.0.0, found 2.0.0".to_owned(),
            ),
        ];

        sort_and_deduplicate_reload_failure_diagnostics(&mut failures);

        assert_eq!(failures.len(), 3);
        assert_eq!(failures[0].0, &first);
        assert_eq!(failures[0].2, "expected 1.0.0, found 2.0.0");
        assert_eq!(failures[1].0, &first);
        assert_eq!(failures[1].2, "expected 1.0.0, found 3.0.0");
        assert_eq!(failures[2].0, &second);
    }

    fn edge_with_span(
        package: &str,
        requirement: &str,
        named_version: &str,
        span: Range<usize>,
    ) -> MvsEdge {
        MvsEdge::new(
            package,
            VersionReq::parse(requirement).expect("valid test requirement"),
            version(named_version),
            span,
        )
        .expect("valid edge")
    }

    fn source_span(source: &str, text: &str) -> Range<usize> {
        let start = source.find(text).expect("source text is present");
        start..start + text.len()
    }

    fn manifest_source(
        package: &str,
        version_: &str,
        requirements: &[(&str, &str)],
    ) -> (String, Vec<Range<usize>>) {
        let mut source = format!("package {package} @ {version_}\n");
        let mut spans = Vec::new();
        if !requirements.is_empty() {
            source.push_str("requires {\n");
            for (target, requirement) in requirements {
                source.push_str("    ");
                let start = source.len();
                source.push_str(&format!("{target} @ {requirement}"));
                spans.push(start..source.len());
                source.push('\n');
            }
            source.push_str("}\n");
        }
        (source, spans)
    }

    fn snapshot_file(
        file_id: u32,
        path: String,
        package_version: &MvsPackageVersion,
        source: &str,
    ) -> SnapshotFile {
        snapshot_file_with_document_version(file_id, path, package_version, source, None)
    }

    fn snapshot_file_with_document_version(
        file_id: u32,
        path: String,
        package_version: &MvsPackageVersion,
        source: &str,
        document_version: Option<u64>,
    ) -> SnapshotFile {
        let source = Arc::new(
            SourceFile::decode(sai_diagnostics::FileId::new(file_id), source.as_bytes())
                .expect("test manifest fits in a source file"),
        );
        SnapshotFile::new(
            source,
            path,
            Some(format!(
                "{}@{}",
                package_version.package(),
                package_version.version()
            )),
            document_version,
        )
    }

    #[test]
    fn final_manifest_locations_require_the_exact_retained_path_edge() {
        let package = package_version("acme:target", "1.0.0");
        let source = "package acme:changed @ 9.0.0\n\
                     requires { acme:secret @ ^1 from path \"private\" }\n";
        let requirement = "acme:secret @ ^1 from path \"private\"";
        let start = source.find(requirement).expect("requirement is present");
        let span = start..start + requirement.len();
        let file_id = sai_diagnostics::FileId::new(71);
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(
                SourceFile::decode(file_id, source.as_bytes())
                    .expect("final manifest fits in a source file"),
            ),
            MvsManifestLocation::final_snapshot_path(&package, 3),
            Some("acme:target@1.0.0".to_owned()),
            None,
        )])
        .expect("the final manifest has a canonical synthetic path");
        let final_manifest = MvsManifestLocation::from_final_snapshot(&snapshot, file_id, &package)
            .expect("a changed package header does not invalidate retained source spans");
        let mut manifests = MvsManifestLocations::default();
        manifests
            .insert_final_package(final_manifest)
            .expect("the final manifest belongs to the snapshot");

        let location = manifests
            .final_path_requirement_location(&package, "acme:secret", "private", &span)
            .expect("the exact path edge is snapshot-bound");
        let start = location.span.range.start().to_usize();
        let end = location.span.range.end().to_usize();
        assert_eq!(&source.as_bytes()[start..end], requirement.as_bytes());
        assert_eq!(location.file.package.as_deref(), Some("acme:target@1.0.0"));
        let found_identity = package_version("acme:changed", "9.0.0");
        let identity_location = manifests
            .final_package_identity_location(&package, &found_identity)
            .expect("the final header matches the identity-mismatch error");
        let identity_start = identity_location.span.range.start().to_usize();
        let identity_end = identity_location.span.range.end().to_usize();
        assert_eq!(
            &source.as_bytes()[identity_start..identity_end],
            b"package acme:changed @ 9.0.0"
        );
        assert_eq!(
            manifests.final_package_identity_location(
                &package,
                &package_version("acme:wrong", "9.0.0"),
            ),
            Err(
                MvsDuplicatePackageLineDiagnosticError::FinalManifestIdentityMismatch {
                    expected: Box::new(package.clone()),
                    found: Box::new(package_version("acme:wrong", "9.0.0")),
                }
            )
        );
        assert_eq!(
            manifests.final_path_requirement_location(&package, "acme:secret", "different", &span,),
            Err(
                MvsDuplicatePackageLineDiagnosticError::FinalManifestRequirementMismatch {
                    requiring_package: Box::new(package.clone()),
                    span: span.clone(),
                }
            )
        );

        let wrong_span = span.start + 1..span.end;
        assert!(matches!(
            manifests.final_path_requirement_location(
                &package,
                "acme:secret",
                "private",
                &wrong_span,
            ),
            Err(MvsDuplicatePackageLineDiagnosticError::FinalManifestRequirementMismatch { .. })
        ));
    }

    #[test]
    fn final_manifest_syntax_issues_preserve_bytes_codes_and_expected_tokens() {
        let invalid_bytes = [0xff, b'x', 0xfe];
        let package = package_version("acme:target", "1.0.0");
        let file_id = sai_diagnostics::FileId::new(72);
        let invalid_source = Arc::new(
            SourceFile::decode(file_id, &invalid_bytes)
                .expect("the VFS retains malformed bytes for diagnostics"),
        );
        let invalid_utf8 = parse_for_role(&invalid_bytes, PackageRole::Dependency);
        let invalid_utf8_spans = invalid_source
            .invalid_utf8_sequences()
            .iter()
            .map(|sequence| {
                let span = invalid_source
                    .to_original_range(sequence.decoded_range())
                    .expect("the malformed byte range maps to its original source");
                span.start().to_usize()..span.end().to_usize()
            })
            .collect::<Vec<_>>();
        let (issues, truncated) =
            final_manifest_syntax_issues(&invalid_utf8, &invalid_utf8_spans, false);
        assert!(!truncated);
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].code.as_str(), "INVALID_UTF8");
        assert_eq!(issues[0].span, 0..1);
        assert_eq!(issues[1].code.as_str(), "INVALID_UTF8");
        assert_eq!(issues[1].span, 2..3);
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            invalid_source,
            MvsManifestLocation::final_snapshot_path(&package, 9),
            Some("acme:target@1.0.0".to_owned()),
            None,
        )])
        .expect("the malformed final manifest has a canonical synthetic path");
        let final_manifest = MvsManifestLocation::from_final_snapshot(&snapshot, file_id, &package)
            .expect("the bounded malformed manifest is retained");
        let location = super::manifest_span_location(
            &final_manifest,
            &MvsRequirer::Package(package),
            &issues[0].span,
        )
        .expect("the invalid byte span belongs to the retained source");
        assert_eq!(location.span.range.start().to_usize(), 0);
        assert_eq!(location.span.range.end().to_usize(), 1);
        assert_eq!(
            &snapshot
                .file(file_id)
                .expect("the final manifest remains in the snapshot")
                .source()
                .raw_bytes()
                [location.span.range.start().to_usize()..location.span.range.end().to_usize()],
            &[0xff]
        );

        let missing_newline_bytes =
            b"package acme:target @ 1.0.0\nsource \"src\" targets { wasm32-core }\n";
        let missing_newline_source =
            SourceFile::decode(sai_diagnostics::FileId::new(73), missing_newline_bytes)
                .expect("the parser fixture fits in a source file");
        let missing_newline = parse_for_role(missing_newline_bytes, PackageRole::Dependency);
        let (issues, truncated) = final_manifest_syntax_issues(
            &missing_newline,
            &[],
            missing_newline_source.invalid_utf8_sequences().len()
                > MAX_FINAL_MANIFEST_SYNTAX_ISSUES + 1,
        );
        assert!(!truncated);
        assert!(issues.iter().any(|issue| {
            issue.code.as_str() == "EXPECTED_TOKEN"
                && issue.message == "expected a newline between declarations"
                && issue.expected_tokens.as_deref() == Some(&["NEWLINE".to_owned()])
        }));
        assert!(
            issues
                .iter()
                .all(|issue| issue.code.as_str() != "UNEXPECTED_NEWLINE")
        );

        let alternatives_bytes =
            b"package acme:target @ 1.0.0\nrequires { acme:dep @ ^1 from invalid }\n";
        let alternatives = parse_for_role(alternatives_bytes, PackageRole::Dependency);
        let (issues, truncated) = final_manifest_syntax_issues(&alternatives, &[], false);
        assert!(!truncated);
        assert!(issues.iter().any(|issue| {
            issue.code.as_str() == "EXPECTED_TOKEN"
                && issue.expected_tokens.as_deref() == Some(&["git".to_owned(), "path".to_owned()])
        }));

        let comparator_bytes = b"package acme:target @ 1.0.0\nrequires { acme:dep @ ^1.0.0, }\n";
        let comparator = parse_for_role(comparator_bytes, PackageRole::Dependency);
        let (issues, truncated) = final_manifest_syntax_issues(&comparator, &[], false);
        assert!(!truncated);
        assert!(issues.iter().any(|issue| {
            issue.code.as_str() == "EXPECTED_TOKEN"
                && issue.expected_tokens.as_deref()
                    == Some(
                        &[
                            "^".to_owned(),
                            "~".to_owned(),
                            "=".to_owned(),
                            ">=".to_owned(),
                            ">".to_owned(),
                            "<=".to_owned(),
                            "<".to_owned(),
                        ][..],
                    )
        }));

        let version_bytes = b"package acme:target @ 1.0.0\nrequires { acme:dep @ ^ }\n";
        let version = parse_for_role(version_bytes, PackageRole::Dependency);
        let (issues, truncated) = final_manifest_syntax_issues(&version, &[], false);
        assert!(!truncated);
        assert!(issues.iter().any(|issue| {
            issue.code.as_str() == "EXPECTED_TOKEN"
                && issue.expected_tokens.as_deref()
                    == Some(&["INTEGER".to_owned(), "VERSION".to_owned()])
        }));

        let list_separator_bytes =
            b"package acme:target @ 1.0.0\ntargets { wasm32-core arm64-core }\n";
        let list_separator = parse_for_role(list_separator_bytes, PackageRole::Dependency);
        let (issues, truncated) = final_manifest_syntax_issues(&list_separator, &[], false);
        assert!(!truncated);
        assert!(issues.iter().any(|issue| {
            issue.code.as_str() == "EXPECTED_TOKEN"
                && issue.expected_tokens.as_deref() == Some(&[",".to_owned(), "NEWLINE".to_owned()])
        }));
    }

    #[test]
    fn expected_token_sets_follow_package_parser_expectations() {
        let cases: &[(&str, &[&str])] = &[
            ("`component`", &["component"]),
            ("`git` or `path`", &["git", "path"]),
            ("`allow`, `warn`, or `deny`", &["allow", "warn", "deny"]),
            ("`artifact`, `path`, or `git`", &["artifact", "path", "git"]),
            ("at least one list item", &["LIST_ITEM"]),
            ("version comparator", &["^", "~", "=", ">=", ">", "<=", "<"]),
            ("version after comparator", &["INTEGER", "VERSION"]),
            ("package identity", &["PACKAGE_PATH"]),
            ("full package version", &["VERSION"]),
            ("size integer", &["INTEGER"]),
            ("size unit", &["UPPER_WORD"]),
            ("path string", &["STRING"]),
            ("repository directory", &["STRING"]),
            ("capability name", &["WORD", "PACKAGE_PATH"]),
            ("package path", &["PACKAGE_PATH"]),
            ("word", &["WORD"]),
            ("string", &["STRING"]),
            ("lint code", &["UPPER_WORD"]),
        ];

        for (description, expected) in cases {
            let expected = expected
                .iter()
                .map(|token| (*token).to_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                super::expected_tokens_from_description(description),
                Some(expected),
                "unexpected token set for parser expectation {description:?}"
            );
        }
    }

    #[test]
    fn final_manifest_semantic_diagnostics_use_registered_package_codes() {
        let cases = [
            (
                PackageValidationErrorKind::MissingTargets,
                "MISSING_TARGETS",
            ),
            (
                PackageValidationErrorKind::PackageHasNoContent,
                "PACKAGE_HAS_NO_CONTENT",
            ),
            (
                PackageValidationErrorKind::UnknownTarget("target".into()),
                "UNKNOWN_TARGET",
            ),
            (
                PackageValidationErrorKind::OnlyTargetNotDeclared("target".into()),
                "ONLY_TARGET_NOT_DECLARED",
            ),
            (
                PackageValidationErrorKind::ArtifactTargetNotDeclared("target".into()),
                "ARTIFACT_TARGET_NOT_DECLARED",
            ),
            (
                PackageValidationErrorKind::ArtifactOnCoreTarget("target".into()),
                "ARTIFACT_ON_CORE_TARGET",
            ),
            (
                PackageValidationErrorKind::MissingArtifactForTarget("target".into()),
                "MISSING_ARTIFACT_FOR_TARGET",
            ),
            (PackageValidationErrorKind::WitRequired, "WIT_REQUIRED"),
            (
                PackageValidationErrorKind::MissingExportWorld,
                "MISSING_EXPORT_WORLD",
            ),
            (
                PackageValidationErrorKind::InvalidWorldName("world".into()),
                "INVALID_WORLD_NAME",
            ),
            (
                PackageValidationErrorKind::MissingSource("acme:dep".into()),
                "MISSING_SOURCE",
            ),
            (
                PackageValidationErrorKind::InvalidPackageName("invalid".into()),
                "INVALID_PACKAGE_NAME",
            ),
            (
                PackageValidationErrorKind::InvalidDescription,
                "INVALID_DESCRIPTION",
            ),
            (
                PackageValidationErrorKind::ImplicitPackageRequired("sai:core".into()),
                "IMPLICIT_PACKAGE_REQUIRED",
            ),
            (
                PackageValidationErrorKind::PackageRequiresItself("acme:self".into()),
                "PACKAGE_REQUIRES_ITSELF",
            ),
            (
                PackageValidationErrorKind::DuplicateRequirement("acme:dep".into()),
                "DUPLICATE_REQUIREMENT",
            ),
            (
                PackageValidationErrorKind::InvalidCapabilityName("invalid".into()),
                "INVALID_CAPABILITY_NAME",
            ),
            (
                PackageValidationErrorKind::CapabilityAllowDenyConflict("alloc".into()),
                "CAPABILITY_ALLOW_DENY_CONFLICT",
            ),
            (
                PackageValidationErrorKind::UnknownPolicyItem("policy".into()),
                "UNKNOWN_POLICY_ITEM",
            ),
            (
                PackageValidationErrorKind::PolicyItemIsCapability("alloc".into()),
                "POLICY_ITEM_IS_CAPABILITY",
            ),
            (
                PackageValidationErrorKind::UnknownLint("UNKNOWN".into()),
                "UNKNOWN_LINT",
            ),
            (
                PackageValidationErrorKind::NotALint("EXPECTED_TOKEN".into()),
                "NOT_A_LINT",
            ),
            (
                PackageValidationErrorKind::UnknownProfile("fast".into()),
                "UNKNOWN_PROFILE",
            ),
            (
                PackageValidationErrorKind::ProfileAllowExceedsPackage("host".into()),
                "PROFILE_ALLOW_EXCEEDS_PACKAGE",
            ),
            (
                PackageValidationErrorKind::UnknownBudgetMetric("memory".into()),
                "UNKNOWN_BUDGET_METRIC",
            ),
            (PackageValidationErrorKind::InvalidSize, "INVALID_SIZE"),
            (
                PackageValidationErrorKind::AllocatorNotRequired("acme:alloc".into()),
                "ALLOCATOR_NOT_REQUIRED",
            ),
            (
                PackageValidationErrorKind::DuplicateOverride {
                    package: "acme:dep".into(),
                    kind: OverrideKind::Source,
                },
                "DUPLICATE_OVERRIDE",
            ),
            (
                PackageValidationErrorKind::DuplicateOverride {
                    package: "acme:dep".into(),
                    kind: OverrideKind::Artifact,
                },
                "DUPLICATE_OVERRIDE",
            ),
            (
                PackageValidationErrorKind::CompilerVersionMismatch {
                    requirement: "^1".into(),
                    found: "0.1.0".into(),
                },
                "COMPILER_VERSION_MISMATCH",
            ),
        ];
        assert_eq!(cases.len(), 30);
        for (error, expected_code) in cases {
            let (code, message) = final_manifest_semantic_diagnostic(&error);
            assert_eq!(
                code.as_str(),
                expected_code,
                "incorrect mapping for {error:?}"
            );
            assert!(!message.is_empty(), "empty diagnostic for {error:?}");
        }
    }

    #[test]
    fn internal_final_contract_audit_failures_use_the_internal_error_record() {
        let failure = ResolverFinalContractAuditFailure {
            package_version: package_version("acme:dep", "1.0.0"),
            read_source: None,
            final_source: None,
            error: ResolverFinalContractAuditFailureKind::MissingReloadedContract,
        };
        let diagnostic = final_contract_audit_internal_error(&[failure], 1)
            .expect("inconsistent audit output produces an internal compiler error");
        assert_eq!(diagnostic.code, Code::InternalCompilerError);
        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(diagnostic.primary().is_none());
        let description = "final-source contract audit found 2 internal inconsistencies (missing_path_source_identity, missing_reloaded_contract)";
        assert_eq!(
            diagnostic.message,
            format!("internal compiler error: {description}")
        );
        assert_eq!(
            diagnostic.data.get("component"),
            Some(&Value::String("sai_package".to_owned()))
        );
        let Some(Value::String(panic_message)) = diagnostic.data.get("panic_message") else {
            panic!("internal error record has a panic_message string");
        };
        let location_prefix = format!("{description} at {}:", file!());
        let location = panic_message
            .strip_prefix(&location_prefix)
            .expect("panic_message includes the description and Rust source location");
        let mut location_parts = location.split(':');
        let line = location_parts
            .next()
            .expect("panic_message includes a source line");
        let column = location_parts
            .next()
            .expect("panic_message includes a source column");
        assert!(location_parts.next().is_none());
        assert!(line.parse::<u32>().is_ok_and(|line| line > 0));
        assert!(column.parse::<u32>().is_ok_and(|column| column > 0));
        assert_eq!(
            diagnostic.data.get("query_stack"),
            Some(&Value::Array(Vec::new()))
        );
        let Some(Value::String(compiler_version)) = diagnostic.data.get("compiler_version") else {
            panic!("internal error record has a compiler_version string");
        };
        let Some((package_version, build_id)) = compiler_version.split_once('+') else {
            panic!("compiler version includes SemVer build metadata");
        };
        assert_eq!(package_version, env!("CARGO_PKG_VERSION"));
        assert_eq!(build_id.len(), 16);
        assert!(
            build_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        );
        assert_eq!(diagnostic.data.len(), 4);
        assert!(
            final_contract_audit_internal_error(&[], 0).is_none(),
            "complete audit results do not emit internal errors"
        );
    }

    #[test]
    fn missing_reloaded_contract_is_suppressed_only_by_a_reportable_failure() {
        let package_version = package_version("acme:dep", "1.0.0");
        let failure = ResolverFinalContractAuditFailure {
            package_version: package_version.clone(),
            read_source: None,
            final_source: None,
            error: ResolverFinalContractAuditFailureKind::MissingReloadedContract,
        };
        assert!(
            final_contract_audit_failure_is_unexplained(
                &failure,
                &BTreeSet::new(),
                &BTreeSet::new()
            ),
            "an unreportable reload failure must not hide the audit inconsistency"
        );
        assert!(
            !final_contract_audit_failure_is_unexplained(
                &failure,
                &BTreeSet::from([package_version.clone()]),
                &BTreeSet::new()
            ),
            "a source-located reload diagnostic explains the missing contract"
        );
        assert!(
            !final_contract_audit_failure_is_unexplained(
                &failure,
                &BTreeSet::new(),
                &BTreeSet::from([package_version])
            ),
            "a reportable source-identity diagnostic explains the missing contract"
        );
    }

    #[test]
    fn final_source_diagnostics_follow_location_package_and_span_order() {
        let root = package_version("acme:root", "1.0.0");
        let dependency = package_version("acme:aaa-dep", "1.0.0");
        let workspace_member = package_version("acme:zzz-member", "1.0.0");
        let manifests = manifests_for(
            (
                root.clone(),
                "package acme:root @ 1.0.0\nsource \"src\"\ntargets { wasm32-core }\n".to_owned(),
            ),
            vec![
                (
                    dependency.clone(),
                    "package acme:aaa-dep @ 1.0.0\nsource \"src\"\ntargets { wasm32-core }\n"
                        .to_owned(),
                ),
                (
                    workspace_member.clone(),
                    "package acme:zzz-member @ 1.0.0\nsource \"src\"\ntargets { wasm32-core }\n"
                        .to_owned(),
                ),
            ],
        );
        let workspace_member_packages = BTreeSet::from([manifests
            .packages
            .get(&workspace_member)
            .and_then(|manifest| manifest.source.package.clone())
            .expect("the workspace member manifest has a package identity")]);
        let diagnostic = |package: &str, path: &str, start: u32, end: u32, value: &str| {
            let mut diagnostic = Diagnostic::new(
                Code::PathPackageNotFound,
                Severity::Error,
                "same",
                Some(Location {
                    file: SourceRef {
                        path: path.to_owned(),
                        package: Some(package.to_owned()),
                        snapshot: None,
                    },
                    span: Span::new(
                        sai_diagnostics::FileId::new(99),
                        TextRange::new(TextSize::new(start), TextSize::new(end))
                            .expect("test span is ordered"),
                    ),
                }),
            );
            diagnostic
                .data
                .insert("value".to_owned(), Value::String(value.to_owned()));
            diagnostic
        };
        let mut diagnostics = vec![
            diagnostic(
                "acme:aaa-dep@1.0.0",
                "dep:acme:aaa-dep@1.0.0/sai.package",
                0,
                1,
                "dependency",
            ),
            {
                let mut diagnostic = Diagnostic::new(
                    Code::PathPackageNotFound,
                    Severity::Error,
                    "locationless ordinary diagnostic",
                    None,
                );
                diagnostic
                    .data
                    .insert("value".to_owned(), Value::String("unlocated".to_owned()));
                diagnostic
            },
            diagnostic("acme:root@1.0.0", "sai.package", 0, 5, "z"),
            diagnostic("acme:root@1.0.0", "sai.package", 0, 5, "a"),
            diagnostic("acme:root@1.0.0", "sai.package", 0, 3, "shorter"),
            diagnostic(
                "acme:zzz-member@1.0.0",
                "dep:acme:zzz-member@1.0.0/sai.package",
                0,
                1,
                "workspace member",
            ),
        ];

        sort_final_source_diagnostics(
            &mut diagnostics,
            &manifests,
            &workspace_member_packages,
            Some(Diagnostic::new(
                Code::InternalCompilerError,
                Severity::Error,
                "internal compiler error",
                None,
            )),
        );

        assert_eq!(
            diagnostics[0].data.get("value"),
            Some(&Value::String("unlocated".to_owned())),
            "locationless ordinary records precede source-located diagnostics"
        );
        assert_eq!(
            diagnostics[1].data.get("value"),
            Some(&Value::String("shorter".to_owned())),
            "root package precedes dependencies and the shorter span sorts first"
        );
        assert_eq!(
            diagnostics[2].data.get("value"),
            Some(&Value::String("a".to_owned())),
            "the final stable-record tiebreaker distinguishes otherwise equal locations"
        );
        assert_eq!(
            diagnostics[3].data.get("value"),
            Some(&Value::String("z".to_owned()))
        );
        assert_eq!(
            diagnostics[4].data.get("value"),
            Some(&Value::String("workspace member".to_owned())),
            "workspace members precede dependencies even when identity bytes sort later"
        );
        assert_eq!(
            diagnostics[5].data.get("value"),
            Some(&Value::String("dependency".to_owned()))
        );
        assert_eq!(
            diagnostics[6].code,
            Code::InternalCompilerError,
            "the ICE is appended after ordered ordinary diagnostics"
        );
    }

    #[test]
    fn final_manifest_syntax_issue_collection_is_bounded() {
        let bytes = vec![b';'; MAX_FINAL_MANIFEST_SYNTAX_ISSUES * 4];
        let parsed = parse_for_role(&bytes, PackageRole::Dependency);
        let (issues, truncated) = final_manifest_syntax_issues(&parsed, &[], false);
        assert_eq!(issues.len(), MAX_FINAL_MANIFEST_SYNTAX_ISSUES);
        assert!(truncated);
    }

    fn manifests_for(
        root: (MvsPackageVersion, String),
        packages: Vec<(MvsPackageVersion, String)>,
    ) -> MvsManifestLocations {
        let mut files = vec![snapshot_file(1, "sai.package".to_owned(), &root.0, &root.1)];
        files.extend(
            packages
                .iter()
                .enumerate()
                .map(|(index, (package, source))| {
                    snapshot_file(
                        u32::try_from(index + 2).expect("test fixture file ID"),
                        format!(
                            "dep:{}@{}/sai.package",
                            package.package(),
                            package.version()
                        ),
                        package,
                        source,
                    )
                }),
        );
        let snapshot = WorkspaceSnapshot::new(files).expect("valid test source snapshot");
        let mut locations = MvsManifestLocations::default();
        locations
            .set_root(
                MvsManifestLocation::from_snapshot(
                    &snapshot,
                    sai_diagnostics::FileId::new(1),
                    &root.0,
                    crate::manifest::PackageRole::Root,
                )
                .expect("valid root manifest location"),
            )
            .expect("root belongs to the snapshot");
        for (index, (package, _)) in packages.iter().enumerate() {
            locations
                .insert_package(
                    MvsManifestLocation::from_snapshot(
                        &snapshot,
                        sai_diagnostics::FileId::new(
                            u32::try_from(index + 2).expect("test fixture file ID"),
                        ),
                        package,
                        crate::manifest::PackageRole::Dependency,
                    )
                    .expect("valid dependency manifest location"),
                )
                .expect("dependency belongs to the snapshot");
        }
        locations
    }

    fn divergent_comparison_fixture() -> (
        ResolverReloadedContractComparison<String, ()>,
        MvsManifestLocations,
    ) {
        let root_source = r#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:dep @ ^1 from git "https://example.test/read"
    acme:alpha @ ^1 from git "https://example.test/alpha"
}
"#;
        let alpha_source = r#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/final" }
"#;
        let read_contract_source = r#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:old @ ^1 from git "https://example.test/old"
    acme:sibling @ ^1 from path "../sibling"
}
"#;
        let final_contract_source = r#"package acme:dep @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:new @ ^2 from git "https://example.test/new"
    acme:sibling @ ^1 from path "../sibling"
}
"#;
        let package = package_version("acme:dep", "1.0.0");
        let root = parse_for_role(root_source.as_bytes(), PackageRole::Root);
        let path_packages = std::collections::BTreeMap::from([(
            ResolverPathKey::package(package.clone(), "../sibling"),
            package_version("acme:sibling", "1.0.0"),
        )]);
        let root_path_overrides = std::collections::BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("root contract context is valid");
        let root_contract =
            build_resolver_contract(&root, &context, PackageValidationContext::default())
                .expect("root contract is valid");
        let alpha = parse_for_role(alpha_source.as_bytes(), PackageRole::Dependency);
        let alpha_contract =
            build_resolver_contract(&alpha, &context, PackageValidationContext::default())
                .expect("alpha contract is valid");
        let alpha_version = alpha_contract.mvs.package_version().clone();
        let read_contract = build_resolver_contract(
            &parse_for_role(read_contract_source.as_bytes(), PackageRole::Dependency),
            &context,
            PackageValidationContext::default(),
        )
        .expect("read-time dependency contract is valid");
        let final_contract = build_resolver_contract(
            &parse_for_role(final_contract_source.as_bytes(), PackageRole::Dependency),
            &context,
            PackageValidationContext::default(),
        )
        .expect("final dependency contract is valid");

        let named_edge = |contract: &crate::resolver_contract::ResolverContract,
                          package: &str,
                          requirer: MvsRequirer| {
            let requirement = contract
                .requirements()
                .iter()
                .find(|requirement| requirement.package.as_ref() == package)
                .expect("source requirement metadata exists");
            let (path_pin, root_override_pin) = match requirement.resolution {
                crate::resolver_contract::ResolverRequirementResolution::Included {
                    pin, ..
                } => (
                    pin == crate::resolver_contract::ResolverPinKind::Path,
                    pin == crate::resolver_contract::ResolverPinKind::RootPathOverride,
                ),
                crate::resolver_contract::ResolverRequirementResolution::ExcludedByRootTargets => {
                    panic!("fixture requirements are included")
                }
            };
            MvsNamedEdge {
                requirer,
                span: requirement.span.clone(),
                path_pin,
                root_override_pin,
            }
        };
        let root_edge = named_edge(
            &root_contract,
            "acme:dep",
            MvsRequirer::Root(root_contract.mvs.package_version().clone()),
        );
        let alpha_edge = named_edge(
            &alpha_contract,
            "acme:dep",
            MvsRequirer::Package(alpha_version.clone()),
        );
        let dependency_contracts = HashMap::from([(alpha_version.clone(), alpha_contract.clone())]);
        let read_source = select_resolver_source(
            &MvsContractRequest {
                package_version: package.clone(),
                named_by: vec![root_edge],
                lock_floor: false,
            },
            &root_contract,
            &context,
            &dependency_contracts,
            &HashMap::new(),
        )
        .expect("read-time source is selected");
        let final_source = select_resolver_source(
            &MvsContractRequest {
                package_version: package.clone(),
                named_by: vec![alpha_edge],
                lock_floor: false,
            },
            &root_contract,
            &context,
            &dependency_contracts,
            &HashMap::new(),
        )
        .expect("final source is selected");
        let edges =
            compare_resolver_contract_edges(&read_contract, &context, &final_contract, &context)
                .expect("contracts can be compared");
        let paths = edges.compare_with_resolved_paths(|request| {
            Ok::<_, ResolverFinalContractPathResolutionError<()>>(match request.side {
                crate::resolver_contract::ResolverContractSourceSide::Expected => {
                    "read-time sibling source"
                }
                crate::resolver_contract::ResolverContractSourceSide::Actual => {
                    "final sibling source"
                }
            })
        });
        let comparison = ResolverReloadedContractComparison {
            reload: ResolverSourceReload {
                package_version: package,
                read_source,
                final_source,
                read_identity: "/private/read-source-id".to_owned(),
                final_identity: "/private/final-source-id".to_owned(),
            },
            edges,
            paths,
        };
        let manifests = manifests_for(
            (
                root_contract.mvs.package_version().clone(),
                root_source.to_owned(),
            ),
            vec![(alpha_version, alpha_source.to_owned())],
        );
        (comparison, manifests)
    }

    #[test]
    fn divergent_source_diagnostic_shows_both_sources_edges_and_override_help() {
        let (comparison, manifests) = divergent_comparison_fixture();

        let diagnostic =
            divergent_sources_diagnostic(&comparison, &manifests).expect("valid diagnostic");

        assert_eq!(diagnostic.code.as_str(), "DIVERGENT_SOURCES");
        assert_eq!(
            diagnostic.message,
            "acme:dep@1.0.0 has different included contracts in its read-time and final sources"
        );
        assert_eq!(
            diagnostic.children()[0]
                .location
                .as_ref()
                .unwrap()
                .file
                .path,
            "sai.package"
        );
        assert_eq!(
            diagnostic.children()[1]
                .location
                .as_ref()
                .unwrap()
                .file
                .path,
            "dep:acme:alpha@1.0.0/sai.package"
        );
        let root_source = r#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:dep @ ^1 from git "https://example.test/read"
    acme:alpha @ ^1 from git "https://example.test/alpha"
}
"#;
        let alpha_source = r#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/final" }
"#;
        assert_eq!(
            diagnostic.children()[0]
                .location
                .as_ref()
                .unwrap()
                .span
                .range
                .start()
                .to_usize(),
            root_source
                .find("acme:dep @ ^1 from git \"https://example.test/read\"")
                .unwrap()
        );
        assert_eq!(
            diagnostic.children()[1]
                .location
                .as_ref()
                .unwrap()
                .span
                .range
                .start()
                .to_usize(),
            alpha_source
                .find("acme:dep @ ^1 from git \"https://example.test/final\"")
                .unwrap()
        );
        assert!(
            diagnostic.children()[0]
                .message
                .contains("https://example.test/read")
        );
        assert!(
            diagnostic.children()[1]
                .message
                .contains("https://example.test/final")
        );
        assert!(
            diagnostic.children()[1]
                .message
                .contains("a different resolved source tree")
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .all(|child| !child.message.contains("/private/"))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.message.contains("acme:old @ ^1"))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.message.contains("acme:new @ ^2"))
        );
        assert!(diagnostic.children().iter().any(|child| {
            child
                .message
                .contains("path dependency acme:sibling resolves differently")
                && child.message.contains("../sibling")
        }));
        assert!(
            diagnostic
                .children()
                .last()
                .unwrap()
                .message
                .contains("root override")
        );
        assert!(matches!(
            diagnostic.data.get("read_contract_edges"),
            Some(Value::Array(edges)) if edges.len() == 2
        ));
        assert!(matches!(
            diagnostic.data.get("final_contract_edges"),
            Some(Value::Array(edges)) if edges.len() == 2
        ));
        assert!(matches!(
            diagnostic.data.get("path_source_differences"),
            Some(Value::Array(differences)) if differences.len() == 1
        ));
        assert_eq!(
            diagnostic.data.get("source_identities_differ"),
            Some(&Value::Bool(true))
        );
    }

    #[test]
    fn divergent_source_diagnostic_rejects_a_mismatched_contract_version() {
        let (mut comparison, manifests) = divergent_comparison_fixture();
        comparison.reload.package_version = package_version("acme:other", "1.0.0");

        assert!(matches!(
            divergent_sources_diagnostic(&comparison, &manifests),
            Err(
                MvsDuplicatePackageLineDiagnosticError::DivergentContractPackageVersionMismatch { .. }
            )
        ));
    }

    #[test]
    fn divergent_source_diagnostic_rejects_equal_source_identities() {
        let (mut comparison, manifests) = divergent_comparison_fixture();
        comparison.reload.final_identity = comparison.reload.read_identity.clone();

        assert!(matches!(
            divergent_sources_diagnostic(&comparison, &manifests),
            Err(MvsDuplicatePackageLineDiagnosticError::DivergentSourcesHaveSameIdentity(_))
        ));
    }

    #[test]
    fn divergent_source_diagnostic_rejects_a_changed_source_clause_at_the_same_span() {
        let (comparison, _) = divergent_comparison_fixture();
        let changed_root = r#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
requires {
    acme:dep @ ^1 from git "https://example.test/evil"
    acme:alpha @ ^1 from git "https://example.test/alpha"
}
"#;
        let alpha_source = r#"package acme:alpha @ 1.0.0
source "src"
targets { wasm32-core }
requires { acme:dep @ ^1 from git "https://example.test/final" }
"#;
        let manifests = manifests_for(
            (
                package_version("acme:app", "1.0.0"),
                changed_root.to_owned(),
            ),
            vec![(
                package_version("acme:alpha", "1.0.0"),
                alpha_source.to_owned(),
            )],
        );

        assert!(matches!(
            divergent_sources_diagnostic(&comparison, &manifests),
            Err(MvsDuplicatePackageLineDiagnosticError::SourceSelectionDoesNotMatchManifest { .. })
        ));
    }

    #[test]
    fn root_git_override_location_rejects_a_different_root_at_the_same_span() {
        let selected_root_source = r#"package acme:app @ 1.0.0
source "src"
targets { wasm32-core }
overrides { acme:dep use git "https://example.test/fork" }
requires { acme:dep @ ^1 from git "https://example.test/read" }
"#;
        let selected_root = parse_for_role(selected_root_source.as_bytes(), PackageRole::Root);
        let path_packages = std::collections::BTreeMap::new();
        let root_path_overrides = std::collections::BTreeMap::new();
        let context = ResolverContractContext::from_root_manifest(
            &selected_root,
            &path_packages,
            &root_path_overrides,
            PackageValidationContext::default(),
        )
        .expect("selected root context is valid");
        let root_contract = build_resolver_contract(
            &selected_root,
            &context,
            PackageValidationContext::default(),
        )
        .expect("selected root contract is valid");
        let package = package_version("acme:dep", "1.0.0");
        let root_package = root_contract.mvs.package_version().clone();
        let requirement = root_contract
            .requirements()
            .iter()
            .find(|requirement| requirement.package.as_ref() == package.package())
            .expect("root dependency edge is present");
        let selection = select_resolver_source(
            &MvsContractRequest {
                package_version: package,
                named_by: vec![MvsNamedEdge {
                    requirer: MvsRequirer::Root(root_package),
                    span: requirement.span.clone(),
                    path_pin: false,
                    root_override_pin: false,
                }],
                lock_floor: false,
            },
            &root_contract,
            &context,
            &HashMap::new(),
            &HashMap::new(),
        )
        .expect("root Git override selects a source");
        let wrong_root_source = selected_root_source.replacen("1.0.0", "2.0.0", 1);
        let wrong_root_manifests = manifests_for(
            (package_version("acme:app", "2.0.0"), wrong_root_source),
            vec![],
        );

        assert!(matches!(
            resolver_source_selection_location(&selection, &wrong_root_manifests),
            Err(MvsDuplicatePackageLineDiagnosticError::ManifestPackageMismatch { .. })
        ));
        let changed_override_source = selected_root_source.replace("fork", "evil");
        let changed_override_manifests = manifests_for(
            (
                package_version("acme:app", "1.0.0"),
                changed_override_source,
            ),
            vec![],
        );
        assert!(matches!(
            resolver_source_selection_location(&selection, &changed_override_manifests),
            Err(MvsDuplicatePackageLineDiagnosticError::SourceSelectionDoesNotMatchManifest { .. })
        ));
    }

    const ROOT_MANIFEST: &str = "package acme:app @ 1.0.0\n\
                                 source \"src\"\n\
                                 targets { wasm32-core }\n\
                                 requires { acme:a @ ^1 from path \"../a\" }\n";
    const A_MANIFEST: &str = "package acme:a @ 1.0.0\n\
                              source \"src\"\n\
                              targets { wasm32-core }\n\
                              requires { acme:app @ ^2 from path \"../app\" }\n";

    fn duplicate_fixture() -> (MvsSelectionOutput, MvsDuplicatePackageLines) {
        duplicate_fixture_with_root_edge(
            "acme:a",
            "^1",
            source_span(ROOT_MANIFEST, "acme:a @ ^1 from path \"../a\""),
        )
    }

    fn duplicate_fixture_with_root_edge(
        target: &str,
        requirement: &str,
        span: Range<usize>,
    ) -> (MvsSelectionOutput, MvsDuplicatePackageLines) {
        let root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![edge_with_span(target, requirement, "1.0.0", span)],
        )
        .expect("valid root");
        let a = MvsContract::new(
            package_version("acme:a", "1.0.0"),
            vec![edge_with_span(
                "acme:app",
                "^2",
                "2.0.0",
                source_span(A_MANIFEST, "acme:app @ ^2 from path \"../app\""),
            )],
        )
        .expect("valid dependency");
        let app_v2 = MvsContract::new(package_version("acme:app", "2.0.0"), vec![])
            .expect("valid second package line");
        let contracts = HashMap::from([
            (a.package_version().clone(), a),
            (app_v2.package_version().clone(), app_v2),
        ]);
        let selection = select(&root, &contracts, &HashMap::new(), &HashMap::new())
            .expect("valid MVS selection");
        let graph = check_selected_graph(&root, &selection).expect("valid selected graph");
        let duplicate = duplicate_package_lines(&graph)
            .into_iter()
            .find(|duplicate| duplicate.package.as_ref() == "acme:app")
            .expect("root and dependency lines form a duplicate");
        (selection, duplicate)
    }

    fn duplicate_with_lock_floor_fixture() -> (
        MvsSelectionOutput,
        MvsDuplicatePackageLines,
        MvsManifestLocations,
    ) {
        let (root_source, root_spans) =
            manifest_source("acme:app", "1.0.0", &[("acme:a", "^1"), ("acme:b", "^1")]);
        let (a_source, a_spans) = manifest_source("acme:a", "1.0.0", &[("acme:lib", "^1")]);
        let (b_source, b_spans) = manifest_source("acme:b", "1.0.0", &[("acme:lib", "^2")]);
        let root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![
                edge_with_span("acme:a", "^1", "1.0.0", root_spans[0].clone()),
                edge_with_span("acme:b", "^1", "1.0.0", root_spans[1].clone()),
            ],
        )
        .expect("valid root");
        let a = MvsContract::new(
            package_version("acme:a", "1.0.0"),
            vec![edge_with_span(
                "acme:lib",
                "^1",
                "1.0.0",
                a_spans[0].clone(),
            )],
        )
        .expect("valid line-one requirer");
        let b = MvsContract::new(
            package_version("acme:b", "1.0.0"),
            vec![edge_with_span(
                "acme:lib",
                "^2",
                "2.0.0",
                b_spans[0].clone(),
            )],
        )
        .expect("valid line-two requirer");
        let lib_v1 = MvsContract::new(package_version("acme:lib", "1.0.0"), vec![])
            .expect("valid first line");
        let lib_v2_named = MvsContract::new(package_version("acme:lib", "2.0.0"), vec![])
            .expect("valid named version");
        let lib_v2_floor = MvsContract::new(package_version("acme:lib", "2.5.0"), vec![])
            .expect("valid lock floor");
        let contracts = HashMap::from([
            (a.package_version().clone(), a),
            (b.package_version().clone(), b),
            (lib_v1.package_version().clone(), lib_v1),
            (lib_v2_named.package_version().clone(), lib_v2_named),
            (lib_v2_floor.package_version().clone(), lib_v2_floor),
        ]);
        let floors = HashMap::from([(
            package_version("acme:lib", "2.0.0").node(),
            version("2.5.0"),
        )]);
        let selection =
            select(&root, &contracts, &floors, &HashMap::new()).expect("valid MVS selection");
        let graph = check_selected_graph(&root, &selection).expect("valid selected graph");
        let duplicate = duplicate_package_lines(&graph)
            .into_iter()
            .find(|duplicate| duplicate.package.as_ref() == "acme:lib")
            .expect("root edges reach both package lines");
        let manifests = manifests_for(
            (package_version("acme:app", "1.0.0"), root_source),
            vec![
                (package_version("acme:a", "1.0.0"), a_source),
                (package_version("acme:b", "1.0.0"), b_source),
            ],
        );
        (selection, duplicate, manifests)
    }

    fn version_conflict_fixture() -> (MvsSelectionOutput, MvsManifestLocations) {
        let (root_source, root_spans) = manifest_source(
            "acme:app",
            "1.0.0",
            &[("acme:lib", "^1.9"), ("acme:a", "^1"), ("acme:b", "^1")],
        );
        let (a_source, a_spans) = manifest_source("acme:a", "1.0.0", &[("acme:lib", "^1.9")]);
        let (b_source, b_spans) =
            manifest_source("acme:b", "1.0.0", &[("acme:lib", ">=1.4, <1.8")]);
        let (lib_v1_source, _) = manifest_source("acme:lib", "1.4.0", &[]);
        let (lib_source, _) = manifest_source("acme:lib", "1.9.0", &[]);
        let root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![
                edge_with_span("acme:lib", "^1.9", "1.9.0", root_spans[0].clone()),
                edge_with_span("acme:a", "^1", "1.0.0", root_spans[1].clone()),
                edge_with_span("acme:b", "^1", "1.0.0", root_spans[2].clone()),
            ],
        )
        .expect("valid root contract");
        let a = MvsContract::new(
            package_version("acme:a", "1.0.0"),
            vec![edge_with_span(
                "acme:lib",
                "^1.9",
                "1.9.0",
                a_spans[0].clone(),
            )],
        )
        .expect("valid selected-version requirer");
        let b = MvsContract::new(
            package_version("acme:b", "1.0.0"),
            vec![edge_with_span(
                "acme:lib",
                ">=1.4, <1.8",
                "1.4.0",
                b_spans[0].clone(),
            )],
        )
        .expect("valid conflicting requirer");
        let lib = MvsContract::new(package_version("acme:lib", "1.9.0"), vec![])
            .expect("valid selected target");
        let lib_v1 = MvsContract::new(package_version("acme:lib", "1.4.0"), vec![])
            .expect("valid candidate target");
        let contracts = HashMap::from([
            (a.package_version().clone(), a),
            (b.package_version().clone(), b),
            (lib_v1.package_version().clone(), lib_v1),
            (lib.package_version().clone(), lib),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("MVS selection");
        let manifests = manifests_for(
            (package_version("acme:app", "1.0.0"), root_source),
            vec![
                (package_version("acme:a", "1.0.0"), a_source),
                (package_version("acme:b", "1.0.0"), b_source),
                (package_version("acme:lib", "1.4.0"), lib_v1_source),
                (package_version("acme:lib", "1.9.0"), lib_source),
            ],
        );
        (selection, manifests)
    }

    fn root_selected_version_conflict_fixture() -> (MvsSelectionOutput, MvsManifestLocations) {
        let (root_source, root_spans) = manifest_source(
            "acme:app",
            "1.0.0",
            &[("acme:lib", "^1.9"), ("acme:b", "^1")],
        );
        let (b_source, b_spans) =
            manifest_source("acme:b", "1.0.0", &[("acme:lib", ">=1.4, <1.8")]);
        let (lib_v1_source, _) = manifest_source("acme:lib", "1.4.0", &[]);
        let (lib_source, _) = manifest_source("acme:lib", "1.9.0", &[]);
        let root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![
                edge_with_span("acme:lib", "^1.9", "1.9.0", root_spans[0].clone()),
                edge_with_span("acme:b", "^1", "1.0.0", root_spans[1].clone()),
            ],
        )
        .expect("valid root contract");
        let b = MvsContract::new(
            package_version("acme:b", "1.0.0"),
            vec![edge_with_span(
                "acme:lib",
                ">=1.4, <1.8",
                "1.4.0",
                b_spans[0].clone(),
            )],
        )
        .expect("valid conflicting requirer");
        let lib = MvsContract::new(package_version("acme:lib", "1.9.0"), vec![])
            .expect("valid selected target");
        let lib_v1 = MvsContract::new(package_version("acme:lib", "1.4.0"), vec![])
            .expect("valid candidate target");
        let contracts = HashMap::from([
            (b.package_version().clone(), b),
            (lib_v1.package_version().clone(), lib_v1),
            (lib.package_version().clone(), lib),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("MVS selection");
        let manifests = manifests_for(
            (package_version("acme:app", "1.0.0"), root_source),
            vec![
                (package_version("acme:b", "1.0.0"), b_source),
                (package_version("acme:lib", "1.4.0"), lib_v1_source),
                (package_version("acme:lib", "1.9.0"), lib_source),
            ],
        );
        (selection, manifests)
    }

    fn path_pinned_lower_bound_conflict_fixture() -> (MvsSelectionOutput, MvsManifestLocations) {
        let root_requirement = "acme:lib @ ^1.5 from path \"../lib\"";
        let root_source = format!(
            "package acme:app @ 1.0.0\nrequires {{\n    {root_requirement}\n    acme:b @ ^1\n}}\n"
        );
        let root_span = source_span(&root_source, root_requirement);
        let (b_source, b_spans) = manifest_source("acme:b", "1.0.0", &[("acme:lib", "^1.8")]);
        let (lib_pin_source, _) = manifest_source("acme:lib", "1.5.0", &[]);
        let (lib_candidate_source, _) = manifest_source("acme:lib", "1.8.0", &[]);
        let root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![
                crate::mvs::MvsEdge::new_path_pinned(
                    "acme:lib",
                    VersionReq::parse("^1.5").expect("valid path requirement"),
                    version("1.5.0"),
                    root_span,
                )
                .expect("valid path pin"),
                edge_with_span(
                    "acme:b",
                    "^1",
                    "1.0.0",
                    source_span(&root_source, "acme:b @ ^1"),
                ),
            ],
        )
        .expect("valid root contract");
        let b = MvsContract::new(
            package_version("acme:b", "1.0.0"),
            vec![edge_with_span(
                "acme:lib",
                "^1.8",
                "1.8.0",
                b_spans[0].clone(),
            )],
        )
        .expect("valid conflicting requirer");
        let lib_pin = MvsContract::new(package_version("acme:lib", "1.5.0"), vec![])
            .expect("valid pinned package");
        let lib_candidate = MvsContract::new(package_version("acme:lib", "1.8.0"), vec![])
            .expect("valid unselected candidate");
        let contracts = HashMap::from([
            (b.package_version().clone(), b),
            (lib_pin.package_version().clone(), lib_pin),
            (lib_candidate.package_version().clone(), lib_candidate),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("MVS selection");
        let manifests = manifests_for(
            (package_version("acme:app", "1.0.0"), root_source),
            vec![
                (package_version("acme:b", "1.0.0"), b_source),
                (package_version("acme:lib", "1.5.0"), lib_pin_source),
                (package_version("acme:lib", "1.8.0"), lib_candidate_source),
            ],
        );
        (selection, manifests)
    }

    fn dependency_cycle_fixture() -> (MvsSelectionOutput, MvsManifestLocations) {
        let (root_source, root_spans) = manifest_source("acme:app", "1.0.0", &[("acme:a", "^1")]);
        let (a_source, a_spans) = manifest_source("acme:a", "1.0.0", &[("acme:b", "^1")]);
        let (b_source, b_spans) = manifest_source("acme:b", "1.0.0", &[("acme:a", "^1")]);
        let root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![edge_with_span(
                "acme:a",
                "^1",
                "1.0.0",
                root_spans[0].clone(),
            )],
        )
        .expect("valid root contract");
        let a = MvsContract::new(
            package_version("acme:a", "1.0.0"),
            vec![edge_with_span("acme:b", "^1", "1.0.0", a_spans[0].clone())],
        )
        .expect("valid cycle package");
        let b = MvsContract::new(
            package_version("acme:b", "1.0.0"),
            vec![edge_with_span("acme:a", "^1", "1.0.0", b_spans[0].clone())],
        )
        .expect("valid cycle package");
        let contracts = HashMap::from([
            (a.package_version().clone(), a),
            (b.package_version().clone(), b),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("MVS selection");
        let manifests = manifests_for(
            (package_version("acme:app", "1.0.0"), root_source),
            vec![
                (package_version("acme:a", "1.0.0"), a_source),
                (package_version("acme:b", "1.0.0"), b_source),
            ],
        );
        (selection, manifests)
    }

    fn dependency_cycle_with_floor_fixture() -> (MvsSelectionOutput, MvsManifestLocations) {
        let (root_source, root_spans) = manifest_source("acme:app", "1.0.0", &[("acme:a", "^1")]);
        let (floor_source, floor_spans) = manifest_source("acme:a", "1.5.0", &[("acme:b", "^1")]);
        let (b_source, b_spans) = manifest_source("acme:b", "1.0.0", &[("acme:a", "^1")]);
        let root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![edge_with_span(
                "acme:a",
                "^1",
                "1.0.0",
                root_spans[0].clone(),
            )],
        )
        .expect("valid root contract");
        let named_a = MvsContract::new(package_version("acme:a", "1.0.0"), vec![])
            .expect("valid named candidate");
        let floor_a = MvsContract::new(
            package_version("acme:a", "1.5.0"),
            vec![edge_with_span(
                "acme:b",
                "^1",
                "1.0.0",
                floor_spans[0].clone(),
            )],
        )
        .expect("valid lock-floor contract");
        let b = MvsContract::new(
            package_version("acme:b", "1.0.0"),
            vec![edge_with_span("acme:a", "^1", "1.0.0", b_spans[0].clone())],
        )
        .expect("valid cycle package");
        let contracts = HashMap::from([
            (named_a.package_version().clone(), named_a),
            (floor_a.package_version().clone(), floor_a),
            (b.package_version().clone(), b),
        ]);
        let floors = HashMap::from([(package_version("acme:a", "1.0.0").node(), version("1.5.0"))]);
        let selection = select(&root, &contracts, &floors, &HashMap::new()).expect("MVS selection");
        let manifests = manifests_for(
            (package_version("acme:app", "1.0.0"), root_source),
            vec![
                (package_version("acme:a", "1.5.0"), floor_source),
                (package_version("acme:b", "1.0.0"), b_source),
            ],
        );
        (selection, manifests)
    }

    fn root_dependency_cycle_fixture() -> (MvsSelectionOutput, MvsManifestLocations) {
        root_dependency_cycle_fixture_with("1.0.0", "1.0.0", false)
    }

    fn root_dependency_cycle_fixture_with(
        root_version: &str,
        named_root_version: &str,
        include_missing_contract: bool,
    ) -> (MvsSelectionOutput, MvsManifestLocations) {
        root_dependency_cycle_fixture_with_a_floor(
            root_version,
            named_root_version,
            include_missing_contract,
            None,
        )
    }

    fn root_dependency_cycle_fixture_with_a_floor(
        root_version: &str,
        named_root_version: &str,
        include_missing_contract: bool,
        selected_a_floor: Option<&str>,
    ) -> (MvsSelectionOutput, MvsManifestLocations) {
        let mut root_requirements = vec![("acme:a", "^1")];
        if include_missing_contract {
            root_requirements.push(("acme:missing", "^1"));
        }
        let (root_source, root_spans) =
            manifest_source("acme:app", root_version, &root_requirements);
        let (a_source, a_spans) = manifest_source("acme:a", "1.0.0", &[("acme:b", "^1")]);
        let (b_source, b_spans) = manifest_source("acme:b", "1.0.0", &[("acme:app", "^1")]);
        let mut root_edges = vec![edge_with_span(
            "acme:a",
            "^1",
            "1.0.0",
            root_spans[0].clone(),
        )];
        if include_missing_contract {
            root_edges.push(edge_with_span(
                "acme:missing",
                "^1",
                "1.0.0",
                root_spans[1].clone(),
            ));
        }
        let root = MvsContract::new(package_version("acme:app", root_version), root_edges)
            .expect("valid root contract");
        let a = MvsContract::new(
            package_version("acme:a", "1.0.0"),
            vec![edge_with_span("acme:b", "^1", "1.0.0", a_spans[0].clone())],
        )
        .expect("valid dependency");
        let b = MvsContract::new(
            package_version("acme:b", "1.0.0"),
            vec![edge_with_span(
                "acme:app",
                "^1",
                named_root_version,
                b_spans[0].clone(),
            )],
        )
        .expect("valid dependency");
        let mut contracts = vec![
            (a.package_version().clone(), a),
            (b.package_version().clone(), b),
        ];
        if let Some(floor_version) = selected_a_floor {
            let a_floor = MvsContract::new(package_version("acme:a", floor_version), vec![])
                .expect("valid lock-floor contract");
            contracts.push((a_floor.package_version().clone(), a_floor));
        }
        let contracts = HashMap::from_iter(contracts);
        let floors = selected_a_floor
            .map(|floor_version| {
                HashMap::from([(
                    package_version("acme:a", floor_version).node(),
                    version(floor_version),
                )])
            })
            .unwrap_or_default();
        let selection = select(&root, &contracts, &floors, &HashMap::new()).expect("MVS selection");
        let manifests = manifests_for(
            (package_version("acme:app", root_version), root_source),
            vec![
                (package_version("acme:a", "1.0.0"), a_source),
                (package_version("acme:b", "1.0.0"), b_source),
            ],
        );
        (selection, manifests)
    }

    fn duplicate_with_candidate_chain_fixture() -> (
        MvsSelectionOutput,
        MvsDuplicatePackageLines,
        MvsManifestLocations,
    ) {
        let (root_source, root_spans) = manifest_source(
            "acme:app",
            "1.0.0",
            &[("acme:a", "^1"), ("acme:c", "^1"), ("acme:lib", "^1")],
        );
        let (a_source, a_spans) = manifest_source("acme:a", "1.0.0", &[("acme:b", "^1")]);
        let (b_v1_source, b_v1_spans) = manifest_source("acme:b", "1.0.0", &[("acme:lib", "^2")]);
        let (c_source, c_spans) =
            manifest_source("acme:c", "1.0.0", &[("acme:b", ">=1.4"), ("acme:d", "^1")]);
        let (d_source, d_spans) = manifest_source("acme:d", "1.0.0", &[("acme:e", "^1")]);
        let (e_source, e_spans) = manifest_source("acme:e", "1.0.0", &[("acme:lib", "^2")]);
        let root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![
                edge_with_span("acme:a", "^1", "1.0.0", root_spans[0].clone()),
                edge_with_span("acme:c", "^1", "1.0.0", root_spans[1].clone()),
                edge_with_span("acme:lib", "^1", "1.0.0", root_spans[2].clone()),
            ],
        )
        .expect("valid root");
        let a = MvsContract::new(
            package_version("acme:a", "1.0.0"),
            vec![edge_with_span("acme:b", "^1", "1.0.0", a_spans[0].clone())],
        )
        .expect("valid a");
        let b_v1 = MvsContract::new(
            package_version("acme:b", "1.0.0"),
            vec![edge_with_span(
                "acme:lib",
                "^2",
                "2.0.0",
                b_v1_spans[0].clone(),
            )],
        )
        .expect("valid unselected b candidate");
        let b_v14 =
            MvsContract::new(package_version("acme:b", "1.4.0"), vec![]).expect("valid selected b");
        let c = MvsContract::new(
            package_version("acme:c", "1.0.0"),
            vec![
                edge_with_span("acme:b", ">=1.4", "1.4.0", c_spans[0].clone()),
                edge_with_span("acme:d", "^1", "1.0.0", c_spans[1].clone()),
            ],
        )
        .expect("valid c");
        let d = MvsContract::new(
            package_version("acme:d", "1.0.0"),
            vec![edge_with_span("acme:e", "^1", "1.0.0", d_spans[0].clone())],
        )
        .expect("valid d");
        let e = MvsContract::new(
            package_version("acme:e", "1.0.0"),
            vec![edge_with_span(
                "acme:lib",
                "^2",
                "2.0.0",
                e_spans[0].clone(),
            )],
        )
        .expect("valid e");
        let lib_v1 = MvsContract::new(package_version("acme:lib", "1.0.0"), vec![])
            .expect("valid first lib line");
        let lib_v2 = MvsContract::new(package_version("acme:lib", "2.0.0"), vec![])
            .expect("valid second lib line");
        let contracts = [a, b_v1, b_v14, c, d, e, lib_v1, lib_v2]
            .into_iter()
            .map(|contract| (contract.package_version().clone(), contract))
            .collect();
        let selection = select(&root, &contracts, &HashMap::new(), &HashMap::new())
            .expect("valid MVS selection");
        let graph = check_selected_graph(&root, &selection).expect("valid selected graph");
        let duplicate = duplicate_package_lines(&graph)
            .into_iter()
            .find(|duplicate| duplicate.package.as_ref() == "acme:lib")
            .expect("root graph reaches both lib lines");
        let manifests = manifests_for(
            (package_version("acme:app", "1.0.0"), root_source),
            vec![
                (package_version("acme:a", "1.0.0"), a_source),
                (package_version("acme:b", "1.0.0"), b_v1_source),
                (package_version("acme:c", "1.0.0"), c_source),
                (package_version("acme:d", "1.0.0"), d_source),
                (package_version("acme:e", "1.0.0"), e_source),
            ],
        );
        (selection, duplicate, manifests)
    }

    fn manifests() -> MvsManifestLocations {
        manifests_with(ROOT_MANIFEST, A_MANIFEST)
    }

    fn manifests_with(root_source: &str, a_source: &str) -> MvsManifestLocations {
        manifests_for(
            (package_version("acme:app", "1.0.0"), root_source.to_owned()),
            vec![(package_version("acme:a", "1.0.0"), a_source.to_owned())],
        )
    }

    fn diagnostic(
        selection: &MvsSelectionOutput,
        duplicate: &MvsDuplicatePackageLines,
        manifests: &MvsManifestLocations,
        cli: Option<sai_diagnostics::OverrideLintLevel>,
        deny_warnings: bool,
    ) -> Option<sai_diagnostics::Diagnostic> {
        duplicate_package_line_diagnostic(
            &MvsExplanationGraph::new(selection).expect("explanation graph"),
            duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests,
                cli,
                deny_warnings,
            },
        )
        .expect("duplicate diagnostic")
    }

    #[test]
    fn emits_source_mapped_duplicate_lines_without_a_primary_span() {
        let (selection, duplicate) = duplicate_fixture();
        let manifests = manifests();
        let diagnostic = diagnostic(&selection, &duplicate, &manifests, None, false)
            .expect("warning is enabled");

        assert_eq!(diagnostic.code.as_str(), "DUPLICATE_PACKAGE_LINE");
        assert_eq!(diagnostic.severity, Severity::Warning);
        assert_eq!(diagnostic.primary(), None);
        assert_eq!(
            diagnostic.level().expect("lint level").source,
            LevelSource::Default
        );

        let children = diagnostic.children();
        assert_eq!(children.len(), 4);
        assert_eq!(children[0].message, "acme:app 1.0.0 (root)");
        assert_eq!(children[1].message, "acme:app 2.0.0");
        assert_eq!(
            children[2].message,
            "acme:app 1.0.0 (root) requires acme:a @ ^1"
        );
        let root_span_start = ROOT_MANIFEST.find("acme:a @ ^1").expect("root requirement");
        let root_span_end = root_span_start + "acme:a @ ^1 from path \"../a\"".len();
        assert_eq!(
            children[2]
                .location
                .as_ref()
                .expect("root edge location")
                .file
                .path,
            "sai.package"
        );
        assert_eq!(
            children[2]
                .location
                .as_ref()
                .expect("root edge location")
                .span
                .range,
            TextRange::new(
                TextSize::new(u32::try_from(root_span_start).expect("test offset")),
                TextSize::new(u32::try_from(root_span_end).expect("test offset")),
            )
            .expect("valid range")
        );
        assert_eq!(children[3].message, "acme:a 1.0.0 requires acme:app @ ^2");
        assert_eq!(
            children[3]
                .location
                .as_ref()
                .expect("dependency edge location")
                .file
                .path,
            "dep:acme:a@1.0.0/sai.package"
        );
        let dependency_span_start = A_MANIFEST
            .find("acme:app @ ^2")
            .expect("dependency requirement");
        let dependency_span_end =
            dependency_span_start + "acme:app @ ^2 from path \"../app\"".len();
        assert_eq!(
            children[3]
                .location
                .as_ref()
                .expect("dependency edge location")
                .span
                .range,
            TextRange::new(
                TextSize::new(u32::try_from(dependency_span_start).expect("test offset")),
                TextSize::new(u32::try_from(dependency_span_end).expect("test offset")),
            )
            .expect("valid range")
        );

        let Value::Array(lines) = diagnostic.data.get("lines").expect("line data") else {
            panic!("lines must be a structured array");
        };
        assert_eq!(lines.len(), 2);
        let Value::Object(root_line) = &lines[0] else {
            panic!("line must be an object");
        };
        assert_eq!(
            root_line.get("origin"),
            Some(&Value::String("root".to_owned()))
        );
        assert_eq!(
            root_line.get("version"),
            Some(&Value::String("1.0.0".to_owned()))
        );
        assert_eq!(root_line.get("chain"), Some(&Value::Array(vec![])));
        let Value::Object(second_line) = &lines[1] else {
            panic!("line must be an object");
        };
        let Value::Array(chain) = second_line.get("chain").expect("chain data") else {
            panic!("chain must be an array");
        };
        assert_eq!(chain.len(), 2);
        let Value::Object(second_link) = &chain[1] else {
            panic!("link must be an object");
        };
        assert_eq!(
            second_link.get("requirer_selected"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            second_link.get("requirement"),
            Some(&Value::String("^2".to_owned()))
        );
        assert_eq!(
            second_link.get("location"),
            Some(&Value::Object(ValueObject::from([
                (
                    "end".to_owned(),
                    Value::Integer(dependency_span_end as u128)
                ),
                (
                    "package".to_owned(),
                    Value::String("acme:a@1.0.0".to_owned())
                ),
                (
                    "path".to_owned(),
                    Value::String("dep:acme:a@1.0.0/sai.package".to_owned())
                ),
                ("snapshot".to_owned(), Value::Null),
                (
                    "start".to_owned(),
                    Value::Integer(dependency_span_start as u128)
                ),
            ])))
        );
    }

    #[test]
    fn graph_entry_point_builds_diagnostics_for_all_duplicate_lines() {
        let (selection, _) = duplicate_fixture();
        let manifests = manifests();
        let diagnostics = duplicate_package_line_diagnostics(
            &selection,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &manifests,
                cli: None,
                deny_warnings: false,
            },
        )
        .expect("selected graph diagnostics");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "DUPLICATE_PACKAGE_LINE");
    }

    #[test]
    fn rejects_root_lint_configuration_from_another_selection() {
        let (selection, duplicate) = duplicate_fixture();
        let graph = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let root_version = package_version("acme:app", "2.0.0");
        let root_source = ROOT_MANIFEST.replace("@ 1.0.0", "@ 2.0.0");
        let manifests = manifests_for(
            (root_version.clone(), root_source),
            vec![(package_version("acme:a", "1.0.0"), A_MANIFEST.to_owned())],
        );
        let error = duplicate_package_line_diagnostic(
            &graph,
            &duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &manifests,
                cli: None,
                deny_warnings: false,
            },
        )
        .expect_err("lint configuration must come from the selection root");

        assert_eq!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::ManifestVersionMismatch {
                requirer: Box::new(MvsRequirer::Root(selection.root().clone())),
                expected: Box::new(selection.root().clone()),
                found: Box::new(root_version),
            }
        );
    }

    #[test]
    fn root_policy_sets_error_level_and_keeps_its_manifest_location() {
        let (selection, duplicate) = duplicate_fixture();
        let source = format!(
            "{ROOT_MANIFEST}lints {{ allow DUPLICATE_PACKAGE_LINE }}\n\
             policy {{ deny duplicate_lines }}\n"
        );
        let manifests = manifests_with(&source, A_MANIFEST);
        let parsed = parse(source.as_bytes());
        assert!(validate(&parsed, PackageValidationContext::default()).is_valid());
        let root_manifest = manifests.root().expect("root manifest");
        let source_ref = root_manifest.source().clone();
        let policy_location = root_manifest
            .lint_policies()
            .deny_duplicate_package_lines
            .as_ref()
            .expect("duplicate-line policy");
        let diagnostic = diagnostic(&selection, &duplicate, &manifests, None, false)
            .expect("policy-denied warning is emitted");

        assert_eq!(diagnostic.severity, Severity::Error);
        let level = diagnostic.level().expect("lint level");
        assert_eq!(level.value, LintLevel::Deny);
        assert_eq!(level.source, LevelSource::Policy);
        assert_eq!(level.location.as_ref(), Some(policy_location));
        assert_eq!(policy_location.file, source_ref);
    }

    #[test]
    fn root_manifest_lint_level_controls_duplicate_line_diagnostic() {
        let (selection, duplicate) = duplicate_fixture();
        let source = format!("{ROOT_MANIFEST}lints {{ deny DUPLICATE_PACKAGE_LINE }}\n");
        let manifests = manifests_with(&source, A_MANIFEST);
        let root_manifest = manifests.root().expect("root manifest");
        let manifest_level = root_manifest
            .manifest_lint_levels()
            .get("DUPLICATE_PACKAGE_LINE")
            .expect("root lint level");
        let diagnostic = diagnostic(&selection, &duplicate, &manifests, None, false)
            .expect("manifest-denied lint is emitted");

        assert_eq!(diagnostic.severity, Severity::Error);
        let level = diagnostic.level().expect("lint level");
        assert_eq!(level.value, LintLevel::Deny);
        assert_eq!(level.source, LevelSource::Manifest);
        assert_eq!(level.location.as_ref(), Some(manifest_level.location()));
    }

    #[test]
    fn allow_suppresses_the_graph_lint_and_deny_warnings_escalates_it() {
        let (selection, duplicate) = duplicate_fixture();
        let manifests = manifests();
        assert!(
            diagnostic(
                &selection,
                &duplicate,
                &manifests,
                Some(sai_diagnostics::OverrideLintLevel::Allow),
                false,
            )
            .is_none()
        );

        let denied = diagnostic(&selection, &duplicate, &manifests, None, true)
            .expect("deny warnings keeps the lint");
        assert_eq!(denied.severity, Severity::Error);
        let level = denied.level().expect("lint level");
        assert_eq!(level.value, LintLevel::Warn);
        assert_eq!(level.source, LevelSource::DenyWarnings);
        assert_eq!(level.location, None);
    }

    #[test]
    fn requires_root_manifest_before_resolving_cli_allow() {
        let (selection, duplicate) = duplicate_fixture();
        let graph = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let error = duplicate_package_line_diagnostic(
            &graph,
            &duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &MvsManifestLocations::default(),
                cli: Some(sai_diagnostics::OverrideLintLevel::Allow),
                deny_warnings: false,
            },
        )
        .expect_err("root policy cannot be resolved without the root manifest");

        assert_eq!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::MissingRootManifest
        );
    }

    #[test]
    fn requires_a_manifest_for_every_requirement_edge() {
        let (selection, duplicate) = duplicate_fixture();
        let graph = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let manifests = manifests_for(
            (
                package_version("acme:app", "1.0.0"),
                ROOT_MANIFEST.to_owned(),
            ),
            Vec::new(),
        );
        let error = duplicate_package_line_diagnostic(
            &graph,
            &duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &manifests,
                cli: None,
                deny_warnings: false,
            },
        )
        .expect_err("source-less dependency edges cannot be rendered faithfully");

        assert_eq!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::MissingManifestLocation(Box::new(
                MvsRequirer::Package(package_version("acme:a", "1.0.0"))
            ))
        );
    }

    #[test]
    fn rejects_snapshot_metadata_for_a_different_package_version() {
        let root_version = package_version("acme:app", "1.0.0");
        let snapshot = WorkspaceSnapshot::new([snapshot_file(
            1,
            "sai.package".to_owned(),
            &root_version,
            ROOT_MANIFEST,
        )])
        .expect("valid source snapshot");
        let mismatched = package_version("acme:other", "9.0.0");
        let error = MvsManifestLocation::from_snapshot(
            &snapshot,
            sai_diagnostics::FileId::new(1),
            &mismatched,
            crate::manifest::PackageRole::Root,
        )
        .expect_err("snapshot metadata must match the MVS package");

        assert_eq!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::ManifestPackageMismatch {
                requirer: Box::new(MvsRequirer::Package(mismatched.clone())),
                expected: "acme:other@9.0.0".to_owned(),
                found: Some("acme:app@1.0.0".to_owned()),
            }
        );
    }

    #[test]
    fn rejects_manifest_headers_that_disagree_with_snapshot_identity() {
        let expected = package_version("acme:app", "1.0.0");
        let source = "package acme:other @ 9.0.0\n";
        let snapshot = WorkspaceSnapshot::new([snapshot_file(
            1,
            "sai.package".to_owned(),
            &expected,
            source,
        )])
        .expect("valid source snapshot");

        let error = MvsManifestLocation::from_snapshot(
            &snapshot,
            sai_diagnostics::FileId::new(1),
            &expected,
            crate::manifest::PackageRole::Root,
        )
        .expect_err("the parsed package header must match the MVS package");

        assert_eq!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::ManifestHeaderMismatch {
                file_id: sai_diagnostics::FileId::new(1),
                expected: Box::new(expected),
                found: Some("acme:other@9.0.0".to_owned()),
            }
        );
    }

    #[test]
    fn rejects_non_manifest_paths_for_root_and_dependency_packages() {
        let root_version = package_version("acme:app", "1.0.0");
        let root_snapshot = WorkspaceSnapshot::new([snapshot_file(
            1,
            "src/copied.sai".to_owned(),
            &root_version,
            ROOT_MANIFEST,
        )])
        .expect("canonical workspace path");
        assert_eq!(
            MvsManifestLocation::from_snapshot(
                &root_snapshot,
                sai_diagnostics::FileId::new(1),
                &root_version,
                crate::manifest::PackageRole::Root,
            ),
            Err(
                super::MvsDuplicatePackageLineDiagnosticError::ManifestPathMismatch {
                    file_id: sai_diagnostics::FileId::new(1),
                    expected: "workspace-relative path ending in `sai.package`".to_owned(),
                    found: "src/copied.sai".to_owned(),
                }
            )
        );

        let dependency_version = package_version("acme:a", "1.0.0");
        let dependency_snapshot = WorkspaceSnapshot::new([snapshot_file(
            1,
            "dep:acme:a@1.0.0/src/copied.sai".to_owned(),
            &dependency_version,
            A_MANIFEST,
        )])
        .expect("canonical dependency path");
        assert_eq!(
            MvsManifestLocation::from_snapshot(
                &dependency_snapshot,
                sai_diagnostics::FileId::new(1),
                &dependency_version,
                crate::manifest::PackageRole::Dependency,
            ),
            Err(
                super::MvsDuplicatePackageLineDiagnosticError::ManifestPathMismatch {
                    file_id: sai_diagnostics::FileId::new(1),
                    expected: "dep:acme:a@1.0.0/sai.package".to_owned(),
                    found: "dep:acme:a@1.0.0/src/copied.sai".to_owned(),
                }
            )
        );
    }

    #[test]
    fn diagnostic_source_references_use_document_versions_and_validate_against_vfs() {
        let (selection, duplicate) = duplicate_fixture();
        let root_version = package_version("acme:app", "1.0.0");
        let dependency_version = package_version("acme:a", "1.0.0");
        let snapshot = WorkspaceSnapshot::new([
            snapshot_file_with_document_version(
                1,
                "sai.package".to_owned(),
                &root_version,
                ROOT_MANIFEST,
                Some(10),
            ),
            snapshot_file_with_document_version(
                2,
                "dep:acme:a@1.0.0/sai.package".to_owned(),
                &dependency_version,
                A_MANIFEST,
                Some(20),
            ),
        ])
        .expect("valid source snapshot");
        let mut manifests = MvsManifestLocations::default();
        manifests
            .set_root(
                MvsManifestLocation::from_snapshot(
                    &snapshot,
                    sai_diagnostics::FileId::new(1),
                    &root_version,
                    crate::manifest::PackageRole::Root,
                )
                .expect("root manifest location"),
            )
            .expect("root belongs to the snapshot");
        manifests
            .insert_package(
                MvsManifestLocation::from_snapshot(
                    &snapshot,
                    sai_diagnostics::FileId::new(2),
                    &dependency_version,
                    crate::manifest::PackageRole::Dependency,
                )
                .expect("dependency manifest location"),
            )
            .expect("dependency belongs to the snapshot");
        let diagnostic = diagnostic(&selection, &duplicate, &manifests, None, false)
            .expect("warning is enabled");

        let children = diagnostic.children();
        assert_eq!(
            children[2]
                .location
                .as_ref()
                .expect("root source location")
                .file
                .snapshot,
            Some(10)
        );
        assert_eq!(
            children[3]
                .location
                .as_ref()
                .expect("dependency source location")
                .file
                .snapshot,
            Some(20)
        );
        assert!(
            diagnostic.with_workspace_snapshot(&snapshot).is_ok(),
            "every generated location must match the exact VFS snapshot"
        );
    }

    #[test]
    fn includes_root_test_requirements_but_excludes_dependency_test_requirements() {
        let root_version = package_version("acme:app", "1.0.0");
        let root_source = "package acme:app @ 1.0.0\nrequires test { acme:a @ ^1 }\n";
        let root_snapshot = WorkspaceSnapshot::new([snapshot_file(
            1,
            "sai.package".to_owned(),
            &root_version,
            root_source,
        )])
        .expect("valid root snapshot");
        let root_manifest = MvsManifestLocation::from_snapshot(
            &root_snapshot,
            sai_diagnostics::FileId::new(1),
            &root_version,
            crate::manifest::PackageRole::Root,
        )
        .expect("valid root manifest");
        let root_span = source_span(root_source, "acme:a @ ^1");
        assert!(
            root_manifest
                .requirements
                .contains_key(&(root_span.start, root_span.end))
        );

        let dependency_version = package_version("acme:a", "1.0.0");
        let dependency_source = "package acme:a @ 1.0.0\nrequires test { acme:lib @ ^1 }\n";
        let dependency_snapshot = WorkspaceSnapshot::new([snapshot_file(
            1,
            "dep:acme:a@1.0.0/sai.package".to_owned(),
            &dependency_version,
            dependency_source,
        )])
        .expect("valid dependency snapshot");
        let dependency_manifest = MvsManifestLocation::from_snapshot(
            &dependency_snapshot,
            sai_diagnostics::FileId::new(1),
            &dependency_version,
            crate::manifest::PackageRole::Dependency,
        )
        .expect("valid dependency manifest");

        assert!(dependency_manifest.requirements.is_empty());
    }

    #[test]
    fn manifest_locations_cannot_mix_workspace_snapshots() {
        let root_version = package_version("acme:app", "1.0.0");
        let root_snapshot = WorkspaceSnapshot::new([snapshot_file(
            1,
            "sai.package".to_owned(),
            &root_version,
            ROOT_MANIFEST,
        )])
        .expect("valid root snapshot");
        let root_manifest = MvsManifestLocation::from_snapshot(
            &root_snapshot,
            sai_diagnostics::FileId::new(1),
            &root_version,
            crate::manifest::PackageRole::Root,
        )
        .expect("valid root manifest");

        let dependency_version = package_version("acme:a", "1.0.0");
        let dependency_snapshot = WorkspaceSnapshot::new([snapshot_file(
            1,
            "dep:acme:a@1.0.0/sai.package".to_owned(),
            &dependency_version,
            A_MANIFEST,
        )])
        .expect("valid dependency snapshot");
        let dependency_manifest = MvsManifestLocation::from_snapshot(
            &dependency_snapshot,
            sai_diagnostics::FileId::new(1),
            &dependency_version,
            crate::manifest::PackageRole::Dependency,
        )
        .expect("valid dependency manifest");

        let expected = root_snapshot.id();
        let found = dependency_snapshot.id();
        let mut locations = MvsManifestLocations::default();
        locations
            .set_root(root_manifest)
            .expect("root snapshot registered");
        assert_eq!(
            locations.insert_package(dependency_manifest),
            Err(
                super::MvsDuplicatePackageLineDiagnosticError::WorkspaceSnapshotMismatch {
                    expected,
                    found,
                }
            )
        );
    }

    #[test]
    fn rejects_requirement_spans_outside_their_manifest() {
        let span = ROOT_MANIFEST.len() + 1..ROOT_MANIFEST.len() + 2;
        let (selection, duplicate) = duplicate_fixture_with_root_edge("acme:a", "^1", span.clone());
        let graph = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let manifests = manifests();
        let error = duplicate_package_line_diagnostic(
            &graph,
            &duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &manifests,
                cli: None,
                deny_warnings: false,
            },
        )
        .expect_err("out-of-file source range must be rejected");

        assert_eq!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::SourceSpanOutOfRange {
                requirer: Box::new(MvsRequirer::Root(package_version("acme:app", "1.0.0"))),
                span,
            }
        );
    }

    #[test]
    fn rejects_in_bounds_spans_that_are_not_package_requirements() {
        let (selection, duplicate) = duplicate_fixture_with_root_edge("acme:a", "^1", 0..1);
        let graph = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let manifests = manifests();
        let error = duplicate_package_line_diagnostic(
            &graph,
            &duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &manifests,
                cli: None,
                deny_warnings: false,
            },
        )
        .expect_err("an in-bounds non-requirement span is not a valid source mapping");

        assert_eq!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::RequirementNotInManifest {
                requirer: Box::new(MvsRequirer::Root(package_version("acme:app", "1.0.0"))),
                span: 0..1,
            }
        );
    }

    #[test]
    fn rejects_requirement_data_that_disagrees_with_parsed_source() {
        let span = source_span(ROOT_MANIFEST, "acme:a @ ^1 from path \"../a\"");
        let (selection, duplicate) =
            duplicate_fixture_with_root_edge("acme:a", ">=1", span.clone());
        let graph = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let manifests = manifests();
        let error = duplicate_package_line_diagnostic(
            &graph,
            &duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &manifests,
                cli: None,
                deny_warnings: false,
            },
        )
        .expect_err("graph requirements must match the source manifest");

        assert_eq!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::RequirementMismatch {
                requirer: Box::new(MvsRequirer::Root(package_version("acme:app", "1.0.0"))),
                span,
                expected_package: "acme:a".into(),
                expected_requirement: Box::new(
                    VersionReq::parse(">=1").expect("valid requirement"),
                ),
                found_package: "acme:a".into(),
                found_requirement: Box::new(VersionReq::parse("^1").expect("valid requirement"),),
            }
        );
    }

    #[test]
    fn records_lock_floor_as_the_origin_of_a_floor_only_line() {
        let (selection, duplicate, manifests) = duplicate_with_lock_floor_fixture();
        let graph = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let diagnostic = duplicate_package_line_diagnostic(
            &graph,
            &duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &manifests,
                cli: None,
                deny_warnings: false,
            },
        )
        .expect("duplicate diagnostic")
        .expect("warning is enabled");

        let Value::Array(lines) = diagnostic.data.get("lines").expect("line data") else {
            panic!("lines must be an array");
        };
        let Value::Object(floor_line) = &lines[1] else {
            panic!("line must be an object");
        };
        assert_eq!(
            floor_line.get("origin"),
            Some(&Value::String("lock_entry".to_owned()))
        );
        assert_eq!(
            floor_line.get("origin_package"),
            Some(&Value::String("acme:lib".to_owned()))
        );
        assert_eq!(
            floor_line.get("origin_version"),
            Some(&Value::String("2.5.0".to_owned()))
        );
        assert_eq!(floor_line.get("chain"), Some(&Value::Array(vec![])));
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.message == "lock entry acme:lib 2.5.0 (`sai.lock`)")
        );
    }

    #[test]
    fn marks_unselected_candidate_requirers_in_rendered_and_structured_chains() {
        let (selection, duplicate, locations) = duplicate_with_candidate_chain_fixture();
        let graph = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let diagnostic = duplicate_package_line_diagnostic(
            &graph,
            &duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &locations,
                cli: None,
                deny_warnings: false,
            },
        )
        .expect("duplicate diagnostic")
        .expect("warning is enabled");

        let Value::Array(lines) = diagnostic.data.get("lines").expect("line data") else {
            panic!("lines must be an array");
        };
        let Value::Object(second_line) = &lines[1] else {
            panic!("line must be an object");
        };
        let Value::Array(chain) = second_line.get("chain").expect("chain data") else {
            panic!("chain must be an array");
        };
        let Value::Object(last_link) = chain.last().expect("final requirement link") else {
            panic!("link must be an object");
        };
        assert_eq!(
            last_link.get("requirer_selected"),
            Some(&Value::Bool(false))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.message == "acme:b 1.0.0 (not selected) requires acme:lib @ ^2")
        );
    }

    #[test]
    fn version_conflict_reports_selected_and_rejecting_requirement_chains() {
        let (selection, manifests) = version_conflict_fixture();
        let diagnostics =
            version_conflict_diagnostics(&selection, &manifests).expect("conflict diagnostics");
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("expected one conflict diagnostic");
        };

        assert_eq!(diagnostic.code.as_str(), "VERSION_CONFLICT");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(
            diagnostic.message,
            "acme:lib#1 is selected at 1.9.0, but acme:b 1.0.0 requires `>=1.4, <1.8`"
        );
        assert_eq!(
            diagnostic.primary().expect("primary requirement").file.path,
            "dep:acme:b@1.0.0/sai.package"
        );
        assert_eq!(
            diagnostic.data.get("selected_reason"),
            Some(&Value::String("requirement".to_owned()))
        );
        assert_eq!(
            diagnostic.data.get("selected_version"),
            Some(&Value::String("1.9.0".to_owned()))
        );
        let Value::Object(rejected_chain) = diagnostic
            .data
            .get("requirement_chain")
            .expect("structured rejecting chain")
        else {
            panic!("requirement chain must be an object");
        };
        let Value::Object(origin) = rejected_chain.get("origin").expect("chain origin") else {
            panic!("chain origin must be an object");
        };
        assert_eq!(origin.get("kind"), Some(&Value::String("root".to_owned())));
        let Value::Array(steps) = rejected_chain.get("steps").expect("chain steps") else {
            panic!("chain steps must be an array");
        };
        let Value::Object(rejected_step) = steps.last().expect("final conflict step") else {
            panic!("chain step must be an object");
        };
        assert_eq!(
            rejected_step.get("requirement"),
            Some(&Value::String(">=1.4, <1.8".to_owned()))
        );
        assert_eq!(
            rejected_step.get("target_version"),
            Some(&Value::String("1.4.0".to_owned()))
        );
        assert!(
            diagnostic.children().iter().any(|child| child.message
                == "acme:a 1.0.0 requires acme:lib @ ^1.9 (names 1.9.0)")
        );
        assert!(
            diagnostic.children().iter().any(|child| child.message
                == "acme:b 1.0.0 requires acme:lib @ >=1.4, <1.8 (names 1.4.0)")
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.kind == ChildKind::Help
                    && child.message.contains("sai update acme:b"))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.kind == ChildKind::Help
                    && child.message.contains("use path` or `use git"))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.kind == ChildKind::Help
                    && child.message
                        == "or override acme:b with a patched copy using `use path` or `use git`")
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.kind == ChildKind::Help
                    && child.message
                        == "lower the root requirement for acme:lib to a version accepted by acme:b (>=1.4, <1.8)")
        );

        let related = diagnostic.related();
        assert_eq!(related.len(), 1);
        let Related::RequirementChain {
            steps: conflicting_chain,
        } = &related[0]
        else {
            panic!("the related entry explains the conflicting requirement");
        };
        assert_eq!(conflicting_chain.len(), 3);
        assert_eq!(conflicting_chain[0].relation, Relation::Requires);
        assert_eq!(conflicting_chain[0].name, "acme:app 1.0.0 (root)");
        assert_eq!(conflicting_chain[1].relation, Relation::Requires);
        assert_eq!(conflicting_chain[1].name, "acme:b 1.0.0");
        assert_eq!(
            conflicting_chain[2].name,
            "acme:lib @ >=1.4, <1.8 (names 1.4.0)"
        );
        assert!(conflicting_chain[2].location.is_none());
        assert_eq!(
            conflicting_chain[1]
                .location
                .as_ref()
                .expect("structured edge source")
                .file
                .path,
            "dep:acme:b@1.0.0/sai.package"
        );
    }

    #[test]
    fn suggests_lowering_the_root_requirement_when_it_selected_the_version() {
        let (selection, manifests) = root_selected_version_conflict_fixture();
        let diagnostics =
            version_conflict_diagnostics(&selection, &manifests).expect("conflict diagnostics");
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("expected one conflict diagnostic");
        };

        assert_eq!(
            diagnostic.data.get("requirer_is_root"),
            Some(&Value::Bool(false))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.kind == ChildKind::Help
                    && child.message
                        == "lower the root requirement for acme:lib to a version accepted by acme:b (>=1.4, <1.8)")
        );
    }

    #[test]
    fn singular_version_conflict_diagnostic_rejects_a_conflict_from_another_selection() {
        let (selection, manifests) = version_conflict_fixture();
        let explanations = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let unrelated = super::MvsVersionConflict {
            requirer: MvsRequirer::Root(package_version("acme:app", "1.0.0")),
            named_target: package_version("acme:lib", "1.9.0"),
            target: package_version("acme:lib", "1.9.0"),
            requirement: VersionReq::parse("=2.0.0").expect("valid requirement"),
            span: 1..2,
        };

        let error = version_conflict_diagnostic(&explanations, &unrelated, &manifests)
            .expect_err("conflict must belong to the explanation graph");
        assert!(matches!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(_)
        ));
    }

    #[test]
    fn does_not_suggest_update_for_a_path_pin_below_a_dependency_minimum() {
        let (selection, manifests) = path_pinned_lower_bound_conflict_fixture();
        let diagnostics =
            version_conflict_diagnostics(&selection, &manifests).expect("conflict diagnostics");
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("expected one conflict diagnostic");
        };

        assert_eq!(
            diagnostic.data.get("selected_reason"),
            Some(&Value::String("path_pin".to_owned()))
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.message.contains("pinned by this path requirement"))
        );
        assert!(
            !diagnostic
                .children()
                .iter()
                .any(|child| child.message.contains("sai update"))
        );
        assert!(diagnostic.children().iter().any(|child| child.message
            == "or override acme:lib with a patched copy using `use path` or `use git`"));
    }

    #[test]
    fn dependency_cycle_diagnostic_lists_each_selected_edge_and_location() {
        let (selection, manifests) = dependency_cycle_fixture();
        let diagnostics =
            super::dependency_cycle_diagnostics(&selection, &manifests).expect("cycle diagnostic");
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("expected one cycle diagnostic");
        };

        assert_eq!(diagnostic.code.as_str(), "DEPENDENCY_CYCLE");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(
            diagnostic.message,
            "selected package versions form a dependency cycle: acme:a 1.0.0 -> acme:b 1.0.0 -> acme:a 1.0.0"
        );
        assert_eq!(
            diagnostic.primary().expect("primary cycle edge").file.path,
            "dep:acme:a@1.0.0/sai.package"
        );
        assert_eq!(
            diagnostic.children()[0].message,
            "acme:a 1.0.0 requires acme:b @ ^1 (selected 1.0.0)"
        );
        assert_eq!(
            diagnostic.children()[0]
                .location
                .as_ref()
                .expect("cycle edge location")
                .file
                .path,
            "dep:acme:a@1.0.0/sai.package"
        );
        assert_eq!(
            diagnostic.children()[1]
                .location
                .as_ref()
                .expect("cycle edge location")
                .file
                .path,
            "dep:acme:b@1.0.0/sai.package"
        );
        assert!(
            diagnostic.children().iter().any(|child| {
                child
                    .location
                    .as_ref()
                    .is_some_and(|location| location.file.path == "sai.package")
            }),
            "the root-to-cycle chain should retain the root requirement location"
        );
        let Value::Object(requirement_chain) = diagnostic
            .data
            .get("requirement_chain")
            .expect("root-to-cycle requirement chain")
        else {
            panic!("the cycle requirement chain must be an object");
        };
        let Value::Array(chain_steps) = requirement_chain
            .get("steps")
            .expect("cycle requirement chain steps")
        else {
            panic!("the cycle requirement chain steps must be an array");
        };
        assert_eq!(chain_steps.len(), 3);
        assert_eq!(diagnostic.related().len(), 1);
        let Value::Array(nodes) = diagnostic.data.get("nodes").expect("cycle nodes") else {
            panic!("cycle nodes must be an array");
        };
        assert_eq!(nodes.len(), 3);
        let Value::Object(last_node) = nodes.last().expect("closed cycle") else {
            panic!("cycle node must be an object");
        };
        assert_eq!(
            last_node.get("package"),
            Some(&Value::String("acme:a".to_owned()))
        );
        let Value::Array(edges) = diagnostic.data.get("edges").expect("cycle edges") else {
            panic!("cycle edges must be an array");
        };
        assert_eq!(edges.len(), 2);
    }

    #[test]
    fn rejects_a_cycle_that_is_not_the_selected_graphs_canonical_cycle() {
        let (selection, manifests) = dependency_cycle_fixture();
        let unrelated = super::MvsDependencyCycle {
            nodes: vec![
                package_version("acme:b", "1.0.0"),
                package_version("acme:a", "1.0.0"),
                package_version("acme:b", "1.0.0"),
            ],
            spans: vec![0..1, 1..2],
        };

        let error = super::dependency_cycle_diagnostic(&selection, &unrelated, &manifests)
            .expect_err("a caller cannot choose a different cycle");
        assert!(matches!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::CycleNotInSelectedGraph(_)
        ));
    }

    #[test]
    fn selected_graph_without_a_cycle_emits_no_cycle_diagnostic() {
        let (selection, _) = duplicate_fixture();

        assert!(
            super::dependency_cycle_diagnostics(&selection, &MvsManifestLocations::default())
                .expect("valid acyclic graph")
                .is_empty()
        );
    }

    #[test]
    fn selected_cycle_provenance_preserves_its_lock_floor_origin() {
        let (selection, manifests) = dependency_cycle_with_floor_fixture();
        let diagnostics =
            super::dependency_cycle_diagnostics(&selection, &manifests).expect("cycle diagnostic");
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("expected one cycle diagnostic");
        };

        let Value::Object(chain) = diagnostic
            .data
            .get("requirement_chain")
            .expect("cycle-closing requirement chain")
        else {
            panic!("requirement chain must be an object");
        };
        let Value::Object(origin) = chain.get("origin").expect("chain origin") else {
            panic!("chain origin must be an object");
        };
        assert_eq!(
            origin.get("kind"),
            Some(&Value::String("lock_floor".to_owned()))
        );
        assert_eq!(
            origin.get("package"),
            Some(&Value::String("acme:a".to_owned()))
        );
        assert_eq!(
            origin.get("version"),
            Some(&Value::String("1.5.0".to_owned()))
        );
    }

    #[test]
    fn final_identity_failure_keeps_its_chain_beside_a_root_cycle() {
        let (root_source, root_spans) =
            manifest_source("acme:app", "1.0.0", &[("acme:a", "^1"), ("acme:c", "^1")]);
        let (a_source, a_spans) = manifest_source("acme:a", "1.0.0", &[("acme:b", "^1")]);
        let (b_source, b_spans) = manifest_source("acme:b", "1.0.0", &[("acme:app", "^1")]);
        let (c_source, _) = manifest_source("acme:c", "1.0.0", &[]);
        let root = MvsContract::new(
            package_version("acme:app", "1.0.0"),
            vec![
                edge_with_span("acme:a", "^1", "1.0.0", root_spans[0].clone()),
                edge_with_span("acme:c", "^1", "1.0.0", root_spans[1].clone()),
            ],
        )
        .expect("valid root contract");
        let a = MvsContract::new(
            package_version("acme:a", "1.0.0"),
            vec![edge_with_span("acme:b", "^1", "1.0.0", a_spans[0].clone())],
        )
        .expect("valid dependency contract");
        let b = MvsContract::new(
            package_version("acme:b", "1.0.0"),
            vec![edge_with_span(
                "acme:app",
                "^1",
                "1.0.0",
                b_spans[0].clone(),
            )],
        )
        .expect("valid dependency contract");
        let c = MvsContract::new(package_version("acme:c", "1.0.0"), vec![])
            .expect("valid independent dependency contract");
        let contracts = HashMap::from([
            (a.package_version().clone(), a),
            (b.package_version().clone(), b),
            (c.package_version().clone(), c),
        ]);
        let selection =
            select(&root, &contracts, &HashMap::new(), &HashMap::new()).expect("MVS selection");
        assert!(!selection.is_complete());
        let manifests = manifests_for(
            (package_version("acme:app", "1.0.0"), root_source),
            vec![
                (package_version("acme:a", "1.0.0"), a_source),
                (package_version("acme:b", "1.0.0"), b_source),
                (package_version("acme:c", "1.0.0"), c_source),
            ],
        );
        let failure = ResolverSourceIdentityFailure {
            package_version: package_version("acme:c", "1.0.0"),
            side: ResolverSourceIdentitySide::Final,
            source: None,
            error: ResolverSourceIdentityFailureKind::Canonicalization(Box::new(
                crate::mixed_source::MixedSourceFinalIdentityError::LocalPath(
                    crate::local_source::LocalPackageError::MissingManifest,
                ),
            )),
        };

        let diagnostics =
            mixed_source_identity_failure_diagnostics(&selection, &[failure], &manifests)
                .expect("root cycle does not suppress an independent identity failure");
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("expected one identity diagnostic");
        };

        assert_eq!(diagnostic.code.as_str(), "PATH_PACKAGE_NOT_FOUND");
        assert_eq!(
            diagnostic
                .primary()
                .expect("requirement edge location")
                .file
                .path,
            "sai.package"
        );
        let Value::Object(chain) = diagnostic
            .data
            .get("requirement_chain")
            .expect("identity requirement chain")
        else {
            panic!("requirement chain must be an object");
        };
        let Value::Array(steps) = chain.get("steps").expect("requirement steps") else {
            panic!("requirement steps must be an array");
        };
        assert_eq!(steps.len(), 1);
    }

    #[test]
    fn root_cycle_diagnostic_reports_the_path_and_indirect_edge() {
        let (selection, manifests) = root_dependency_cycle_fixture();
        let diagnostics = super::root_dependency_cycle_diagnostics(&selection, &manifests)
            .expect("root cycle diagnostics");
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("expected one root cycle diagnostic");
        };

        assert_eq!(diagnostic.code.as_str(), "DEPENDENCY_CYCLE");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert!(
            diagnostic
                .message
                .ends_with("acme:app 1.0.0 -> acme:a 1.0.0 -> acme:b 1.0.0 -> acme:app 1.0.0")
        );
        assert_eq!(
            diagnostic.primary().expect("closing edge").file.path,
            "dep:acme:b@1.0.0/sai.package"
        );
        assert!(
            diagnostic
                .children()
                .iter()
                .any(|child| child.message == "acme:b 1.0.0 requires acme:app @ ^1 (names 1.0.0)")
        );
        let Value::Array(nodes) = diagnostic.data.get("nodes").expect("cycle nodes") else {
            panic!("cycle nodes must be an array");
        };
        assert_eq!(nodes.len(), 4);
        let Value::Array(edges) = diagnostic.data.get("edges").expect("cycle edges") else {
            panic!("cycle edges must be an array");
        };
        assert_eq!(edges.len(), 3);
        assert_eq!(diagnostic.related().len(), 1);
    }

    #[test]
    fn root_cycle_closes_at_the_selected_root_version() {
        let (selection, manifests) = root_dependency_cycle_fixture_with("1.4.0", "1.0.0", false);
        let diagnostics = super::root_dependency_cycle_diagnostics(&selection, &manifests)
            .expect("root cycle diagnostics");
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("expected one root cycle diagnostic");
        };

        let Value::Array(nodes) = diagnostic.data.get("nodes").expect("cycle nodes") else {
            panic!("cycle nodes must be an array");
        };
        assert_eq!(nodes.len(), 4);
        let Value::Object(last_node) = nodes.last().expect("closed cycle") else {
            panic!("cycle node must be an object");
        };
        assert_eq!(
            last_node.get("package"),
            Some(&Value::String("acme:app".to_owned()))
        );
        assert_eq!(
            last_node.get("version"),
            Some(&Value::String("1.4.0".to_owned()))
        );

        let Value::Array(edges) = diagnostic.data.get("edges").expect("cycle edges") else {
            panic!("cycle edges must be an array");
        };
        let Value::Object(closing_edge) = edges.last().expect("closing edge") else {
            panic!("cycle edge must be an object");
        };
        assert_eq!(
            closing_edge.get("target_version"),
            Some(&Value::String("1.4.0".to_owned()))
        );
        assert_eq!(
            closing_edge.get("named_target_version"),
            Some(&Value::String("1.0.0".to_owned()))
        );
    }

    #[test]
    fn root_cycle_edge_records_selected_and_named_target_versions() {
        let (selection, manifests) =
            root_dependency_cycle_fixture_with_a_floor("1.0.0", "1.0.0", false, Some("1.5.0"));
        let diagnostics = super::root_dependency_cycle_diagnostics(&selection, &manifests)
            .expect("root cycle diagnostics");
        let [diagnostic] = diagnostics.as_slice() else {
            panic!("expected one root cycle diagnostic");
        };

        let Value::Array(nodes) = diagnostic.data.get("nodes").expect("cycle nodes") else {
            panic!("cycle nodes must be an array");
        };
        let Value::Object(first_target) = nodes.get(1).expect("first dependency") else {
            panic!("cycle node must be an object");
        };
        assert_eq!(
            first_target.get("version"),
            Some(&Value::String("1.0.0".to_owned()))
        );

        let Value::Array(edges) = diagnostic.data.get("edges").expect("cycle edges") else {
            panic!("cycle edges must be an array");
        };
        let Value::Object(root_edge) = edges.first().expect("root edge") else {
            panic!("cycle edge must be an object");
        };
        assert_eq!(
            root_edge.get("target_version"),
            Some(&Value::String("1.5.0".to_owned()))
        );
        assert_eq!(
            root_edge.get("named_target_version"),
            Some(&Value::String("1.0.0".to_owned()))
        );
    }

    #[test]
    fn root_cycle_is_reported_alongside_an_independent_missing_contract() {
        let (selection, manifests) = root_dependency_cycle_fixture_with("1.0.0", "1.0.0", true);
        assert!(selection.errors().len() > 1);

        let diagnostics = super::root_dependency_cycle_diagnostics(&selection, &manifests)
            .expect("the visited root cycle remains explainable");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("dependency cycle reaches"));
    }

    #[test]
    fn rejects_root_cycle_errors_from_another_selection() {
        let (selection, manifests) = root_dependency_cycle_fixture();
        let mut unrelated = selection.errors()[0].clone();
        unrelated.span = Some(0..1);

        let error = super::root_dependency_cycle_diagnostic(&selection, &unrelated, &manifests)
            .expect_err("a caller cannot pair a different cycle error");
        assert!(matches!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::RootCycleNotInSelection(_)
        ));
    }

    #[test]
    fn complete_selection_has_no_root_cycle_diagnostics() {
        let (selection, _) = duplicate_fixture();

        assert!(
            super::root_dependency_cycle_diagnostics(&selection, &MvsManifestLocations::default())
                .expect("complete selection")
                .is_empty()
        );
    }
}
