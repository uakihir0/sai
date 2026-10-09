//! Diagnostics produced from resolved package-graph facts.

use std::collections::BTreeMap;
use std::ops::Range;

use crate::manifest::{PackageDeclaration, PackageParseError, PackageRole, parse_for_role};
use crate::mvs::{MvsPackageVersion, MvsRequirer, MvsSelectionOutput};
use crate::mvs_explanations::{MvsChainOrigin, MvsExplanationError, MvsExplanationGraph};
use crate::mvs_graph_validation::{
    MvsDuplicatePackageLines, MvsSelectedGraphInputError, check_selected_graph,
    duplicate_package_lines,
};
use crate::version_req::VersionReq;
use sai_diagnostics::{
    Child, ChildKind, Code, Diagnostic, LintPolicies, LintResolution, LintResolutionContext,
    LintResolutionError, ManifestLintLevel, OverrideLintLevel, SourceRef, TextRange, TextSize,
    Value, ValueObject, resolve_lint_occurrence,
};

/// Source identity for one parsed package manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MvsManifestLocation {
    source: SourceRef,
    file_id: sai_diagnostics::FileId,
    package_version: MvsPackageVersion,
    role: PackageRole,
    workspace_snapshot: sai_diagnostics::WorkspaceSnapshotId,
    source_len: usize,
    requirements: BTreeMap<(usize, usize), ManifestRequirement>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ManifestRequirement {
    package: Box<str>,
    version: VersionReq,
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
        for declaration in &parsed.file().declarations {
            let PackageDeclaration::Requires { test, entries } = &declaration.value else {
                continue;
            };
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
                    },
                );
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
            role,
            workspace_snapshot: snapshot.id(),
            source_len: source_file.raw_bytes().len(),
            requirements,
        })
    }

    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    pub const fn file_id(&self) -> sai_diagnostics::FileId {
        self.file_id
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
    workspace_snapshot: Option<sai_diagnostics::WorkspaceSnapshotId>,
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

    pub fn root(&self) -> Option<&MvsManifestLocation> {
        self.root.as_ref()
    }

    pub fn packages(&self) -> &BTreeMap<MvsPackageVersion, MvsManifestLocation> {
        &self.packages
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
    /// Policies parsed from the root package manifest.
    pub root_policy: &'a LintPolicies,
    pub cli: Option<OverrideLintLevel>,
    /// The root package's validated `lints` setting for this code, if any.
    pub manifest: Option<&'a ManifestLintLevel>,
    pub deny_warnings: bool,
}

/// Invalid input while constructing a duplicate package-line diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MvsDuplicatePackageLineDiagnosticError {
    InvalidExplanation(Box<MvsExplanationError>),
    InvalidSelectedGraph(Box<MvsSelectedGraphInputError>),
    LintResolution(LintResolutionError),
    MissingManifestLocation(Box<MvsRequirer>),
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
    let explanation = explanations
        .explain_duplicate_lines(duplicate)
        .map_err(|error| {
            MvsDuplicatePackageLineDiagnosticError::InvalidExplanation(Box::new(error))
        })?;

    let no_attributes = [];
    let resolution = resolve_lint_occurrence(
        "DUPLICATE_PACKAGE_LINE",
        &LintResolutionContext {
            primary: None,
            attributes: &no_attributes,
            cli: context.cli,
            manifest: context.manifest,
            package_policy: context.root_policy,
            root_policy: context.root_policy,
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
    use std::collections::HashMap;
    use std::ops::Range;
    use std::sync::Arc;

    use sai_diagnostics::{
        LevelSource, LintLevel, Severity, TextRange, TextSize, Value, ValueObject,
    };
    use sai_vfs::{SnapshotFile, SourceFile, WorkspaceSnapshot};

    use super::{
        MvsDuplicatePackageLineDiagnosticContext, MvsManifestLocation, MvsManifestLocations,
        duplicate_package_line_diagnostic, duplicate_package_line_diagnostics,
    };
    use crate::manifest::parse;
    use crate::mvs::{
        MvsContract, MvsEdge, MvsPackageVersion, MvsRequirer, MvsSelectionOutput, select,
    };
    use crate::mvs_explanations::MvsExplanationGraph;
    use crate::mvs_graph_validation::{
        MvsDuplicatePackageLines, check_selected_graph, duplicate_package_lines,
    };
    use crate::validation::{PackageValidationContext, resolve_lint_policies, validate};
    use crate::version::Version;
    use crate::version_req::VersionReq;

    fn version(value: &str) -> Version {
        Version::parse(value).expect("valid test version")
    }

    fn package_version(package: &str, version_: &str) -> MvsPackageVersion {
        MvsPackageVersion::new(package, version(version_)).expect("valid package version")
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
        root_policy: &sai_diagnostics::LintPolicies,
        cli: Option<sai_diagnostics::OverrideLintLevel>,
        deny_warnings: bool,
    ) -> Option<sai_diagnostics::Diagnostic> {
        duplicate_package_line_diagnostic(
            &MvsExplanationGraph::new(selection).expect("explanation graph"),
            duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests,
                root_policy,
                cli,
                manifest: None,
                deny_warnings,
            },
        )
        .expect("duplicate diagnostic")
    }

    #[test]
    fn emits_source_mapped_duplicate_lines_without_a_primary_span() {
        let (selection, duplicate) = duplicate_fixture();
        let manifests = manifests();
        let diagnostic = diagnostic(
            &selection,
            &duplicate,
            &manifests,
            &sai_diagnostics::LintPolicies::default(),
            None,
            false,
        )
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
                root_policy: &sai_diagnostics::LintPolicies::default(),
                cli: None,
                manifest: None,
                deny_warnings: false,
            },
        )
        .expect("selected graph diagnostics");

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code.as_str(), "DUPLICATE_PACKAGE_LINE");
    }

    #[test]
    fn root_policy_sets_error_level_and_keeps_its_manifest_location() {
        let (selection, duplicate) = duplicate_fixture();
        let source = format!("{ROOT_MANIFEST}policy {{ deny duplicate_lines }}\n");
        let manifests = manifests_with(&source, A_MANIFEST);
        let parsed = parse(source.as_bytes());
        assert!(validate(&parsed, PackageValidationContext::default()).is_valid());
        let root_manifest = manifests.root().expect("root manifest");
        let source_ref = root_manifest.source().clone();
        let policy = resolve_lint_policies(&parsed, source_ref.clone(), root_manifest.file_id())
            .expect("valid root policy");
        let policy_location = policy
            .deny_duplicate_package_lines
            .as_ref()
            .expect("duplicate-line policy");
        let diagnostic = diagnostic(&selection, &duplicate, &manifests, &policy, None, false)
            .expect("policy-denied warning is emitted");

        assert_eq!(diagnostic.severity, Severity::Error);
        let level = diagnostic.level().expect("lint level");
        assert_eq!(level.value, LintLevel::Deny);
        assert_eq!(level.source, LevelSource::Policy);
        assert_eq!(level.location.as_ref(), Some(policy_location));
        assert_eq!(policy_location.file, source_ref);
    }

    #[test]
    fn allow_suppresses_the_graph_lint_and_deny_warnings_escalates_it() {
        let (selection, duplicate) = duplicate_fixture();
        let manifests = manifests();
        let policies = sai_diagnostics::LintPolicies::default();

        assert!(
            diagnostic(
                &selection,
                &duplicate,
                &manifests,
                &policies,
                Some(sai_diagnostics::OverrideLintLevel::Allow),
                false,
            )
            .is_none()
        );

        let denied = diagnostic(&selection, &duplicate, &manifests, &policies, None, true)
            .expect("deny warnings keeps the lint");
        assert_eq!(denied.severity, Severity::Error);
        let level = denied.level().expect("lint level");
        assert_eq!(level.value, LintLevel::Warn);
        assert_eq!(level.source, LevelSource::DenyWarnings);
        assert_eq!(level.location, None);
    }

    #[test]
    fn requires_manifest_locations_for_every_requirement_edge() {
        let (selection, duplicate) = duplicate_fixture();
        let graph = MvsExplanationGraph::new(&selection).expect("explanation graph");
        let error = duplicate_package_line_diagnostic(
            &graph,
            &duplicate,
            &MvsDuplicatePackageLineDiagnosticContext {
                manifests: &MvsManifestLocations::default(),
                root_policy: &sai_diagnostics::LintPolicies::default(),
                cli: None,
                manifest: None,
                deny_warnings: false,
            },
        )
        .expect_err("source-less requirement edges cannot be rendered faithfully");

        assert_eq!(
            error,
            super::MvsDuplicatePackageLineDiagnosticError::MissingManifestLocation(Box::new(
                MvsRequirer::Root(package_version("acme:app", "1.0.0"))
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
        let diagnostic = diagnostic(
            &selection,
            &duplicate,
            &manifests,
            &sai_diagnostics::LintPolicies::default(),
            None,
            false,
        )
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
                root_policy: &sai_diagnostics::LintPolicies::default(),
                cli: None,
                manifest: None,
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
                root_policy: &sai_diagnostics::LintPolicies::default(),
                cli: None,
                manifest: None,
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
                root_policy: &sai_diagnostics::LintPolicies::default(),
                cli: None,
                manifest: None,
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
                root_policy: &sai_diagnostics::LintPolicies::default(),
                cli: None,
                manifest: None,
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
                root_policy: &sai_diagnostics::LintPolicies::default(),
                cli: None,
                manifest: None,
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
}
