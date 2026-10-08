use crate::{
    LevelInfo, LevelSource, LintLevel, LintNameLookup, Location, Severity, lookup_lint_name,
};
use sai_vfs::{Span, TextRange};

/// A level that may be selected by an attribute, manifest, or command-line flag.
///
/// `info` is a valid default, but no configurable source may assign it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OverrideLintLevel {
    Allow,
    Warn,
    Deny,
}

impl OverrideLintLevel {
    const fn as_level(self) -> LintLevel {
        match self {
            Self::Allow => LintLevel::Allow,
            Self::Warn => LintLevel::Warn,
            Self::Deny => LintLevel::Deny,
        }
    }
}

/// One already-validated lint level from the root package manifest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ManifestLintLevel {
    level: OverrideLintLevel,
    location: Location,
}

impl ManifestLintLevel {
    /// Creates a manifest level with the location of its `lints` entry.
    pub fn new(level: OverrideLintLevel, location: Location) -> Self {
        Self { level, location }
    }

    pub const fn level(&self) -> OverrideLintLevel {
        self.level
    }

    pub fn location(&self) -> &Location {
        &self.location
    }
}

/// A validated lint attribute and the complete syntax range of its target.
///
/// Both spans must use the same file and the same byte-offset convention as
/// the diagnostic's primary span. In the compiler, convert decoded spans to
/// original-file offsets before constructing this value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ScopedLintLevel {
    code: String,
    level: OverrideLintLevel,
    syntax_range: Span,
    location: Location,
}

impl ScopedLintLevel {
    /// Creates an attribute level if its source location belongs to the target.
    pub fn new(
        code: impl Into<String>,
        level: OverrideLintLevel,
        syntax_range: Span,
        location: Location,
    ) -> Option<Self> {
        if syntax_range.file != location.span.file || !contains_range(syntax_range, location.span) {
            return None;
        }

        Some(Self {
            code: code.into(),
            level,
            syntax_range,
            location,
        })
    }

    pub fn code(&self) -> &str {
        &self.code
    }

    pub const fn level(&self) -> OverrideLintLevel {
        self.level
    }

    pub const fn syntax_range(&self) -> Span {
        self.syntax_range
    }

    pub fn location(&self) -> &Location {
        &self.location
    }
}

/// Lint-denying policy items for one package.
///
/// `deny_ambient_imports` and `deny_runtime_contracts` apply only to
/// occurrences in that package. `deny_duplicate_package_lines` is read only
/// from the root package's policy and applies to the complete build graph.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LintPolicies {
    pub deny_ambient_imports: Option<Location>,
    pub deny_runtime_contracts: Option<Location>,
    pub deny_duplicate_package_lines: Option<Location>,
}

/// Inputs needed to resolve one lint occurrence.
///
/// `attributes` are collected from the enclosing targets in source order.
/// Their complete syntax ranges determine applicability and the smallest
/// containing range wins. `package_policy` must describe the package that
/// owns the occurrence; `root_policy` must describe the root package even
/// when the occurrence belongs to a dependency. `is_root_or_workspace_member`
/// controls both the CLI/manifest sources and the dependency cap.
pub struct LintResolutionContext<'a> {
    pub primary: Option<&'a Location>,
    pub attributes: &'a [ScopedLintLevel],
    /// Command-line settings have no source location by definition.
    pub cli: Option<OverrideLintLevel>,
    pub manifest: Option<&'a ManifestLintLevel>,
    pub package_policy: &'a LintPolicies,
    pub root_policy: &'a LintPolicies,
    pub is_root_or_workspace_member: bool,
    pub deny_warnings: bool,
}

/// The result of resolving and post-processing a lint occurrence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LintResolution {
    /// The lint is disabled or suppressed by the dependency cap.
    Suppressed,
    /// The lint should be emitted with this severity and provenance.
    Emitted {
        severity: Severity,
        level: LevelInfo,
    },
}

