//! Cross-declaration validation for parsed package contracts.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::manifest::{
    OverrideSource, PackageDeclaration, PackageParseOutput, ProfileItem, Requirement, Spanned,
};
use crate::version::Version;

pub use crate::manifest::PackageRole;

/// A package manifest cannot supply lint policy data.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageLintPolicyError {
    PackageSyntaxInvalid,
    SourceSpanOutOfRange(Range<usize>),
}

/// Extracts validated lint-denying policy locations from a syntax-valid
/// package manifest.
///
/// Call this only after semantic validation succeeds. Package-local
/// `ambient_imports` and `runtime_contracts` apply to dependencies as well.
/// `duplicate_lines` is retained only for a root package because that policy
/// applies to the whole graph and dependency declarations are ignored.
pub fn resolve_lint_policies(
    parsed: &PackageParseOutput,
    source: sai_diagnostics::SourceRef,
    file_id: sai_diagnostics::FileId,
) -> Result<sai_diagnostics::LintPolicies, PackageLintPolicyError> {
    if !parsed.is_syntax_valid() {
        return Err(PackageLintPolicyError::PackageSyntaxInvalid);
    }

    let mut policies = sai_diagnostics::LintPolicies::default();
    for declaration in &parsed.file.declarations {
        let PackageDeclaration::Policy(items) = &declaration.value else {
            continue;
        };
        for item in items {
            let destination = match item.value.as_ref() {
                "ambient_imports" => &mut policies.deny_ambient_imports,
                "runtime_contracts" => &mut policies.deny_runtime_contracts,
                "duplicate_lines" if parsed.role() == PackageRole::Root => {
                    &mut policies.deny_duplicate_package_lines
                }
                _ => continue,
            };
            if destination.is_none() {
                *destination = Some(package_policy_location(
                    &source,
                    file_id,
                    item.span.clone(),
                )?);
            }
        }
    }

    Ok(policies)
}

fn package_policy_location(
    source: &sai_diagnostics::SourceRef,
    file_id: sai_diagnostics::FileId,
    range: Range<usize>,
) -> Result<sai_diagnostics::Location, PackageLintPolicyError> {
    let Some(start) = sai_diagnostics::TextSize::from_usize(range.start) else {
        return Err(PackageLintPolicyError::SourceSpanOutOfRange(range));
    };
    let Some(end) = sai_diagnostics::TextSize::from_usize(range.end) else {
        return Err(PackageLintPolicyError::SourceSpanOutOfRange(range));
    };
    let Some(range_value) = sai_diagnostics::TextRange::new(start, end) else {
        return Err(PackageLintPolicyError::SourceSpanOutOfRange(range));
    };
    Ok(sai_diagnostics::Location {
        file: source.clone(),
        span: sai_diagnostics::Span::new(file_id, range_value),
    })
}

/// Inputs that affect manifest validation but are not part of the manifest.
#[derive(Clone, Copy, Debug, Default)]
pub struct PackageValidationContext<'a> {
    /// The language version of the toolchain, when checking `compiler`.
    pub compiler_version: Option<&'a Version>,
}

/// One package-contract semantic error and its source range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageValidationError {
    pub kind: PackageValidationErrorKind,
    pub span: Range<usize>,
}

/// A semantic error determined from one package contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageValidationErrorKind {
    MissingTargets,
    PackageHasNoContent,
    UnknownTarget(Box<str>),
    OnlyTargetNotDeclared(Box<str>),
    ArtifactTargetNotDeclared(Box<str>),
    ArtifactOnCoreTarget(Box<str>),
    MissingArtifactForTarget(Box<str>),
    WitRequired,
    MissingExportWorld,
    InvalidWorldName(Box<str>),
    MissingSource(Box<str>),
    InvalidPackageName(Box<str>),
    InvalidDescription,
    ImplicitPackageRequired(Box<str>),
    PackageRequiresItself(Box<str>),
    DuplicateRequirement(Box<str>),
    InvalidCapabilityName(Box<str>),
    CapabilityAllowDenyConflict(Box<str>),
    UnknownPolicyItem(Box<str>),
    PolicyItemIsCapability(Box<str>),
    UnknownLint(Box<str>),
    NotALint(Box<str>),
    UnknownProfile(Box<str>),
    ProfileAllowExceedsPackage(Box<str>),
    UnknownBudgetMetric(Box<str>),
    InvalidSize,
    AllocatorNotRequired(Box<str>),
    DuplicateOverride {
        package: Box<str>,
        kind: OverrideKind,
    },
    CompilerVersionMismatch {
        requirement: Box<str>,
        found: Box<str>,
    },
}

/// The two independent override slots for one package: source and artifact.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OverrideKind {
    Source,
    Artifact,
}

/// One package-contract semantic warning and its source range.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageValidationWarning {
    pub kind: PackageValidationWarningKind,
    pub span: Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageValidationWarningKind {
    VersionRequirementSpansLines,
    InvalidLicenseExpression,
}

/// Semantic diagnostics derived from one parsed package contract.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackageValidationOutput {
    pub errors: Vec<PackageValidationError>,
    pub warnings: Vec<PackageValidationWarning>,
}

