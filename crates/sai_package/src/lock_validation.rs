//! Cross-field validation for parsed `sai.lock` files.

use std::collections::{HashMap, HashSet};
use std::net::Ipv6Addr;
use std::ops::Range;

use crate::identity::PackageIdentity;
use crate::lock::{
    LockPackage, LockPackageField, LockParseOutput, LockRoot, LockRootField, LockSource,
};
use crate::manifest::Spanned;
use crate::package_path::{ContentPackagePath, DependencyPackagePath};
use crate::version::CompatibilityLine;

pub const SUPPORTED_LOCK_FORMAT_VERSION: u64 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LockValidationError {
    pub kind: LockValidationErrorKind,
    pub span: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockValidationErrorKind {
    UnsupportedFormatVersion(u64),
    Corrupt(LockCorruptionKind),
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LockField {
    Targets,
    Source,
    Directory,
    Commit,
    Checksum,
    WitChecksum,
    Compiler,
    Capabilities,
    Artifact,
    Imports,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LockCorruptionKind {
    FormatVersionZero,
    MissingRootBlock,
    InvalidPackageIdentity(Box<str>),
    DuplicateEntry {
        package: Box<str>,
        line: CompatibilityLine,
    },
    DuplicateArtifactTarget(Box<str>),
    EmptyTargets,
    MissingField(LockField),
    DuplicateField(LockField),
    FieldNotAllowed(LockField),
    OnlyTargetNotDeclared(Box<str>),
    EmptyOnlyClause,
    ArtifactOnNonComponentTarget(Box<str>),
    InvalidSourceUrl,
    NonCanonicalSourceUrl,
    InvalidSourcePath,
    InvalidDirectory,
    InvalidCommit,
    InvalidChecksum,
    InvalidString,
    UnknownTarget(Box<str>),
    InvalidCapability(Box<str>),
    DuplicateRequirement(Box<str>),
    SelfRequirement(Box<str>),
    ArtifactImportTargetsDiffer,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LockValidationOutput {
    pub errors: Vec<LockValidationError>,
}

impl LockValidationOutput {
    /// Reports whether semantic validation found no errors. This does not
    /// include lexical or syntax diagnostics from [`LockParseOutput`].
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }
}

/// Validates lock fields and relationships that do not require package
/// contents, the current root contract, or a resolved dependency graph.
///
/// Incomplete syntax trees are left to the parser's diagnostics so validation
/// does not report missing fields based on parser recovery.
pub fn validate(parsed: &LockParseOutput) -> LockValidationOutput {
    let mut validator = Validator::default();
    let file = &parsed.file;
    let format_version = file.format_version.unwrap_or(0);
    let format_span = file.format_version_span.clone().unwrap_or(0..0);
    if format_version > SUPPORTED_LOCK_FORMAT_VERSION {
        validator.error(
            LockValidationErrorKind::UnsupportedFormatVersion(format_version),
            format_span,
        );
        return validator.finish();
    }
    if !parsed.is_syntax_valid() {
        return LockValidationOutput::default();
    }
    if format_version == 0 {
        validator.corrupt(LockCorruptionKind::FormatVersionZero, format_span);
    }

    let Some(root) = &file.root else {
        validator.corrupt(LockCorruptionKind::MissingRootBlock, 0..0);
        return validator.finish();
    };
    validator.validate_identity(&root.value.package, root.span.clone());
    validator.validate_root(&root.value, root.span.clone());

    let mut entries = HashSet::<(String, CompatibilityLine)>::new();
    for package in &file.packages {
        let identity_valid =
            validator.validate_identity(&package.value.package, package.span.clone());
        if identity_valid {
            let key = (
                package.value.package.to_string(),
                package.value.version.compatibility_line(),
            );
            if !entries.insert(key.clone()) {
                validator.corrupt(
                    LockCorruptionKind::DuplicateEntry {
                        package: key.0.into(),
                        line: key.1,
                    },
                    package.span.clone(),
                );
            }
        }
        validator.validate_package(&package.value, package.span.clone());
    }
    validator.finish()
}

#[derive(Default)]
struct Validator {
    errors: Vec<LockValidationError>,
}

impl Validator {
    fn validate_root(&mut self, root: &LockRoot, root_span: Range<usize>) {
        let mut targets = 0;
        let mut required = HashSet::new();
        let mut root_targets = HashSet::new();
        let mut only_targets = Vec::new();
        for field in &root.fields {
            match &field.value {
                LockRootField::Targets(items) => {
                    targets += 1;
                    if items.is_empty() {
                        self.corrupt(LockCorruptionKind::EmptyTargets, field.span.clone());
                    }
                    self.validate_targets(items);
                    root_targets.extend(items.iter().map(|item| item.value.to_string()));
                }
                LockRootField::Requires(requirement) => {
                    self.validate_requirement(
                        requirement,
                        &root.package,
                        &mut required,
                        field.span.clone(),
                    );
                    if let Some(only) = &requirement.only {
                        only_targets.extend(only.iter());
                    }
                }
            }
        }
        for target in only_targets {
            if !root_targets.contains(target.value.as_ref()) {
                self.corrupt(
                    LockCorruptionKind::OnlyTargetNotDeclared(target.value.clone()),
                    target.span.clone(),
                );
            }
        }
        if targets == 0 {
            self.corrupt(
                LockCorruptionKind::MissingField(LockField::Targets),
                root_span.clone(),
            );
        } else if targets > 1 {
            self.corrupt(
                LockCorruptionKind::DuplicateField(LockField::Targets),
                root_span,
            );
        }
    }

    fn validate_package(&mut self, package: &LockPackage, package_span: Range<usize>) {
        let mut fields = HashMap::<LockField, Range<usize>>::new();
        let mut source = None::<(&LockSource, Range<usize>)>;
        let mut artifact_targets = HashSet::<String>::new();
        let mut import_targets = HashSet::<String>::new();
        let mut required = HashSet::new();
        let mut capabilities_present = false;
        let mut artifact_or_import_seen = false;
        let mut artifact_import_fields = Vec::new();

        for field in &package.fields {
            let single_field = match &field.value {
                LockPackageField::Source(source_value) => {
                    source.get_or_insert((source_value, field.span.clone()));
                    Some(LockField::Source)
                }
                LockPackageField::Directory(value) => {
                    if !valid_lock_string(value) || ContentPackagePath::parse(value).is_err() {
                        self.corrupt(LockCorruptionKind::InvalidDirectory, field.span.clone());
                    }
                    Some(LockField::Directory)
                }
                LockPackageField::Commit(value) => {
                    self.validate_string(value, field.span.clone());
                    if !valid_commit(value) {
                        self.corrupt(LockCorruptionKind::InvalidCommit, field.span.clone());
                    }
                    Some(LockField::Commit)
                }
                LockPackageField::Checksum(value) => {
                    self.validate_string(value, field.span.clone());
                    if !valid_checksum(value) {
                        self.corrupt(LockCorruptionKind::InvalidChecksum, field.span.clone());
                    }
                    Some(LockField::Checksum)
                }
                LockPackageField::WitChecksum(value) => {
                    self.validate_string(value, field.span.clone());
                    if !valid_checksum(value) {
                        self.corrupt(LockCorruptionKind::InvalidChecksum, field.span.clone());
                    }
                    Some(LockField::WitChecksum)
                }
                LockPackageField::Artifact { target, checksum } => {
                    artifact_or_import_seen = true;
                    self.validate_target(target, field.span.clone());
                    if !crate::validation::is_component_target(target) {
                        self.corrupt(
                            LockCorruptionKind::ArtifactOnNonComponentTarget(target.clone()),
                            field.span.clone(),
                        );
                    }
                    self.validate_string(checksum, field.span.clone());
                    if !valid_checksum(checksum) {
                        self.corrupt(LockCorruptionKind::InvalidChecksum, field.span.clone());
                    }
                    if !artifact_targets.insert(target.to_string()) {
                        self.corrupt(
                            LockCorruptionKind::DuplicateArtifactTarget(target.clone()),
                            field.span.clone(),
                        );
                    }
                    artifact_import_fields.push((LockField::Artifact, field.span.clone()));
                    None
                }
                LockPackageField::Compiler(_) => Some(LockField::Compiler),
                LockPackageField::Capabilities(items) => {
                    capabilities_present = true;
                    self.validate_capabilities(items);
                    Some(LockField::Capabilities)
                }
                LockPackageField::Imports {
                    target,
                    capabilities,
                } => {
                    artifact_or_import_seen = true;
                    self.validate_target(target, field.span.clone());
                    if !crate::validation::is_component_target(target) {
                        self.corrupt(
                            LockCorruptionKind::ArtifactOnNonComponentTarget(target.clone()),
                            field.span.clone(),
                        );
                    }
                    self.validate_capabilities(capabilities);
                    if !import_targets.insert(target.to_string()) {
                        self.corrupt(
                            LockCorruptionKind::DuplicateArtifactTarget(target.clone()),
                            field.span.clone(),
                        );
                    }
                    artifact_import_fields.push((LockField::Imports, field.span.clone()));
                    None
                }
                LockPackageField::Requires(requirement) => {
                    self.validate_requirement(
                        requirement,
                        &package.package,
                        &mut required,
                        field.span.clone(),
                    );
                    None
                }
            };

            if let Some(field_kind) = single_field {
                if let Some(previous) = fields.insert(field_kind, field.span.clone()) {
                    self.corrupt(
                        LockCorruptionKind::DuplicateField(field_kind),
                        previous.start..field.span.end,
                    );
                }
            }
        }

        let Some((source, source_span)) = source else {
            self.corrupt(
                LockCorruptionKind::MissingField(LockField::Source),
                package_span,
            );
            return;
        };
        match source {
            LockSource::Git(url) => {
                if !valid_lock_string(url) {
                    self.corrupt(LockCorruptionKind::InvalidString, source_span.clone());
                }
                match canonical_git_url(url) {
                    Ok(canonical) if canonical == url.as_ref() => {}
                    Ok(_) => self.corrupt(
                        LockCorruptionKind::NonCanonicalSourceUrl,
                        source_span.clone(),
                    ),
                    Err(()) => {
                        self.corrupt(LockCorruptionKind::InvalidSourceUrl, source_span.clone())
                    }
                }
                for required_field in [LockField::Commit, LockField::Checksum] {
                    if !fields.contains_key(&required_field) {
                        self.corrupt(
                            LockCorruptionKind::MissingField(required_field),
                            package_span.clone(),
                        );
                    }
                }
                if !artifact_or_import_seen && !capabilities_present {
                    self.corrupt(
                        LockCorruptionKind::MissingField(LockField::Capabilities),
                        package_span.clone(),
                    );
                }
                if artifact_targets != import_targets {
                    self.corrupt(
                        LockCorruptionKind::ArtifactImportTargetsDiffer,
                        package_span.clone(),
                    );
                }
                if !artifact_targets.is_empty() && !fields.contains_key(&LockField::WitChecksum) {
                    self.corrupt(
                        LockCorruptionKind::MissingField(LockField::WitChecksum),
                        package_span,
                    );
                }
            }
            LockSource::Path(path) => {
                if !valid_lock_string(path) || !normalized_source_path(path) {
                    self.corrupt(LockCorruptionKind::InvalidSourcePath, source_span.clone());
                }
                for field in [
                    LockField::Directory,
                    LockField::Commit,
                    LockField::Checksum,
                    LockField::WitChecksum,
                    LockField::Capabilities,
                ] {
                    if fields.contains_key(&field) {
                        self.corrupt(
                            LockCorruptionKind::FieldNotAllowed(field),
                            fields[&field].clone(),
                        );
                    }
                }
                for (field, span) in artifact_import_fields {
                    self.corrupt(LockCorruptionKind::FieldNotAllowed(field), span);
                }
            }
        }
    }

    fn validate_identity(&mut self, identity: &str, span: Range<usize>) -> bool {
        if PackageIdentity::parse(identity).is_ok() {
            true
        } else {
            self.corrupt(
                LockCorruptionKind::InvalidPackageIdentity(identity.into()),
                span,
            );
            false
        }
    }

    fn validate_requirement(
        &mut self,
        requirement: &crate::lock::LockRequirement,
        owner: &str,
        seen: &mut HashSet<String>,
        span: Range<usize>,
    ) {
        self.validate_identity(&requirement.package, span.clone());
        if requirement.package.as_ref() == owner {
            self.corrupt(
                LockCorruptionKind::SelfRequirement(requirement.package.clone()),
                span.clone(),
            );
        }
        if !seen.insert(requirement.package.to_string()) {
            self.corrupt(
                LockCorruptionKind::DuplicateRequirement(requirement.package.clone()),
                span.clone(),
            );
        }
        if let Some(only) = &requirement.only {
            if only.is_empty() {
                self.corrupt(LockCorruptionKind::EmptyOnlyClause, span.clone());
            }
            self.validate_targets(only);
        }
    }

    fn validate_targets(&mut self, items: &[Spanned<Box<str>>]) {
        for item in items {
            self.validate_target(&item.value, item.span.clone());
        }
    }

    fn validate_target(&mut self, target: &str, span: Range<usize>) {
        if !crate::manifest::is_target_name(target) {
            self.corrupt(LockCorruptionKind::UnknownTarget(target.into()), span);
        }
    }

    fn validate_capabilities(&mut self, items: &[Spanned<Box<str>>]) {
        for item in items {
            if !crate::validation::is_valid_capability(&item.value) {
                self.corrupt(
                    LockCorruptionKind::InvalidCapability(item.value.clone()),
                    item.span.clone(),
                );
            }
        }
    }

    fn validate_string(&mut self, value: &str, span: Range<usize>) {
        if !valid_lock_string(value) {
            self.corrupt(LockCorruptionKind::InvalidString, span);
        }
    }

    fn error(&mut self, kind: LockValidationErrorKind, span: Range<usize>) {
        self.errors.push(LockValidationError { kind, span });
    }

    fn corrupt(&mut self, kind: LockCorruptionKind, span: Range<usize>) {
        self.error(LockValidationErrorKind::Corrupt(kind), span);
    }

    fn finish(self) -> LockValidationOutput {
        LockValidationOutput {
            errors: self.errors,
        }
    }
}

fn valid_lock_string(value: &str) -> bool {
    value.chars().all(|character| {
        !matches!(character as u32, 0x0000..=0x001f | 0x007f..=0x009f | 0x202a..=0x202e | 0x2066..=0x2069)
    })
}

fn valid_commit(value: &str) -> bool {
    matches!(value.len(), 40 | 64)
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_checksum(value: &str) -> bool {
    value.strip_prefix("sai1:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn normalized_source_path(path: &str) -> bool {
    if DependencyPackagePath::parse(path).is_err() {
        return false;
    }
    if path == "." {
        return true;
    }
    let mut saw_normal_component = false;
    for component in path.split('/') {
        if component == ".." {
            if saw_normal_component {
                return false;
            }
        } else {
            saw_normal_component = true;
        }
    }
    true
}

pub(crate) fn canonical_git_url(value: &str) -> Result<String, ()> {
    if value.len() > 2048 || !value.is_ascii() || value.contains(['?', '#']) {
        return Err(());
    }
    let (scheme, remainder) = value.split_once("://").ok_or(())?;
    let scheme = scheme.to_ascii_lowercase();
    if !matches!(scheme.as_str(), "https" | "ssh") {
        return Err(());
    }

    let authority_end = remainder.find('/').ok_or(())?;
    let authority = &remainder[..authority_end];
    let path = &remainder[authority_end..];
    if authority.is_empty() || path.len() <= 1 {
        return Err(());
    }
    let (userinfo, host_port) = match authority.rsplit_once('@') {
        Some((userinfo, host_port)) => {
            if scheme != "ssh"
                || userinfo.is_empty()
                || userinfo.contains(':')
                || userinfo.contains('@')
                || !userinfo
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"._~!$&'()*+,;=-".contains(&byte))
            {
                return Err(());
            }
            (Some(userinfo), host_port)
        }
        None => (None, authority),
    };

    let (host, port) = if let Some(bracketed) = host_port.strip_prefix('[') {
        let closing = bracketed.find(']').ok_or(())?;
        let address = &bracketed[..closing];
        address.parse::<Ipv6Addr>().map_err(|_| ())?;
        let suffix = &bracketed[closing + 1..];
        let port = if suffix.is_empty() {
            None
        } else {
            Some(suffix.strip_prefix(':').ok_or(())?)
        };
        (&host_port[..closing + 2], port)
    } else {
        let (host, port) = match host_port.rsplit_once(':') {
            Some((host, port)) => {
                if host.contains(':') {
                    return Err(());
                }
                (host, Some(port))
            }
            None => (host_port, None),
        };
        if !valid_dns_host(host) {
            return Err(());
        }
        (host, port)
    };
    if host.is_empty() {
        return Err(());
    }
    if let Some(port) = port {
        if port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err(());
        }
        let port_number = port.parse::<u32>().map_err(|_| ())?;
        if !(1..=65535).contains(&port_number) {
            return Err(());
        }
    }

    let segments = path.strip_prefix('/').ok_or(())?;
    let path_without_trailing_slash = segments.strip_suffix('/').unwrap_or(segments);
    if path_without_trailing_slash.is_empty()
        || path_without_trailing_slash.split('/').any(|segment| {
            segment.is_empty()
                || segment == "."
                || segment == ".."
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"-._~".contains(&byte))
        })
    {
        return Err(());
    }

    let default_port = match scheme.as_str() {
        "https" => Some(443),
        "ssh" => Some(22),
        _ => unreachable!(),
    };
    let keep_port = port.is_some_and(|value| value.parse::<u32>().ok() != default_port);
    let mut canonical = format!("{scheme}://");
    if let Some(userinfo) = userinfo {
        canonical.push_str(userinfo);
        canonical.push('@');
    }
    canonical.push_str(&host.to_ascii_lowercase());
    if keep_port {
        canonical.push(':');
        canonical.push_str(port.expect("port is present"));
    }
    canonical.push('/');
    canonical.push_str(path_without_trailing_slash);
    Ok(canonical)
}

fn valid_dns_host(host: &str) -> bool {
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let host = host.strip_suffix('.').unwrap_or(host);
    let labels = host.split('.').collect::<Vec<_>>();
    if labels.len() == 4
        && labels
            .iter()
            .all(|label| label.bytes().all(|b| b.is_ascii_digit()))
    {
        return host.parse::<std::net::Ipv4Addr>().is_ok();
    }
    !host.is_empty()
        && labels.iter().all(|label| {
            let label = *label;
            !label.is_empty()
                && label.len() <= 63
                && label.as_bytes()[0] != b'-'
                && label.as_bytes()[label.len() - 1] != b'-'
                && label
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        })
}

#[cfg(test)]
mod tests {
    use super::{
        LockCorruptionKind, LockField, LockValidationErrorKind, SUPPORTED_LOCK_FORMAT_VERSION,
        validate,
    };
    use crate::lock::parse;

    fn valid_git_lock() -> &'static str {
        "lock 1\n\
         root acme:app @ 1.0.0\n\
             targets { wasm32-wasi }\n\
         package acme:dep @ 1.2.0\n\
             source git \"https://github.com/acme/dep\"\n\
             dir \"packages/dep\"\n\
             commit \"0123456789012345678901234567890123456789\"\n\
             checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
             capabilities { alloc }\n"
    }

    fn corruption_kinds(source: &str) -> Vec<LockCorruptionKind> {
        validate(&parse(source.as_bytes()))
            .errors
            .into_iter()
            .filter_map(|error| match error.kind {
                LockValidationErrorKind::Corrupt(kind) => Some(kind),
                LockValidationErrorKind::UnsupportedFormatVersion(_) => None,
            })
            .collect()
    }

    #[test]
    fn accepts_a_complete_git_lock_entry() {
        let output = validate(&parse(valid_git_lock().as_bytes()));
        assert!(output.is_valid(), "{:?}", output.errors);
        assert_eq!(SUPPORTED_LOCK_FORMAT_VERSION, 1);
    }

    #[test]
    fn validates_lock_format_version_support() {
        let zero = validate(&parse(
            b"lock 0\nroot acme:app @ 1.0.0\ntargets { wasm32-wasi }\n",
        ));
        assert!(zero.errors.iter().any(|error| {
            error.kind == LockValidationErrorKind::Corrupt(LockCorruptionKind::FormatVersionZero)
        }));

        let future = validate(&parse(
            b"lock 2\nroot acme:app @ 1.0.0\n    future-field value\n",
        ));
        assert_eq!(
            future.errors[0].kind,
            LockValidationErrorKind::UnsupportedFormatVersion(2)
        );
        assert_eq!(future.errors[0].span, 5..6);

        let future_with_invalid_utf8 = validate(&parse(b"lock 2\n\xff\n"));
        assert_eq!(
            future_with_invalid_utf8.errors[0].kind,
            LockValidationErrorKind::UnsupportedFormatVersion(2)
        );

        let nested_comment_with_invalid_utf8_body = validate(&parse(
            b"lock /* outer /* inner */ end */ 2 // future\n\xff",
        ));
        assert_eq!(
            nested_comment_with_invalid_utf8_body.errors[0].kind,
            LockValidationErrorKind::UnsupportedFormatVersion(2)
        );

        let leading_comment_with_invalid_utf8_body =
            validate(&parse(b"/* leading /* nested */ comment */ lock 2\n\xff"));
        assert_eq!(
            leading_comment_with_invalid_utf8_body.errors[0].kind,
            LockValidationErrorKind::UnsupportedFormatVersion(2)
        );

        let oversized_version = parse(b"lock 18446744073709551616\n");
        assert!(!oversized_version.lexical_errors.is_empty());
        assert!(
            validate(&oversized_version)
                .errors
                .iter()
                .all(|error| !matches!(
                    error.kind,
                    LockValidationErrorKind::UnsupportedFormatVersion(_)
                ))
        );
    }

    #[test]
    fn enforces_required_and_single_occurrence_fields() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
                 targets { wasm32-wasi }\n\
             package acme:dep @ 1.2.0\n\
                 source git \"https://github.com/acme/dep\"\n\
                 source git \"https://github.com/acme/dep\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 capabilities { alloc }\n";
        let kinds = corruption_kinds(source);
        assert!(kinds.contains(&LockCorruptionKind::DuplicateField(LockField::Targets)));
        assert!(kinds.contains(&LockCorruptionKind::DuplicateField(LockField::Source)));
        assert!(kinds.contains(&LockCorruptionKind::MissingField(LockField::Commit)));
    }

    #[test]
    fn rejects_reserved_and_malformed_package_identities() {
        let source = "lock 1\n\
             root sai:core @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package wasi:dep @ 1.0.0\n\
                 source path \"../dep\"\n";
        let kinds = corruption_kinds(source);
        assert_eq!(
            kinds
                .iter()
                .filter(|kind| matches!(kind, LockCorruptionKind::InvalidPackageIdentity(_)))
                .count(),
            2
        );
    }

    #[test]
    fn rejects_duplicate_entries_on_the_same_compatibility_line() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package acme:dep @ 1.0.0\n\
                 source path \"../dep-a\"\n\
             package acme:dep @ 1.2.0\n\
                 source path \"../dep-b\"\n";
        assert!(
            corruption_kinds(source).contains(&LockCorruptionKind::DuplicateEntry {
                package: "acme:dep".into(),
                line: crate::version::Version::parse("1.0.0")
                    .unwrap()
                    .compatibility_line(),
            })
        );
    }

    #[test]
    fn validates_commit_checksum_and_decoded_string_forms() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package acme:dep @ 1.2.0\n\
                 source git \"https://github.com/acme/dep\\n\"\n\
                 commit \"ABC\"\n\
                 checksum \"sai1:bad\"\n\
                 capabilities { alloc }\n";
        let kinds = corruption_kinds(source);
        assert!(kinds.contains(&LockCorruptionKind::InvalidString));
        assert!(kinds.contains(&LockCorruptionKind::InvalidCommit));
        assert!(kinds.contains(&LockCorruptionKind::InvalidChecksum));
        assert!(kinds.contains(&LockCorruptionKind::InvalidSourceUrl));
    }

    #[test]
    fn validates_known_targets_and_root_only_sets() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 requires acme:dep @ ^1 -> 1.2.0 only { wasm32-wasi }\n\
                 targets { imaginary, wasm32-core, wasm32-core }\n\
             package acme:dep @ 1.2.0\n\
                 source path \"../dep\"\n";
        let kinds = corruption_kinds(source);
        assert!(kinds.contains(&LockCorruptionKind::UnknownTarget("imaginary".into())));
        assert!(kinds.contains(&LockCorruptionKind::OnlyTargetNotDeclared(
            "wasm32-wasi".into()
        )));
    }

    #[test]
    fn treats_duplicate_set_members_as_noncanonical_values() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi, wasm32-wasi }\n\
                 requires acme:dep @ ^1 -> 1.2.0 only { wasm32-wasi, wasm32-wasi }\n\
             package acme:dep @ 1.2.0\n\
                 source git \"https://github.com/acme/dep\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 capabilities { alloc, alloc }\n";
        assert!(corruption_kinds(source).is_empty());
    }

    #[test]
    fn rejects_an_empty_root_target_set() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { }\n";
        assert!(corruption_kinds(source).contains(&LockCorruptionKind::EmptyTargets));
    }

    #[test]
    fn rejects_empty_only_clauses() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
                 requires acme:dep @ ^1 -> 1.2.0 only { }\n\
             package acme:dep @ 1.2.0\n\
                 source path \"../dep\"\n";
        assert!(corruption_kinds(source).contains(&LockCorruptionKind::EmptyOnlyClause));
    }

    #[test]
    fn validates_requirement_identity_uniqueness_and_self_edges() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
                 requires acme:app @ ^1 -> 1.0.0\n\
                 requires acme:app @ ^1 -> 1.0.0\n\
             package acme:dep @ 1.2.0\n\
                 source path \"../dep\"\n\
                 requires sai:core @ ^1 -> 1.0.0\n";
        let kinds = corruption_kinds(source);
        assert!(kinds.contains(&LockCorruptionKind::SelfRequirement("acme:app".into())));
        assert!(kinds.contains(&LockCorruptionKind::DuplicateRequirement("acme:app".into())));
        assert!(kinds.contains(&LockCorruptionKind::InvalidPackageIdentity(
            "sai:core".into()
        )));
    }

    #[test]
    fn enforces_normalized_source_and_repository_directory_paths() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package acme:dep @ 1.2.0\n\
                 source path \"../vendor/../dep\"\n\
             package acme:other @ 1.3.0\n\
                 source git \"https://github.com/acme/other\"\n\
                 dir \"packages/../other\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 capabilities { alloc }\n";
        let kinds = corruption_kinds(source);
        assert!(kinds.contains(&LockCorruptionKind::InvalidSourcePath));
        assert!(kinds.contains(&LockCorruptionKind::InvalidDirectory));
    }

    #[test]
    fn validates_git_url_safety_and_canonical_form() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package acme:credential @ 1.0.0\n\
                 source git \"https://user@example.test/repo\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 capabilities { alloc }\n\
             package acme:noncanonical @ 1.0.0\n\
                 source git \"HTTPS://GitHub.COM:443/acme/repo/\"\n\
                 commit \"1123456789012345678901234567890123456789\"\n\
                 checksum \"sai1:1123456789012345678901234567890123456789012345678901234567890123\"\n\
                 capabilities { alloc }\n";
        let kinds = corruption_kinds(source);
        assert!(kinds.contains(&LockCorruptionKind::InvalidSourceUrl));
        assert!(kinds.contains(&LockCorruptionKind::NonCanonicalSourceUrl));

        assert!(corruption_kinds(valid_git_lock()).is_empty());
        assert_eq!(
            super::canonical_git_url("ssh://build+bot@github.com/acme/dep").unwrap(),
            "ssh://build+bot@github.com/acme/dep"
        );
    }

    #[test]
    fn canonical_git_url_handles_ssh_ipv6_ports_and_rejects_ambiguous_forms() {
        assert_eq!(
            super::canonical_git_url("SSH://git@[2001:DB8::1]:22/acme/repo/").unwrap(),
            "ssh://git@[2001:db8::1]/acme/repo"
        );
        assert_eq!(
            super::canonical_git_url("https://127.0.0.1:8443/acme/repo").unwrap(),
            "https://127.0.0.1:8443/acme/repo"
        );
        for url in [
            "http://example.test/repo",
            "git://example.test/repo",
            "ssh://git:secret@example.test/repo",
            "ssh://build%2Bbot@example.test/repo",
            "ssh://git%2@example.test/repo",
            "https://example.test:0/repo",
            "https://example.test:+443/repo",
            "https://999.999.999.999/repo",
            "https://example.test/a%2fb",
            "https://example.test/a/../b",
            "https://example.test/repo?query",
            "https://example.test/repo#fragment",
            "https://[2001:db8::gg]/repo",
            "https://example.test:65536/repo",
            "https://example.test//repo",
        ] {
            assert!(
                super::canonical_git_url(url).is_err(),
                "unexpectedly accepted {url:?}"
            );
        }
    }

    #[test]
    fn validates_artifact_targets_wit_and_capability_names() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package acme:artifact @ 1.0.0\n\
                 source git \"https://github.com/acme/artifact\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 artifact wasm32-core \"sai1:1123456789012345678901234567890123456789012345678901234567890123\"\n\
                 imports wasm32-core { acme:cap/foo/bar }\n";
        let kinds = corruption_kinds(source);
        assert!(
            kinds.contains(&LockCorruptionKind::ArtifactOnNonComponentTarget(
                "wasm32-core".into()
            ))
        );
        assert!(kinds.contains(&LockCorruptionKind::MissingField(LockField::WitChecksum)));
        assert!(kinds.contains(&LockCorruptionKind::InvalidCapability(
            "acme:cap/foo/bar".into()
        )));
    }

    #[test]
    fn requires_artifact_and_import_target_sets_to_match() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi, wasm64-wasi }\n\
             package acme:hybrid @ 1.0.0\n\
                 source git \"https://github.com/acme/hybrid\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 wit \"sai1:2123456789012345678901234567890123456789012345678901234567890123\"\n\
                 artifact wasm32-wasi \"sai1:1123456789012345678901234567890123456789012345678901234567890123\"\n\
                 imports wasm64-wasi { alloc }\n";
        assert!(
            corruption_kinds(source).contains(&LockCorruptionKind::ArtifactImportTargetsDiffer)
        );
    }

    #[test]
    fn rejects_duplicate_artifact_target_fields() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package acme:artifact @ 1.0.0\n\
                 source git \"https://github.com/acme/artifact\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 wit \"sai1:2123456789012345678901234567890123456789012345678901234567890123\"\n\
                 artifact wasm32-wasi \"sai1:1123456789012345678901234567890123456789012345678901234567890123\"\n\
                 artifact wasm32-wasi \"sai1:3123456789012345678901234567890123456789012345678901234567890123\"\n\
                 imports wasm32-wasi { alloc }\n\
                 imports wasm32-wasi { alloc }\n";
        assert_eq!(
            corruption_kinds(source)
                .iter()
                .filter(|kind| {
                    matches!(
                        kind,
                        LockCorruptionKind::DuplicateArtifactTarget(target)
                            if target.as_ref() == "wasm32-wasi"
                    )
                })
                .count(),
            2
        );
    }

    #[test]
    fn path_entries_forbid_git_content_and_artifact_fields() {
        let source = "lock 1\n\
             root acme:app @ 1.0.0\n\
                 targets { wasm32-wasi }\n\
             package acme:path @ 1.0.0\n\
                 source path \"../path\"\n\
                 commit \"0123456789012345678901234567890123456789\"\n\
                 checksum \"sai1:0123456789012345678901234567890123456789012345678901234567890123\"\n\
                 artifact wasm32-wasi \"sai1:1123456789012345678901234567890123456789012345678901234567890123\"\n\
                 imports wasm32-wasi { alloc }\n";
        let kinds = corruption_kinds(source);
        assert!(kinds.contains(&LockCorruptionKind::FieldNotAllowed(LockField::Commit)));
        assert!(kinds.contains(&LockCorruptionKind::FieldNotAllowed(LockField::Checksum)));
        assert!(kinds.contains(&LockCorruptionKind::FieldNotAllowed(LockField::Artifact)));
        assert!(kinds.contains(&LockCorruptionKind::FieldNotAllowed(LockField::Imports)));
    }
}