/// Why a requested code could not be resolved as a lint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LintResolutionError {
    UnknownCode,
    NotALint { severity: Severity },
}

/// Resolves one lint, applies the dependency cap, then applies
/// `--deny-warnings`.
///
/// The resolver consumes prevalidated source settings. Parsing the package
/// manifest and CLI, deriving attribute target ranges, and selecting package
/// policy data belong to their respective frontend and driver phases.
pub fn resolve_lint_occurrence(
    code: &str,
    context: &LintResolutionContext<'_>,
) -> Result<LintResolution, LintResolutionError> {
    let registered = match lookup_lint_name(code) {
        LintNameLookup::Lint(entry) => entry,
        LintNameLookup::HardDiagnostic(severity) => {
            return Err(LintResolutionError::NotALint { severity });
        }
        LintNameLookup::Unknown => return Err(LintResolutionError::UnknownCode),
    };

    let policy_location = match code {
        "AMBIENT_IMPORT" => context.package_policy.deny_ambient_imports.as_ref(),
        "CONTRACT_RUNTIME_CHECK" => context.package_policy.deny_runtime_contracts.as_ref(),
        "DUPLICATE_PACKAGE_LINE" => context.root_policy.deny_duplicate_package_lines.as_ref(),
        _ => None,
    };

    let (value, source, location) = if let Some(location) = policy_location {
        (LintLevel::Deny, LevelSource::Policy, Some(location.clone()))
    } else if let Some(attribute) = innermost_attribute(code, context) {
        (
            attribute.level.as_level(),
            LevelSource::Attribute,
            Some(attribute.location.clone()),
        )
    } else if context.is_root_or_workspace_member {
        if let Some(cli) = context.cli {
            (cli.as_level(), LevelSource::Cli, None)
        } else if let Some(manifest) = context.manifest {
            (
                manifest.level.as_level(),
                LevelSource::Manifest,
                Some(manifest.location.clone()),
            )
        } else {
            (registered.default_level, LevelSource::Default, None)
        }
    } else {
        (registered.default_level, LevelSource::Default, None)
    };

    if value == LintLevel::Allow
        || (!context.is_root_or_workspace_member && source != LevelSource::Policy)
    {
        return Ok(LintResolution::Suppressed);
    }

    let mut level = LevelInfo {
        value,
        source,
        location,
    };
    let base_severity = severity_for_level(value);
    let severity = apply_deny_warnings(base_severity, context.deny_warnings);
    if severity != base_severity {
        // `value` remains the resolved lint level (`warn`); the provenance
        // records that the command-line post-processing raised its severity.
        level.source = LevelSource::DenyWarnings;
        level.location = None;
    }

    Ok(LintResolution::Emitted { severity, level })
}

/// Turns any remaining hard warning into an error when `--deny-warnings` is
/// active. Hard diagnostics retain `level: None`.
pub const fn apply_deny_warnings(severity: Severity, enabled: bool) -> Severity {
    if enabled && matches!(severity, Severity::Warning) {
        Severity::Error
    } else {
        severity
    }
}

fn innermost_attribute<'a>(
    code: &str,
    context: &'a LintResolutionContext<'_>,
) -> Option<&'a ScopedLintLevel> {
    let primary = context.primary?;
    let mut selected = None;

    for attribute in context.attributes {
        if attribute.code != code || !contains_range(attribute.syntax_range, primary.span) {
            continue;
        }

        if selected.is_none_or(|current: &ScopedLintLevel| {
            attribute.syntax_range.range.len() <= current.syntax_range.range.len()
        }) {
            selected = Some(attribute);
        }
    }

    selected
}

fn contains_range(outer: Span, inner: Span) -> bool {
    if outer.file != inner.file {
        return false;
    }

    let outer_range: TextRange = outer.range;
    let inner_range: TextRange = inner.range;
    outer_range.start() <= inner_range.start()
        && inner_range.end() <= outer_range.end()
        && (!inner_range.is_empty() || outer_range.contains(inner_range.start()))
}