impl PackageValidationOutput {
    /// Reports whether semantic validation found no errors. This does not
    /// include syntax or lexical diagnostics from [`PackageParseOutput`].
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Checks package-wide rules that do not require reading the package tree or
/// resolving its dependencies.
///
/// Local checks still run on successfully parsed declarations when syntax
/// errors exist. Checks that depend on an absent declaration, a complete list,
/// or a relationship between declarations are skipped for syntactically
/// incomplete files to avoid cascading diagnostics from the partial syntax
/// tree.
pub fn validate(
    parsed: &PackageParseOutput,
    context: PackageValidationContext<'_>,
) -> PackageValidationOutput {
    let mut output = PackageValidationOutput::default();
    let complete = parsed.is_syntax_valid();
    let file = &parsed.file;
    let targets = file.declarations.iter().find_map(|declaration| {
        if let PackageDeclaration::Targets(targets) = &declaration.value {
            Some(targets)
        } else {
            None
        }
    });
    let package_identity = file
        .header
        .as_ref()
        .map(|header| header.value.identity.to_string());
    let mut declared_targets = HashSet::new();
    let mut package_allow = Vec::<&Spanned<Box<str>>>::new();
    let mut package_deny = Vec::<&Spanned<Box<str>>>::new();
    let mut required_packages = HashSet::<String>::new();
    let ordinary_requirements = file
        .declarations
        .iter()
        .filter_map(|declaration| match &declaration.value {
            PackageDeclaration::Requires {
                test: false,
                entries,
            } => Some(entries),
            _ => None,
        })
        .flatten()
        .map(|entry| entry.value.package.to_string())
        .collect::<HashSet<_>>();
    let mut overrides = HashMap::<(&str, OverrideKind), Range<usize>>::new();
    let mut has_source = false;
    let mut has_artifact = false;
    let mut has_wit = false;
    let mut has_export_world = false;
    let mut artifact_targets = HashSet::<&str>::new();

    for declaration in &file.declarations {
        match &declaration.value {
            PackageDeclaration::Compiler(requirement) => {
                if let Some(version) = context.compiler_version {
                    if !requirement.matches(version) {
                        output.errors.push(PackageValidationError {
                            kind: PackageValidationErrorKind::CompilerVersionMismatch {
                                requirement: requirement.to_string().into(),
                                found: version.to_string().into(),
                            },
                            span: declaration.span.clone(),
                        });
                    }
                }
            }
            PackageDeclaration::Source(_) => has_source = true,
            PackageDeclaration::Wit(_) => has_wit = true,
            PackageDeclaration::ExportWorld(world) => {
                has_export_world = true;
                if !is_valid_export_world(world) {
                    output.errors.push(PackageValidationError {
                        kind: PackageValidationErrorKind::InvalidWorldName(world.clone()),
                        span: declaration.span.clone(),
                    });
                }
            }
            PackageDeclaration::Targets(items) => {
                for item in items {
                    if !crate::manifest::is_target_name(&item.value) {
                        output.errors.push(PackageValidationError {
                            kind: PackageValidationErrorKind::UnknownTarget(item.value.clone()),
                            span: item.span.clone(),
                        });
                    } else {
                        declared_targets.insert(item.value.as_ref());
                    }
                }
            }
            PackageDeclaration::Requires { test, entries } => {
                if *test && parsed.role() == PackageRole::Dependency {
                    continue;
                }
                for entry in entries {
                    validate_requirement(
                        entry,
                        package_identity.as_deref(),
                        &mut required_packages,
                        &mut output,
                    );
                }
            }
            PackageDeclaration::Allow(items) => {
                package_allow.extend(items);
                validate_capability_items(items, &mut output);
            }
            PackageDeclaration::Deny(items) => {
                package_deny.extend(items);
                validate_capability_items(items, &mut output);
            }
            PackageDeclaration::Policy(items) => {
                for item in items {
                    validate_policy_item(item, &mut output);
                }
            }
            PackageDeclaration::Lints(items) if parsed.role() == PackageRole::Root => {
                for item in items {
                    match sai_diagnostics::lookup_lint_name(&item.value.code) {
                        sai_diagnostics::LintNameLookup::Lint(_) => {}
                        sai_diagnostics::LintNameLookup::HardDiagnostic(_) => {
                            output.errors.push(PackageValidationError {
                                kind: PackageValidationErrorKind::NotALint(item.value.code.clone()),
                                span: item.span.clone(),
                            });
                        }
                        sai_diagnostics::LintNameLookup::Unknown => {
                            output.errors.push(PackageValidationError {
                                kind: PackageValidationErrorKind::UnknownLint(
                                    item.value.code.clone(),
                                ),
                                span: item.span.clone(),
                            });
                        }
                    }
                }
            }
            PackageDeclaration::Profile { name, items } if parsed.role() == PackageRole::Root => {
                if !matches!(name.as_ref(), "dev" | "release" | "tiny") {
                    output.errors.push(PackageValidationError {
                        kind: PackageValidationErrorKind::UnknownProfile(name.clone()),
                        span: declaration.span.clone(),
                    });
                }
                for item in items {
                    match &item.value {
                        ProfileItem::Allow(capabilities) => {
                            validate_capability_items(capabilities, &mut output);
                        }
                        ProfileItem::Budget(budgets) => {
                            for budget in budgets {
                                if budget.value.metric.as_ref() != "wasm" {
                                    output.errors.push(PackageValidationError {
                                        kind: PackageValidationErrorKind::UnknownBudgetMetric(
                                            budget.value.metric.clone(),
                                        ),
                                        span: budget.span.clone(),
                                    });
                                }
                                if !valid_size(&budget.value.amount, &budget.value.unit) {
                                    output.errors.push(PackageValidationError {
                                        kind: PackageValidationErrorKind::InvalidSize,
                                        span: budget.span.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            PackageDeclaration::Allocator(identity) if parsed.role() == PackageRole::Root => {
                if !crate::identity::is_valid_identity_spelling(identity) {
                    output.errors.push(PackageValidationError {
                        kind: PackageValidationErrorKind::InvalidPackageName(identity.clone()),
                        span: declaration.span.clone(),
                    });
                }
                if is_implicit_package(identity) {
                    output.errors.push(PackageValidationError {
                        kind: PackageValidationErrorKind::ImplicitPackageRequired(identity.clone()),
                        span: declaration.span.clone(),
                    });
                }
                if complete && !ordinary_requirements.contains(identity.as_ref()) {
                    output.errors.push(PackageValidationError {
                        kind: PackageValidationErrorKind::AllocatorNotRequired(identity.clone()),
                        span: declaration.span.clone(),
                    });
                }
            }
            PackageDeclaration::Overrides(items) if parsed.role() == PackageRole::Root => {
                for item in items {
                    if !crate::identity::is_valid_identity_spelling(&item.value.package) {
                        output.errors.push(PackageValidationError {
                            kind: PackageValidationErrorKind::InvalidPackageName(
                                item.value.package.clone(),
                            ),
                            span: item.span.clone(),
                        });
                    }
                    let kind = match item.value.source {
                        OverrideSource::Artifact => OverrideKind::Artifact,
                        OverrideSource::Path(_) | OverrideSource::Git { .. } => {
                            OverrideKind::Source
                        }
                    };
                    if is_implicit_package(&item.value.package) {
                        output.errors.push(PackageValidationError {
                            kind: PackageValidationErrorKind::ImplicitPackageRequired(
                                item.value.package.clone(),
                            ),
                            span: item.span.clone(),
                        });
                    }
                    if overrides
                        .insert((item.value.package.as_ref(), kind), item.span.clone())
                        .is_some()
                    {
                        output.errors.push(PackageValidationError {
                            kind: PackageValidationErrorKind::DuplicateOverride {
                                package: item.value.package.clone(),
                                kind,
                            },
                            span: item.span.clone(),
                        });
                    }
                }
            }
            PackageDeclaration::ArtifactComponents(entries) => {
                has_artifact = true;
                for entry in entries {
                    let target = entry.value.target.as_ref();
                    if !crate::manifest::is_target_name(target) {
                        output.errors.push(PackageValidationError {
                            kind: PackageValidationErrorKind::UnknownTarget(
                                entry.value.target.clone(),
                            ),
                            span: entry.span.clone(),
                        });
                    } else {
                        artifact_targets.insert(target);
                        if !is_component_target(target) {
                            output.errors.push(PackageValidationError {
                                kind: PackageValidationErrorKind::ArtifactOnCoreTarget(
                                    entry.value.target.clone(),
                                ),
                                span: entry.span.clone(),
                            });
                        }
                    }
                }
            }
            PackageDeclaration::Description(description)
                if description.len() > 1024
                    || description.contains('\n')
                    || description.contains('\r') =>
            {
                output.errors.push(PackageValidationError {
                    kind: PackageValidationErrorKind::InvalidDescription,
                    span: declaration.span.clone(),
                });
            }
            PackageDeclaration::License(expression)
                if spdx::Expression::parse(expression).is_err() =>
            {
                output.warnings.push(PackageValidationWarning {
                    kind: PackageValidationWarningKind::InvalidLicenseExpression,
                    span: declaration.span.clone(),
                });
            }
            PackageDeclaration::Description(_)
            | PackageDeclaration::License(_)
            | PackageDeclaration::Lints(_)
            | PackageDeclaration::Profile { .. }
            | PackageDeclaration::Allocator(_)
            | PackageDeclaration::Overrides(_)
            | PackageDeclaration::Repository(_)
            | PackageDeclaration::Authors(_) => {}
        }
    }

    if complete && has_artifact && !has_source {
        for declaration in &file.declarations {
            let PackageDeclaration::ExportWorld(world) = &declaration.value else {
                continue;
            };
            if !world.contains(':') && world.contains('/') && is_valid_export_world(world) {
                output.errors.push(PackageValidationError {
                    kind: PackageValidationErrorKind::InvalidWorldName(world.clone()),
                    span: declaration.span.clone(),
                });
            }
        }
    }

    for denied in &package_deny {
        if package_allow
            .iter()
            .any(|allowed| allowed.value == denied.value)
        {
            output.errors.push(PackageValidationError {
                kind: PackageValidationErrorKind::CapabilityAllowDenyConflict(denied.value.clone()),
                span: denied.span.clone(),
            });
        }
    }

    if complete && parsed.role() == PackageRole::Root {
        for declaration in &file.declarations {
            let PackageDeclaration::Profile { items, .. } = &declaration.value else {
                continue;
            };
            for item in items {
                let ProfileItem::Allow(capabilities) = &item.value else {
                    continue;
                };
                for capability in capabilities {
                    if is_valid_capability(&capability.value)
                        && !package_allow
                            .iter()
                            .any(|allowed| capability_covers(&allowed.value, &capability.value))
                    {
                        output.errors.push(PackageValidationError {
                            kind: PackageValidationErrorKind::ProfileAllowExceedsPackage(
                                capability.value.clone(),
                            ),
                            span: capability.span.clone(),
                        });
                    }
                }
            }
        }
    }

    if complete {
        if targets.is_none_or(|items| items.is_empty()) {
            output.errors.push(PackageValidationError {
                kind: PackageValidationErrorKind::MissingTargets,
                span: file
                    .header
                    .as_ref()
                    .map_or(0..0, |header| header.span.end..header.span.end),
            });
        }
        if !has_source && !has_artifact {
            output.errors.push(PackageValidationError {
                kind: PackageValidationErrorKind::PackageHasNoContent,
                span: file
                    .header
                    .as_ref()
                    .map_or(0..0, |header| header.span.end..header.span.end),
            });
        }
        if has_artifact {
            let artifact_span = file
                .declarations
                .iter()
                .find(|declaration| {
                    matches!(declaration.value, PackageDeclaration::ArtifactComponents(_))
                })
                .map_or(0..0, |declaration| declaration.span.clone());
            if !has_wit {
                output.errors.push(PackageValidationError {
                    kind: PackageValidationErrorKind::WitRequired,
                    span: artifact_span.clone(),
                });
            }
            if !has_export_world {
                output.errors.push(PackageValidationError {
                    kind: PackageValidationErrorKind::MissingExportWorld,
                    span: artifact_span,
                });
            }
        }
        if let Some(target_items) = targets {
            for declaration in &file.declarations {
                if let PackageDeclaration::Requires { test, entries } = &declaration.value {
                    if *test && parsed.role() == PackageRole::Dependency {
                        continue;
                    }
                    for entry in entries {
                        if let Some(only) = &entry.value.only {
                            for target in &only.value {
                                if crate::manifest::is_target_name(&target.value)
                                    && !declared_targets.contains(target.value.as_ref())
                                {
                                    output.errors.push(PackageValidationError {
                                        kind: PackageValidationErrorKind::OnlyTargetNotDeclared(
                                            target.value.clone(),
                                        ),
                                        span: target.span.clone(),
                                    });
                                }
                            }
                        }
                    }
                }
            }
            for declaration in &file.declarations {
                if let PackageDeclaration::ArtifactComponents(entries) = &declaration.value {
                    for entry in entries {
                        let target = entry.value.target.as_ref();
                        if crate::manifest::is_target_name(target)
                            && !declared_targets.contains(target)
                        {
                            output.errors.push(PackageValidationError {
                                kind: PackageValidationErrorKind::ArtifactTargetNotDeclared(
                                    entry.value.target.clone(),
                                ),
                                span: entry.span.clone(),
                            });
                        }
                    }
                }
            }
            if has_artifact && !has_source {
                for target in target_items {
                    if crate::manifest::is_target_name(&target.value)
                        && !artifact_targets.contains(target.value.as_ref())
                    {
                        output.errors.push(PackageValidationError {
                            kind: PackageValidationErrorKind::MissingArtifactForTarget(
                                target.value.clone(),
                            ),
                            span: target.span.clone(),
                        });
                    }
                }
            }
        }
    }

    output
}

fn validate_requirement(
    entry: &Spanned<Requirement>,
    package_identity: Option<&str>,
    seen: &mut HashSet<String>,
    output: &mut PackageValidationOutput,
) {
    let package = entry.value.package.as_ref();
    if !crate::identity::is_valid_identity_spelling(package) {
        output.errors.push(PackageValidationError {
            kind: PackageValidationErrorKind::InvalidPackageName(entry.value.package.clone()),
            span: entry.span.clone(),
        });
    }
    if is_implicit_package(package) {
        output.errors.push(PackageValidationError {
            kind: PackageValidationErrorKind::ImplicitPackageRequired(entry.value.package.clone()),
            span: entry.span.clone(),
        });
    }
    if package_identity == Some(package) {
        output.errors.push(PackageValidationError {
            kind: PackageValidationErrorKind::PackageRequiresItself(entry.value.package.clone()),
            span: entry.span.clone(),
        });
    }
    if !seen.insert(package.to_owned()) {
        output.errors.push(PackageValidationError {
            kind: PackageValidationErrorKind::DuplicateRequirement(entry.value.package.clone()),
            span: entry.span.clone(),
        });
    }
    if entry.value.source.is_none() {
        output.errors.push(PackageValidationError {
            kind: PackageValidationErrorKind::MissingSource(entry.value.package.clone()),
            span: entry.span.clone(),
        });
    }
    if let Some(only) = &entry.value.only {
        for target in &only.value {
            if !crate::manifest::is_target_name(&target.value) {
                output.errors.push(PackageValidationError {
                    kind: PackageValidationErrorKind::UnknownTarget(target.value.clone()),
                    span: target.span.clone(),
                });
            }
        }
    }
    if let Some(grant) = &entry.value.grant {
        validate_capability_items(&grant.value, output);
    }
    if requirement_spans_lines(&entry.value.version) {
        output.warnings.push(PackageValidationWarning {
            kind: PackageValidationWarningKind::VersionRequirementSpansLines,
            span: entry.span.clone(),
        });
    }
}

fn validate_capability_items(items: &[Spanned<Box<str>>], output: &mut PackageValidationOutput) {
    for item in items {
        if !is_valid_capability(&item.value) {
            output.errors.push(PackageValidationError {
                kind: PackageValidationErrorKind::InvalidCapabilityName(item.value.clone()),
                span: item.span.clone(),
            });
        }
    }
}

pub(crate) fn is_valid_capability(value: &str) -> bool {
    if matches!(
        value,
        "alloc" | "threads" | "host" | "native_ffi" | "process" | "memory64"
    ) {
        return true;
    }
    let Some((package, interface)) = value.split_once('/') else {
        return is_capability_package(value) && value.bytes().all(|byte| byte.is_ascii());
    };
    is_capability_package(package)
        && !interface.is_empty()
        && !interface.contains('/')
        && !interface.contains(':')
        && interface.bytes().all(|byte| byte.is_ascii())
}

fn is_capability_package(value: &str) -> bool {
    crate::identity::is_valid_identity_spelling(value)
}

pub(crate) fn capability_covers(grant: &str, required: &str) -> bool {
    if grant == required {
        return true;
    }
    required
        .strip_prefix(grant)
        .is_some_and(|suffix| suffix.starts_with('/'))
        && !grant.contains('/')
}

fn validate_policy_item(item: &Spanned<Box<str>>, output: &mut PackageValidationOutput) {
    const POLICIES: &[&str] = &[
        "unsafe",
        "ambient_imports",
        "runtime_contracts",
        "duplicate_lines",
    ];
    if POLICIES.contains(&item.value.as_ref()) {
        return;
    }
    if is_valid_capability(&item.value) {
        output.errors.push(PackageValidationError {
            kind: PackageValidationErrorKind::PolicyItemIsCapability(item.value.clone()),
            span: item.span.clone(),
        });
    } else {
        output.errors.push(PackageValidationError {
            kind: PackageValidationErrorKind::UnknownPolicyItem(item.value.clone()),
            span: item.span.clone(),
        });
    }
}

fn is_implicit_package(package: &str) -> bool {
    matches!(package, "sai:core" | "sai:alloc")
}

pub(crate) fn is_component_target(target: &str) -> bool {
    matches!(target, "wasm32-wasi" | "wasm64-wasi")
}

fn is_valid_export_world(world: &str) -> bool {
    if world.contains(':') {
        let Some((package, interface)) = world.split_once('/') else {
            return false;
        };
        return is_capability_package(package)
            && !interface.is_empty()
            && !interface.contains('/')
            && !interface.contains(':');
    }
    world.split('/').all(|segment| {
        let mut bytes = segment.bytes();
        bytes.next().is_some_and(|first| first.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
    })
}

fn valid_size(amount: &str, unit: &str) -> bool {
    let multiplier = match unit {
        "B" => 1_u64,
        "KiB" => 1024,
        "MiB" => 1024 * 1024,
        _ => return false,
    };
    amount
        .parse::<u64>()
        .ok()
        .filter(|amount| *amount > 0)
        .and_then(|amount| amount.checked_mul(multiplier))
        .is_some()
}

fn requirement_spans_lines(requirement: &crate::version_req::VersionReq) -> bool {
    let minimum = requirement.minimum();
    let next_line = if minimum.major() >= 1 {
        minimum
            .major()
            .checked_add(1)
            .map(|major| format!("{major}.0.0"))
    } else if minimum.minor() >= 1 {
        minimum.minor().checked_add(1).map_or_else(
            || Some("1.0.0".to_owned()),
            |minor| Some(format!("0.{minor}.0")),
        )
    } else {
        minimum.patch().checked_add(1).map_or_else(
            || Some("0.1.0".to_owned()),
            |patch| Some(format!("0.0.{patch}")),
        )
    };
    next_line
        .and_then(|version| Version::parse(&version).ok())
        .is_some_and(|version| requirement.matches(&version))
}

#[cfg(test)]
mod tests {
    use crate::manifest::{
        PackageDeclaration, PackageParseOutput, PackageRole, parse, parse_for_role,
    };
    use crate::version::Version;

    use super::{
        PackageLintPolicyError, PackageValidationContext, PackageValidationErrorKind,
        PackageValidationWarningKind, resolve_lint_policies, validate,
    };

    fn parse_text(source: &str) -> PackageParseOutput {
        parse(source.as_bytes())
    }

    fn kinds(source: &str) -> Vec<PackageValidationErrorKind> {
        validate(&parse_text(source), PackageValidationContext::default())
            .errors
            .into_iter()
            .map(|error| error.kind)
            .collect()
    }

    #[test]
    fn resolves_root_lint_policies_with_manifest_locations() {
        let source = "package acme:app @ 1.0.0\nsource \"src\"\ntargets { wasm32-core }\n\
             policy { deny duplicate_lines, deny ambient_imports, deny runtime_contracts }\n";
        let parsed = parse(source.as_bytes());
        assert!(validate(&parsed, PackageValidationContext::default()).is_valid());
        let source_ref = sai_diagnostics::SourceRef {
            path: "sai.package".into(),
            package: Some("acme:app".into()),
            snapshot: Some(1),
        };
        let file_id = sai_diagnostics::FileId::new(7);
        let policies =
            resolve_lint_policies(&parsed, source_ref.clone(), file_id).expect("valid policies");
        let duplicate_location = policies
            .deny_duplicate_package_lines
            .as_ref()
            .expect("root graph policy");
        let duplicate_start = source
            .find("deny duplicate_lines")
            .expect("policy item is in source");

        assert_eq!(duplicate_location.file, source_ref);
        assert_eq!(duplicate_location.span.file, file_id);
        assert_eq!(
            duplicate_location.span.range.start().to_usize(),
            duplicate_start
        );
        assert!(policies.deny_ambient_imports.is_some());
        assert!(policies.deny_runtime_contracts.is_some());

        let empty = sai_diagnostics::LintPolicies::default();
        let resolved = sai_diagnostics::resolve_lint_occurrence(
            "DUPLICATE_PACKAGE_LINE",
            &sai_diagnostics::LintResolutionContext {
                primary: None,
                attributes: &[],
                cli: None,
                manifest: None,
                package_policy: &empty,
                root_policy: &policies,
                is_root_or_workspace_member: false,
                deny_warnings: false,
            },
        )
        .expect("registered package lint");
        let sai_diagnostics::LintResolution::Emitted { severity, level } = resolved else {
            panic!("root policy must emit the graph-wide lint");
        };
        assert_eq!(severity, sai_diagnostics::Severity::Error);
        assert_eq!(level.source, sai_diagnostics::LevelSource::Policy);
        assert_eq!(level.location.as_ref(), Some(duplicate_location));
    }

    #[test]
    fn ignores_duplicate_line_policy_in_dependency_manifests() {
        let parsed = parse_for_role(
            b"package acme:dep @ 1.0.0\n\
              source \"src\"\n\
              targets { wasm32-core }\n\
              policy { deny duplicate_lines, deny ambient_imports }\n",
            PackageRole::Dependency,
        );
        assert!(validate(&parsed, PackageValidationContext::default()).is_valid());
        let policies = resolve_lint_policies(
            &parsed,
            sai_diagnostics::SourceRef {
                path: "deps/acme-dep/sai.package".into(),
                package: Some("acme:dep".into()),
                snapshot: Some(1),
            },
            sai_diagnostics::FileId::new(8),
        )
        .expect("valid dependency policy");

        assert!(policies.deny_duplicate_package_lines.is_none());
        assert!(policies.deny_ambient_imports.is_some());
    }

    #[test]
    fn rejects_lint_policy_extraction_from_incomplete_syntax() {
        let parsed = parse(b"package acme:broken @ 1.0.0\npolicy { deny duplicate_lines\n");

        assert_eq!(
            resolve_lint_policies(
                &parsed,
                sai_diagnostics::SourceRef {
                    path: "sai.package".into(),
                    package: Some("acme:broken".into()),
                    snapshot: Some(1),
                },
                sai_diagnostics::FileId::new(9),
            ),
            Err(PackageLintPolicyError::PackageSyntaxInvalid)
        );
    }

    #[test]
    fn requires_targets_and_package_content() {
        assert_eq!(
            kinds("package acme:empty @ 1.0.0\n"),
            vec![
                PackageValidationErrorKind::MissingTargets,
                PackageValidationErrorKind::PackageHasNoContent,
            ]
        );
        assert_eq!(
            kinds(
                "package acme:empty @ 1.0.0\n\
                 source \"src\"\n\
                 targets {}\n"
            ),
            vec![PackageValidationErrorKind::MissingTargets]
        );
    }

    #[test]
    fn accepts_every_target_name_from_the_contract() {
        let output = validate(
            &parse_text(
                "package acme:portable @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core, wasm64-core, wasm32-wasi, wasm64-wasi,\n\
                           x86_64-linux, aarch64-linux, aarch64-macos, x86_64-windows }\n",
            ),
            PackageValidationContext::default(),
        );
        assert!(output.errors.is_empty(), "{:?}", output.errors);
    }

    #[test]
    fn reports_unknown_targets_and_only_targets_missing_from_the_package() {
        let output = validate(
            &parse_text(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core, imaginary-target }\n\
                 requires {\n\
                     acme:dep @ ^1 from path \"../dep\"\n\
                         only { wasm32-wasi, imaginary-target }\n\
                 }\n",
            ),
            PackageValidationContext::default(),
        );
        assert_eq!(
            output
                .errors
                .iter()
                .map(|error| error.kind.clone())
                .collect::<Vec<_>>(),
            vec![
                PackageValidationErrorKind::UnknownTarget("imaginary-target".into()),
                PackageValidationErrorKind::UnknownTarget("imaginary-target".into()),
                PackageValidationErrorKind::OnlyTargetNotDeclared("wasm32-wasi".into()),
            ]
        );
    }

    #[test]
    fn artifact_and_hybrid_packages_obey_static_target_requirements() {
        let hybrid = validate(
            &parse_text(
                "package acme:hybrid @ 1.0.0\n\
                 source \"src\"\n\
                 wit \"wit\"\n\
                 export world api\n\
                 targets { wasm32-core, wasm32-wasi }\n\
                 artifact component { wasm32-wasi \"dist/app.wasm\" }\n",
            ),
            PackageValidationContext::default(),
        );
        assert!(hybrid.errors.is_empty(), "{:?}", hybrid.errors);

        assert_eq!(
            kinds(
                "package acme:artifact @ 1.0.0\n\
                 targets { wasm32-core, wasm32-wasi }\n\
                 artifact component {\n\
                     wasm32-wasi \"dist/app.wasm\"\n\
                     x86_64-linux \"dist/native.wasm\"\n\
                 }\n",
            ),
            vec![
                PackageValidationErrorKind::ArtifactOnCoreTarget("x86_64-linux".into()),
                PackageValidationErrorKind::WitRequired,
                PackageValidationErrorKind::MissingExportWorld,
                PackageValidationErrorKind::ArtifactTargetNotDeclared("x86_64-linux".into()),
                PackageValidationErrorKind::MissingArtifactForTarget("wasm32-core".into()),
            ]
        );
    }

    #[test]
    fn artifact_local_world_is_single_segment_while_hybrid_world_can_be_nested() {
        assert_eq!(
            kinds(
                "package acme:artifact @ 1.0.0\n\
                 wit \"wit\"\n\
                 export world api/client\n\
                 targets { wasm32-wasi }\n\
                 artifact component { wasm32-wasi \"dist/app.wasm\" }\n",
            ),
            vec![PackageValidationErrorKind::InvalidWorldName(
                "api/client".into()
            )]
        );

        assert!(
            kinds(
                "package acme:hybrid @ 1.0.0\n\
                 source \"src\"\n\
                 wit \"wit\"\n\
                 export world api/client\n\
                 targets { wasm32-wasi }\n\
                 artifact component { wasm32-wasi \"dist/app.wasm\" }\n",
            )
            .is_empty()
        );
    }

    #[test]
    fn rejects_artifacts_for_undeclared_targets_and_invalid_world_names() {
        assert_eq!(
            kinds(
                "package acme:artifact @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
                 wit \"wit\"\n\
                 export world wasi:http\n\
                 artifact component {\n\
                     wasm32-wasi \"dist/app.wasm\"\n\
                     wasm64-wasi \"dist/app64.wasm\"\n\
                 }\n",
            ),
            vec![
                PackageValidationErrorKind::InvalidWorldName("wasi:http".into()),
                PackageValidationErrorKind::ArtifactTargetNotDeclared("wasm64-wasi".into()),
            ]
        );
        assert_eq!(
            kinds(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 export world api-name\n",
            ),
            vec![PackageValidationErrorKind::InvalidWorldName(
                "api-name".into()
            )]
        );
    }

    #[test]
    fn validates_requirement_sources_identity_and_uniqueness() {
        assert_eq!(
            kinds(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 requires {\n\
                     acme:app @ ^1 from path \"../self\"\n\
                     sai:core @ ^1 from git \"https://example.test/core\"\n\
                     acme:dep @ ^1\n\
                 }\n\
                 requires test {\n\
                     acme:dep @ ^2 from path \"../dep\"\n\
                 }\n",
            ),
            vec![
                PackageValidationErrorKind::PackageRequiresItself("acme:app".into()),
                PackageValidationErrorKind::ImplicitPackageRequired("sai:core".into()),
                PackageValidationErrorKind::MissingSource("acme:dep".into()),
                PackageValidationErrorKind::DuplicateRequirement("acme:dep".into()),
            ]
        );
    }

    #[test]
    fn validates_identity_segment_limits_for_package_references() {
        let long_package = format!("{}:allocator", "a".repeat(65));
        let source = format!(
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets {{ wasm32-core }}\n\
             requires {{ {long_package} @ ^1 from path \"../long\" }}\n\
             allocator {long_package}\n\
             overrides {{\n\
                 {long_package} use artifact\n\
             }}\n"
        );
        let invalid_names = kinds(&source)
            .into_iter()
            .filter(|kind| matches!(kind, PackageValidationErrorKind::InvalidPackageName(_)))
            .count();
        assert_eq!(
            invalid_names,
            3,
            "{:#?}",
            validate(&parse_text(&source), PackageValidationContext::default())
        );
    }

    #[test]
    fn validates_capability_spelling_conflicts_and_profile_coverage() {
        assert_eq!(
            kinds(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 allow { alloc, acme:storage, invalid }\n\
                 deny { alloc, acme:storage/read }\n\
                 profile tiny { allow { acme:storage/write, host } }\n",
            ),
            vec![
                PackageValidationErrorKind::InvalidCapabilityName("invalid".into()),
                PackageValidationErrorKind::CapabilityAllowDenyConflict("alloc".into()),
                PackageValidationErrorKind::ProfileAllowExceedsPackage("host".into()),
            ]
        );
    }

    #[test]
    fn capability_names_must_have_at_most_one_interface_segment() {
        assert_eq!(
            kinds(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 allow { acme:storage/read/write }\n",
            ),
            vec![PackageValidationErrorKind::InvalidCapabilityName(
                "acme:storage/read/write".into()
            )]
        );
    }

    #[test]
    fn capability_and_external_world_package_segments_obey_identity_length_limits() {
        let long_namespace = "a".repeat(65);
        let long_name = "b".repeat(65);
        let source = format!(
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets {{ wasm32-core }}\n\
             allow {{ {long_namespace}:store, acme:{long_name} }}\n\
             export world acme:{long_name}/proxy\n"
        );
        assert_eq!(
            kinds(&source),
            vec![
                PackageValidationErrorKind::InvalidCapabilityName(
                    format!("{long_namespace}:store").into()
                ),
                PackageValidationErrorKind::InvalidCapabilityName(
                    format!("acme:{long_name}").into()
                ),
                PackageValidationErrorKind::InvalidWorldName(
                    format!("acme:{long_name}/proxy").into()
                ),
            ]
        );
    }

    #[test]
    fn validates_policy_and_budget_values() {
        assert_eq!(
            kinds(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 policy { deny unsafe, deny host, deny typo }\n\
                 profile dev {\n\
                     budget { wasm <= 1 KiB, wasm <= 0 B, wasm <= 18446744073709551615 MiB,\n\
                              unknown <= 2 GB }\n\
                 }\n",
            ),
            vec![
                PackageValidationErrorKind::PolicyItemIsCapability("host".into()),
                PackageValidationErrorKind::UnknownPolicyItem("typo".into()),
                PackageValidationErrorKind::InvalidSize,
                PackageValidationErrorKind::InvalidSize,
                PackageValidationErrorKind::UnknownBudgetMetric("unknown".into()),
                PackageValidationErrorKind::InvalidSize,
            ]
        );
    }

    #[test]
    fn lint_settings_use_the_diagnostic_registry() {
        assert_eq!(
            kinds(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 lints { allow NAMING_CONVENTION, warn UNKNOWN_LINT_NAME,\n\
                         deny MISSING_PACKAGE_DECL, deny INVALID_DESCRIPTION }\n",
            ),
            vec![
                PackageValidationErrorKind::UnknownLint("UNKNOWN_LINT_NAME".into()),
                PackageValidationErrorKind::NotALint("MISSING_PACKAGE_DECL".into()),
                PackageValidationErrorKind::NotALint("INVALID_DESCRIPTION".into()),
            ]
        );
    }

    #[test]
    fn root_only_validation_is_ignored_for_dependency_manifests() {
        let parsed = parse_for_role(
            b"package acme:dep @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core }\n\
             lints { allow UNKNOWN_LINT_NAME }\n\
             requires test { sai:alloc @ ^1 }\n\
             profile experimental { allow { host } budget { unknown <= 0 B } }\n\
             allocator acme:missing\n\
             overrides { sai:core use artifact }\n",
            PackageRole::Dependency,
        );
        let output = validate(&parsed, PackageValidationContext::default());
        assert!(output.errors.is_empty(), "{:?}", output.errors);
    }

    #[test]
    fn allocator_requires_a_production_dependency() {
        assert_eq!(
            kinds(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 requires test { acme:alloc @ ^1 from path \"../alloc\" }\n\
                 allocator acme:alloc\n",
            ),
            vec![PackageValidationErrorKind::AllocatorNotRequired(
                "acme:alloc".into()
            )]
        );
    }

    #[test]
    fn validates_override_kinds_independently() {
        assert_eq!(
            kinds(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 requires { acme:dep @ ^1 from path \"../dep\" }\n\
                 overrides {\n\
                     acme:dep use path \"../first\"\n\
                     acme:dep use git \"https://example.test/dep\"\n\
                     acme:dep use artifact\n\
                     acme:dep use artifact\n\
                 }\n",
            ),
            vec![
                PackageValidationErrorKind::DuplicateOverride {
                    package: "acme:dep".into(),
                    kind: super::OverrideKind::Source,
                },
                PackageValidationErrorKind::DuplicateOverride {
                    package: "acme:dep".into(),
                    kind: super::OverrideKind::Artifact,
                },
            ]
        );
    }

    #[test]
    fn duplicate_overrides_are_checked_despite_unrelated_syntax_errors() {
        let parsed = parse_text(
            "package acme:app @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core }\n\
             overrides {\n\
                 acme:dep use artifact\n\
                 acme:dep use artifact\n\
             }\n\
             ;\n",
        );
        assert!(!parsed.is_syntax_valid());
        let output = validate(&parsed, PackageValidationContext::default());
        assert!(output.errors.iter().any(|error| matches!(
            error.kind,
            PackageValidationErrorKind::DuplicateOverride { .. }
        )));
    }

    #[test]
    fn checks_compiler_requirement_when_toolchain_version_is_known() {
        let parsed = parse_text(
            "package acme:app @ 1.0.0\n\
             compiler ^0.1\n\
             source \"src\"\n\
             targets { wasm32-core }\n",
        );
        let current = Version::parse("0.2.0").expect("valid version");
        let output = validate(
            &parsed,
            PackageValidationContext {
                compiler_version: Some(&current),
            },
        );
        assert_eq!(output.errors.len(), 1);
        assert!(matches!(
            output.errors[0].kind,
            PackageValidationErrorKind::CompilerVersionMismatch { .. }
        ));
    }

    #[test]
    fn incomplete_syntax_does_not_cascade_missing_declaration_errors() {
        let parsed = parse_text(
            "package acme:app @ 1.0.0\n\
             targets {\n\
             source \"src\"\n",
        );
        assert!(!parsed.is_syntax_valid());
        let output = validate(&parsed, PackageValidationContext::default());
        assert!(
            output.errors.iter().all(|error| !matches!(
                error.kind,
                PackageValidationErrorKind::MissingTargets
                    | PackageValidationErrorKind::PackageHasNoContent
            )),
            "{:?}",
            output.errors
        );
    }

    #[test]
    fn root_only_declarations_remain_in_the_syntax_tree_for_dependencies() {
        let parsed = parse_for_role(
            b"package acme:dep @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core }\n\
             profile tiny { budget { wasm <= 1 B } }\n",
            PackageRole::Dependency,
        );
        assert!(
            parsed
                .file
                .declarations
                .iter()
                .any(|declaration| matches!(declaration.value, PackageDeclaration::Profile { .. }))
        );
        let output = validate(&parsed, PackageValidationContext::default());
        assert!(output.warnings.is_empty());
        assert!(output.errors.is_empty());
    }

    #[test]
    fn warns_when_dependency_requirement_admits_a_later_compatibility_line() {
        let output = validate(
            &parse_text(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 requires {\n\
                     acme:wide @ >=1.2 from path \"../wide\"\n\
                     acme:bounded @ >=1.2, <3.0 from path \"../bounded\"\n\
                     acme:caret @ ^1.2 from path \"../caret\"\n\
                     acme:tilde-zero @ ~0 from path \"../tilde\"\n\
                 }\n",
            ),
            PackageValidationContext::default(),
        );
        assert_eq!(
            output
                .warnings
                .iter()
                .map(|warning| warning.kind)
                .collect::<Vec<_>>(),
            vec![
                PackageValidationWarningKind::VersionRequirementSpansLines,
                PackageValidationWarningKind::VersionRequirementSpansLines,
                PackageValidationWarningKind::VersionRequirementSpansLines,
            ]
        );
        assert_eq!(
            output
                .warnings
                .iter()
                .map(|warning| warning.span.start)
                .collect::<Vec<_>>()
                .len(),
            3
        );
    }

    #[test]
    fn explicit_compatibility_line_end_does_not_warn_for_boundary_prereleases() {
        let output = validate(
            &parse_text(
                "package acme:app @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 requires {\n\
                     acme:bounded @ >=0.7.1, <0.8 from path \"../bounded\"\n\
                     acme:boundary @ >=1.2, <2.0.0 from path \"../boundary\"\n\
                 }\n",
            ),
            PackageValidationContext::default(),
        );
        assert!(output.warnings.is_empty(), "{:?}", output.warnings);
    }

    #[test]
    fn validates_description_length_and_spdx_license_expressions() {
        let source = format!(
            "package acme:metadata @ 1.0.0\n\
             source \"src\"\n\
             targets {{ wasm32-core }}\n\
             description \"{}\"\n\
             license \"MIT OR Apache-2.0\"\n",
            "é".repeat(513)
        );
        let output = validate(&parse_text(&source), PackageValidationContext::default());
        assert_eq!(
            output.errors[0].kind,
            PackageValidationErrorKind::InvalidDescription
        );
        assert!(output.warnings.is_empty(), "{:?}", output.warnings);

        let at_limit = format!(
            "package acme:metadata @ 1.0.0\n\
             source \"src\"\n\
             targets {{ wasm32-core }}\n\
             description \"{}\"\n\
             license \"MIT OR Apache-2.0\"\n",
            "é".repeat(512)
        );
        let output = validate(&parse_text(&at_limit), PackageValidationContext::default());
        assert!(output.errors.is_empty(), "{:?}", output.errors);
        assert!(output.warnings.is_empty(), "{:?}", output.warnings);

        let multiline = validate(
            &parse_text(
                "package acme:metadata @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 description \"two\\nlines\"\n",
            ),
            PackageValidationContext::default(),
        );
        assert_eq!(
            multiline.errors[0].kind,
            PackageValidationErrorKind::InvalidDescription
        );

        let valid = validate(
            &parse_text(
                "package acme:license @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 license \"LicenseRef-Internal-Use\"\n",
            ),
            PackageValidationContext::default(),
        );
        assert!(valid.warnings.is_empty(), "{:?}", valid.warnings);

        let invalid = validate(
            &parse_text(
                "package acme:license @ 1.0.0\n\
                 source \"src\"\n\
                 targets { wasm32-core }\n\
                 license \"MIT OR Not-A-Real-License\"\n",
            ),
            PackageValidationContext::default(),
        );
        assert_eq!(
            invalid.warnings[0].kind,
            PackageValidationWarningKind::InvalidLicenseExpression
        );
    }
}