const fn severity_for_level(level: LintLevel) -> Severity {
    match level {
        LintLevel::Allow => Severity::Info,
        LintLevel::Info => Severity::Info,
        LintLevel::Warn => Severity::Warning,
        LintLevel::Deny => Severity::Error,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FileId, SourceRef, TextSize};

    fn span(start: u32, end: u32) -> Span {
        Span::new(
            FileId::new(1),
            TextRange::new(TextSize::new(start), TextSize::new(end)).expect("ordered range"),
        )
    }

    fn location(start: u32, end: u32) -> Location {
        location_in(1, "src/lib.sai", start, end)
    }

    fn location_in(file_id: u32, path: &str, start: u32, end: u32) -> Location {
        Location {
            file: SourceRef {
                path: path.to_owned(),
                package: Some("root".to_owned()),
                snapshot: None,
            },
            span: Span::new(
                FileId::new(file_id),
                TextRange::new(TextSize::new(start), TextSize::new(end)).expect("ordered range"),
            ),
        }
    }

    fn attribute(
        level: OverrideLintLevel,
        scope: (u32, u32),
        attribute_span: (u32, u32),
    ) -> ScopedLintLevel {
        ScopedLintLevel::new(
            "UNUSED_VARIABLE",
            level,
            span(scope.0, scope.1),
            location(attribute_span.0, attribute_span.1),
        )
        .expect("attribute belongs to its syntax scope")
    }

    fn empty_policies() -> LintPolicies {
        LintPolicies::default()
    }

    #[allow(clippy::too_many_arguments)]
    fn context<'a>(
        primary: Option<&'a Location>,
        attributes: &'a [ScopedLintLevel],
        cli: Option<OverrideLintLevel>,
        manifest: Option<&'a ManifestLintLevel>,
        package_policy: &'a LintPolicies,
        root_policy: &'a LintPolicies,
        is_root_or_workspace_member: bool,
        deny_warnings: bool,
    ) -> LintResolutionContext<'a> {
        LintResolutionContext {
            primary,
            attributes,
            cli,
            manifest,
            package_policy,
            root_policy,
            is_root_or_workspace_member,
            deny_warnings,
        }
    }

    fn emitted(
        result: Result<LintResolution, LintResolutionError>,
        expected_severity: Severity,
        expected_level: LintLevel,
        expected_source: LevelSource,
    ) -> LevelInfo {
        match result.expect("registered lint should resolve") {
            LintResolution::Emitted { severity, level } => {
                assert_eq!(severity, expected_severity);
                assert_eq!(level.value, expected_level);
                assert_eq!(level.source, expected_source);
                level
            }
            LintResolution::Suppressed => panic!("lint should be emitted"),
        }
    }

    #[test]
    fn level_sources_follow_policy_attribute_cli_manifest_default_precedence() {
        let primary = location(30, 31);
        let outer = attribute(OverrideLintLevel::Warn, (0, 100), (1, 2));
        let inner = attribute(OverrideLintLevel::Deny, (20, 50), (21, 22));
        let attributes = [outer, inner];
        let cli = OverrideLintLevel::Warn;
        let manifest = ManifestLintLevel::new(OverrideLintLevel::Deny, location(5, 10));
        let empty = empty_policies();

        let resolved = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&primary),
                &attributes,
                Some(cli),
                Some(&manifest),
                &empty,
                &empty,
                true,
                false,
            ),
        )
        .expect("registered lint");
        let level = emitted(
            Ok(resolved),
            Severity::Error,
            LintLevel::Deny,
            LevelSource::Attribute,
        );
        assert_eq!(level.location, Some(location(21, 22)));

        let no_attributes: [ScopedLintLevel; 0] = [];
        let allow_cli = OverrideLintLevel::Allow;
        let resolved = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&primary),
                &no_attributes,
                Some(allow_cli),
                Some(&manifest),
                &empty,
                &empty,
                true,
                false,
            ),
        )
        .expect("registered lint");
        assert_eq!(resolved, LintResolution::Suppressed);

        let resolved = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&primary),
                &no_attributes,
                Some(cli),
                Some(&manifest),
                &empty,
                &empty,
                true,
                false,
            ),
        )
        .expect("registered lint");
        let level = emitted(
            Ok(resolved),
            Severity::Warning,
            LintLevel::Warn,
            LevelSource::Cli,
        );
        assert_eq!(level.location, None);

        let resolved = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&primary),
                &no_attributes,
                None,
                Some(&manifest),
                &empty,
                &empty,
                true,
                false,
            ),
        )
        .expect("registered lint");
        let level = emitted(
            Ok(resolved),
            Severity::Error,
            LintLevel::Deny,
            LevelSource::Manifest,
        );
        assert_eq!(level.location, Some(location(5, 10)));

        let resolved = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&primary),
                &no_attributes,
                None,
                None,
                &empty,
                &empty,
                true,
                false,
            ),
        )
        .expect("registered lint");
        let level = emitted(
            Ok(resolved),
            Severity::Warning,
            LintLevel::Warn,
            LevelSource::Default,
        );
        assert_eq!(level.location, None);
    }

    #[test]
    fn attribute_scope_uses_primary_span_file_and_innermost_target() {
        let primary = location(30, 31);
        let outer = attribute(OverrideLintLevel::Warn, (0, 100), (1, 2));
        let inner = attribute(OverrideLintLevel::Allow, (20, 50), (21, 22));
        let unrelated = attribute(OverrideLintLevel::Deny, (40, 80), (41, 42));
        let attributes = [outer, unrelated, inner];
        let empty = empty_policies();

        let result = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&primary),
                &attributes,
                None,
                None,
                &empty,
                &empty,
                true,
                false,
            ),
        )
        .expect("registered lint");
        assert_eq!(result, LintResolution::Suppressed);

        let outside = location(101, 102);
        let result = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&outside),
                &attributes,
                None,
                None,
                &empty,
                &empty,
                true,
                false,
            ),
        )
        .expect("registered lint");
        assert!(matches!(
            result,
            LintResolution::Emitted {
                level: LevelInfo {
                    source: LevelSource::Default,
                    ..
                },
                ..
            }
        ));

        let other_file = location_in(2, "src/other.sai", 30, 31);
        let result = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&other_file),
                &attributes,
                None,
                None,
                &empty,
                &empty,
                true,
                false,
            ),
        )
        .expect("registered lint");
        assert!(matches!(
            result,
            LintResolution::Emitted {
                level: LevelInfo {
                    source: LevelSource::Default,
                    ..
                },
                ..
            }
        ));

        assert!(
            ScopedLintLevel::new(
                "UNUSED_VARIABLE",
                OverrideLintLevel::Warn,
                span(0, 10),
                location(20, 21),
            )
            .is_none()
        );
    }

    #[test]
    fn package_policy_overrides_all_other_sources_and_dependency_cap() {
        let primary = location(30, 31);
        let attribute = attribute(OverrideLintLevel::Allow, (0, 100), (1, 2));
        let attributes = [attribute];
        let cli = OverrideLintLevel::Deny;
        let manifest = ManifestLintLevel::new(OverrideLintLevel::Warn, location(5, 10));
        let package_policy = LintPolicies {
            deny_runtime_contracts: Some(location(3, 4)),
            ..LintPolicies::default()
        };
        let empty = empty_policies();
        let resolved = resolve_lint_occurrence(
            "CONTRACT_RUNTIME_CHECK",
            &context(
                Some(&primary),
                &attributes,
                Some(cli),
                Some(&manifest),
                &package_policy,
                &empty,
                false,
                false,
            ),
        )
        .expect("registered lint");
        let level = emitted(
            Ok(resolved),
            Severity::Error,
            LintLevel::Deny,
            LevelSource::Policy,
        );
        assert_eq!(level.location, Some(location(3, 4)));

        let root_policy = LintPolicies {
            deny_duplicate_package_lines: Some(location(7, 8)),
            ..LintPolicies::default()
        };
        let no_attributes: [ScopedLintLevel; 0] = [];
        let resolved = resolve_lint_occurrence(
            "DUPLICATE_PACKAGE_LINE",
            &context(
                Some(&primary),
                &no_attributes,
                None,
                None,
                &empty,
                &root_policy,
                false,
                false,
            ),
        )
        .expect("registered lint");
        let level = emitted(
            Ok(resolved),
            Severity::Error,
            LintLevel::Deny,
            LevelSource::Policy,
        );
        assert_eq!(level.location, Some(location(7, 8)));
    }

    #[test]
    fn dependency_cap_suppresses_non_policy_levels_before_deny_warnings() {
        let primary = location(30, 31);
        let no_attributes: [ScopedLintLevel; 0] = [];
        let cli = OverrideLintLevel::Deny;
        let empty = empty_policies();

        let result = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&primary),
                &no_attributes,
                Some(cli),
                None,
                &empty,
                &empty,
                false,
                true,
            ),
        )
        .expect("registered lint");
        assert_eq!(result, LintResolution::Suppressed);

        let result = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&primary),
                &no_attributes,
                None,
                None,
                &empty,
                &empty,
                false,
                true,
            ),
        )
        .expect("registered lint");
        assert_eq!(result, LintResolution::Suppressed);
    }

    #[test]
    fn deny_warnings_changes_severity_and_records_post_processing_source() {
        let primary = location(30, 31);
        let no_attributes: [ScopedLintLevel; 0] = [];
        let cli = OverrideLintLevel::Warn;
        let empty = empty_policies();
        let result = resolve_lint_occurrence(
            "UNUSED_VARIABLE",
            &context(
                Some(&primary),
                &no_attributes,
                Some(cli),
                None,
                &empty,
                &empty,
                true,
                true,
            ),
        )
        .expect("registered lint");
        let level = emitted(
            Ok(result),
            Severity::Error,
            LintLevel::Warn,
            LevelSource::DenyWarnings,
        );
        assert_eq!(level.location, None);

        assert_eq!(
            apply_deny_warnings(Severity::Warning, true),
            Severity::Error
        );
        assert_eq!(apply_deny_warnings(Severity::Info, true), Severity::Info);
        assert_eq!(apply_deny_warnings(Severity::Error, true), Severity::Error);
        assert_eq!(
            apply_deny_warnings(Severity::Warning, false),
            Severity::Warning
        );
    }

    #[test]
    fn info_default_and_hard_diagnostic_classification_are_preserved() {
        let primary = location(30, 31);
        let no_attributes: [ScopedLintLevel; 0] = [];
        let empty = empty_policies();
        let result = resolve_lint_occurrence(
            "CONTRACT_RUNTIME_CHECK",
            &context(
                Some(&primary),
                &no_attributes,
                None,
                None,
                &empty,
                &empty,
                true,
                true,
            ),
        )
        .expect("registered lint");
        let level = emitted(
            Ok(result),
            Severity::Info,
            LintLevel::Info,
            LevelSource::Default,
        );
        assert_eq!(level.location, None);

        assert_eq!(
            resolve_lint_occurrence(
                "UNKNOWN_FIELD",
                &context(
                    Some(&primary),
                    &no_attributes,
                    None,
                    None,
                    &empty,
                    &empty,
                    true,
                    false,
                ),
            ),
            Err(LintResolutionError::NotALint {
                severity: Severity::Error
            })
        );
        assert_eq!(
            resolve_lint_occurrence(
                "NOT_IN_REGISTRY",
                &context(
                    Some(&primary),
                    &no_attributes,
                    None,
                    None,
                    &empty,
                    &empty,
                    true,
                    false,
                ),
            ),
            Err(LintResolutionError::UnknownCode)
        );
    }
}
