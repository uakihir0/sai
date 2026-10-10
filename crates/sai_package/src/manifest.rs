//! Parsing for the `sai.package` package contract.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use sha2::{Digest, Sha256};

use crate::identity::{PackageIdentity, PackageIdentityError};
use crate::lexer::{
    PackageLexError, PackageLexErrorKind, PackagePunctuator, PackageToken, PackageTokenKind,
    PackageTrivia, lex,
};
use crate::package_path::{
    ContentPackagePath, DependencyPackagePath, PackagePathError, RepositoryPackageDirectory,
};
use crate::version::{Version, VersionParseError};
use crate::version_req::{PartialVersion, VersionReq, VersionReqParseError};

/// Maximum number of bytes accepted for a `sai.package` manifest.
pub const MAX_PACKAGE_MANIFEST_BYTES: usize = 16 * 1024 * 1024;

/// A value associated with its half-open byte range in the original file.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Spanned<T> {
    pub value: T,
    pub span: Range<usize>,
}

/// The validated identity and version from the first package declaration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageHeader {
    pub identity: PackageIdentity,
    pub version: Version,
}

/// Whether a package contract is checked as the build root or a dependency.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum PackageRole {
    #[default]
    Root,
    Dependency,
}

/// A parsed package contract. Declarations with invalid headers, required
/// structure, or local path values are omitted. Within a recoverable list,
/// valid entries are retained alongside parser diagnostics for malformed
/// entries; consumers must check the parse diagnostics before semantic use.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PackageFile {
    pub header: Option<Spanned<PackageHeader>>,
    pub declarations: Vec<Spanned<PackageDeclaration>>,
    pub trivia: Vec<PackageTrivia>,
}

/// A syntactically parsed declaration in a package contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageDeclaration {
    Compiler(VersionReq),
    Source(Box<str>),
    Wit(Box<str>),
    ArtifactComponents(Vec<Spanned<ArtifactEntry>>),
    Targets(Vec<Spanned<Box<str>>>),
    ExportWorld(Box<str>),
    Requires {
        test: bool,
        entries: Vec<Spanned<Requirement>>,
    },
    Allow(Vec<Spanned<Box<str>>>),
    Deny(Vec<Spanned<Box<str>>>),
    Policy(Vec<Spanned<Box<str>>>),
    Lints(Vec<Spanned<LintItem>>),
    Profile {
        name: Box<str>,
        items: Vec<Spanned<ProfileItem>>,
    },
    Allocator(Box<str>),
    Overrides(Vec<Spanned<Override>>),
    Description(Box<str>),
    License(Box<str>),
    Repository(Box<str>),
    Authors(Vec<Spanned<Box<str>>>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ArtifactEntry {
    pub target: Box<str>,
    pub path: Box<str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Requirement {
    /// The package-path spelling is retained so semantic validation can
    /// distinguish implicit toolchain packages from ordinary identities.
    pub package: Box<str>,
    pub version: VersionReq,
    pub source: Option<Spanned<RequirementSource>>,
    pub only: Option<Spanned<Vec<Spanned<Box<str>>>>>,
    pub grant: Option<Spanned<Vec<Spanned<Box<str>>>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RequirementSource {
    Git {
        url: Box<str>,
        directory: Option<Box<str>>,
    },
    Path(Box<str>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintItem {
    pub level: LintLevel,
    pub code: Box<str>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LintLevel {
    Allow,
    Warn,
    Deny,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProfileItem {
    Allow(Vec<Spanned<Box<str>>>),
    Budget(Vec<Spanned<BudgetItem>>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BudgetItem {
    pub metric: Box<str>,
    /// The decimal source spelling, retained for size validation.
    pub amount: Box<str>,
    pub unit: Box<str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Override {
    pub package: Box<str>,
    pub source: OverrideSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OverrideSource {
    Artifact,
    Path(Box<str>),
    Git {
        url: Box<str>,
        directory: Option<Box<str>>,
    },
}

/// A syntax or structural error produced while parsing a package contract.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageParseError {
    pub kind: PackageParseErrorKind,
    pub span: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageParseErrorKind {
    MissingPackageDeclaration,
    PackageDeclarationNotFirst,
    DuplicateDeclaration(Box<str>),
    UnknownDeclaration {
        name: Box<str>,
        suggestion: Option<Box<str>>,
    },
    UnknownArtifactKind(Box<str>),
    ExpectedToken(Box<str>),
    ExpectedListSeparator,
    ExpectedDeclaration,
    ExpectedProfileItem,
    InvalidPackageIdentity(PackageIdentityError),
    InvalidPackageVersion(VersionParseError),
    InvalidContentPath(PackagePathError),
    InvalidDependencyPath(PackagePathError),
    InvalidRepositoryDirectory(PackagePathError),
    ConflictingLintLevel,
    InvalidVersionRequirement(VersionReqParseError),
    ManifestTooLarge,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageParseWarning {
    pub kind: PackageParseWarningKind,
    pub span: Range<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PackageParseWarningKind {
    DuplicateListItem,
}

/// The syntax tree and diagnostics from parsing one `sai.package` file.
#[derive(Clone, Debug)]
pub struct PackageParseOutput {
    pub(crate) file: PackageFile,
    manifest_digest: Option<[u8; 32]>,
    /// Package-graph role used while parsing, which affects root-only
    /// duplicate diagnostics.
    role: PackageRole,
    pub(crate) lexical_errors: Vec<PackageLexError>,
    pub(crate) errors: Vec<PackageParseError>,
    pub(crate) warnings: Vec<PackageParseWarning>,
}

impl PartialEq for PackageParseOutput {
    fn eq(&self, other: &Self) -> bool {
        self.file == other.file
            && self.role == other.role
            && self.lexical_errors == other.lexical_errors
            && self.errors == other.errors
            && self.warnings == other.warnings
    }
}

impl Eq for PackageParseOutput {}

impl PackageParseOutput {
    pub const fn file(&self) -> &PackageFile {
        &self.file
    }

    /// SHA-256 digest of the exact manifest bytes supplied to the parser.
    ///
    /// The digest is unavailable when the input exceeds the manifest-size
    /// limit and is rejected before parsing.
    pub const fn manifest_digest(&self) -> Option<&[u8; 32]> {
        self.manifest_digest.as_ref()
    }

    pub const fn role(&self) -> PackageRole {
        self.role
    }

    pub fn lexical_errors(&self) -> &[PackageLexError] {
        &self.lexical_errors
    }

    pub fn errors(&self) -> &[PackageParseError] {
        &self.errors
    }

    pub fn warnings(&self) -> &[PackageParseWarning] {
        &self.warnings
    }

    /// Reports whether lexing and syntax parsing succeeded.
    ///
    /// Package-wide semantic checks such as target and capability validation
    /// are performed by later validation stages.
    pub fn is_syntax_valid(&self) -> bool {
        self.lexical_errors.is_empty() && self.errors.is_empty()
    }
}

/// Lexes and parses one `sai.package` file.
///
/// Source positions are UTF-8 byte offsets. Lexical errors are returned
/// separately from parser errors. Later declarations are retained when the
/// parser can recover an unambiguous declaration boundary. This entry point
/// treats the package as the build root; use [`parse_for_role`] for a
/// dependency so ignored root-only declarations do not produce duplicate
/// diagnostics.
pub fn parse(source: &[u8]) -> PackageParseOutput {
    parse_for_role(source, PackageRole::Root)
}

/// Lexes and parses one `sai.package` file in its package-graph role.
///
/// Root-only declarations remain in the syntax tree for dependencies, but
/// duplicate-declaration and duplicate-list diagnostics for those ignored
/// declarations are suppressed. Inputs larger than
/// [`MAX_PACKAGE_MANIFEST_BYTES`] return one `ManifestTooLarge` error without
/// lexing the file.
pub fn parse_for_role(source: &[u8], role: PackageRole) -> PackageParseOutput {
    if source.len() > MAX_PACKAGE_MANIFEST_BYTES {
        return PackageParseOutput {
            file: PackageFile::default(),
            manifest_digest: None,
            role,
            lexical_errors: Vec::new(),
            errors: vec![PackageParseError {
                kind: PackageParseErrorKind::ManifestTooLarge,
                span: 0..0,
            }],
            warnings: Vec::new(),
        };
    }

    let manifest_digest = Some(Sha256::digest(source).into());
    let lexed = lex(source);
    let mut file = PackageFile {
        trivia: lexed.trivia,
        ..PackageFile::default()
    };
    let lexical_errors = lexed.errors;
    if lexical_errors
        .iter()
        .any(|error| error.kind == PackageLexErrorKind::InvalidUtf8)
    {
        return PackageParseOutput {
            file,
            manifest_digest,
            role,
            lexical_errors,
            errors: Vec::new(),
            warnings: Vec::new(),
        };
    }

    let text = std::str::from_utf8(source).expect("lexer only returns tokens for UTF-8 input");
    let mut parser = Parser::new(text, lexed.tokens, &lexical_errors, role);
    parser.parse_file(&mut file);
    collect_list_diagnostics(&file, role, &mut parser.warnings, &mut parser.errors);
    PackageParseOutput {
        file,
        manifest_digest,
        role,
        lexical_errors,
        errors: parser.errors,
        warnings: parser.warnings,
    }
}

struct Parser<'a> {
    text: &'a str,
    tokens: Vec<PackageToken>,
    lexical_error_spans: Vec<Range<usize>>,
    position: usize,
    errors: Vec<PackageParseError>,
    warnings: Vec<PackageParseWarning>,
    declaration_keys: HashSet<DeclarationKey>,
    role: PackageRole,
    package_declarations: usize,
    recovered_declaration_boundary: Option<usize>,
}

impl<'a> Parser<'a> {
    fn new(
        text: &'a str,
        tokens: Vec<PackageToken>,
        lexical_errors: &[PackageLexError],
        role: PackageRole,
    ) -> Self {
        Self {
            text,
            tokens,
            lexical_error_spans: lexical_errors
                .iter()
                .map(|error| error.span.clone())
                .collect(),
            position: 0,
            errors: Vec::new(),
            warnings: Vec::new(),
            declaration_keys: HashSet::new(),
            role,
            package_declarations: 0,
            recovered_declaration_boundary: None,
        }
    }

    fn parse_file(&mut self, file: &mut PackageFile) {
        self.skip_newlines();
        let mut declaration_count = 0;
        let mut saw_package = false;
        while !self.at_eof() {
            let start_position = self.position;
            let first = declaration_count == 0;
            let mut recovered_declaration_boundary = false;
            if self.at_word("package") {
                if !first {
                    self.error_here(PackageParseErrorKind::PackageDeclarationNotFirst);
                }
                self.package_declarations += 1;
                if self.package_declarations > 1 {
                    self.error_here(PackageParseErrorKind::DuplicateDeclaration(
                        "package".into(),
                    ));
                }
                saw_package = true;
                if let Some(header) = self.parse_package_header() {
                    if file.header.is_none() {
                        file.header = Some(header);
                    }
                }
            } else {
                let declaration_key = self.current_declaration_key();
                let declaration = self.parse_declaration();
                recovered_declaration_boundary =
                    self.recovered_declaration_boundary.take().is_some();
                if let Some(key) = declaration_key {
                    if (self.role == PackageRole::Root || !key.is_root_only())
                        && !self.declaration_keys.insert(key.clone())
                    {
                        self.error(
                            PackageParseErrorKind::DuplicateDeclaration(key.name()),
                            declaration.as_ref().map_or_else(
                                || self.tokens[start_position].span.clone(),
                                |declaration| declaration.span.clone(),
                            ),
                        );
                    }
                }
                if let Some(declaration) = declaration {
                    file.declarations.push(declaration);
                } else if self.position == start_position {
                    self.error_here(PackageParseErrorKind::ExpectedDeclaration);
                    self.bump();
                }
            }

            declaration_count += 1;
            if recovered_declaration_boundary {
                if self.at_newline() {
                    self.skip_newlines();
                }
            } else if self.at_newline() {
                self.skip_newlines();
            } else if !self.at_eof() {
                self.error_here(PackageParseErrorKind::ExpectedToken(
                    "newline between declarations".into(),
                ));
                self.recover_to_newline();
            }
        }

        if !saw_package {
            let end = self.current().span.start;
            self.error(PackageParseErrorKind::MissingPackageDeclaration, end..end);
        }
    }

    fn parse_package_header(&mut self) -> Option<Spanned<PackageHeader>> {
        let start = self.position;
        self.bump(); // `package`
        let identity_token = self.expect_kind(PackageTokenKind::PackagePath, "package identity")?;
        let identity_text = self.token_text(identity_token);
        let identity = match PackageIdentity::parse(identity_text) {
            Ok(identity) => Some(identity),
            Err(error) => {
                self.error(
                    PackageParseErrorKind::InvalidPackageIdentity(error),
                    self.tokens[identity_token].span.clone(),
                );
                None
            }
        };
        self.expect_punctuator(PackagePunctuator::At, "`@`")?;
        let version_token = self.expect_kind(PackageTokenKind::Version, "full package version")?;
        let version_text = self.token_text(version_token);
        let version = match Version::parse(version_text) {
            Ok(version) => Some(version),
            Err(error) => {
                self.error(
                    PackageParseErrorKind::InvalidPackageVersion(error),
                    self.tokens[version_token].span.clone(),
                );
                None
            }
        };
        let (Some(identity), Some(version)) = (identity, version) else {
            return None;
        };
        Some(Spanned {
            value: PackageHeader { identity, version },
            span: self.span_from(start),
        })
    }

    fn current_declaration_key(&self) -> Option<DeclarationKey> {
        if !self.at_kind(PackageTokenKind::Word) {
            return None;
        }
        let keyword = self.token_text(self.position);
        Some(match keyword {
            "compiler" => DeclarationKey::Compiler,
            "source" => DeclarationKey::Source,
            "wit" => DeclarationKey::Wit,
            "artifact" => DeclarationKey::Artifact,
            "targets" => DeclarationKey::Targets,
            "export" => DeclarationKey::ExportWorld,
            "requires" => {
                DeclarationKey::Requires(self.tokens.get(self.position + 1).is_some_and(|token| {
                    token.kind == PackageTokenKind::Word && &self.text[token.span.clone()] == "test"
                }))
            }
            "allow" => DeclarationKey::Allow,
            "deny" => DeclarationKey::Deny,
            "policy" => DeclarationKey::Policy,
            "lints" => DeclarationKey::Lints,
            "profile" => {
                let Some(name_token) = self.tokens.get(self.position + 1) else {
                    return Some(DeclarationKey::Profile("<missing>".into()));
                };
                if name_token.kind != PackageTokenKind::Word {
                    return Some(DeclarationKey::Profile("<missing>".into()));
                }
                DeclarationKey::Profile(self.text[name_token.span.clone()].into())
            }
            "allocator" => DeclarationKey::Allocator,
            "overrides" => DeclarationKey::Overrides,
            "description" => DeclarationKey::Description,
            "license" => DeclarationKey::License,
            "repository" => DeclarationKey::Repository,
            "authors" => DeclarationKey::Authors,
            _ => return None,
        })
    }

    fn parse_declaration(&mut self) -> Option<Spanned<PackageDeclaration>> {
        let start = self.position;
        let keyword = if self.at_kind(PackageTokenKind::Word) {
            self.token_text(self.position).to_owned()
        } else {
            return None;
        };
        let declaration = match keyword.as_str() {
            "compiler" => {
                self.bump();
                self.parse_version_requirement()
                    .map(PackageDeclaration::Compiler)
            }
            "source" => {
                self.bump();
                self.parse_content_path().map(PackageDeclaration::Source)
            }
            "wit" => {
                self.bump();
                self.parse_content_path().map(PackageDeclaration::Wit)
            }
            "artifact" => self.parse_artifact(),
            "targets" => {
                self.bump();
                let values = self.parse_targets_list()?;
                Some(PackageDeclaration::Targets(values))
            }
            "export" => self.parse_export_world(),
            "requires" => self.parse_requires(),
            "allow" => {
                self.bump();
                Some(PackageDeclaration::Allow(self.parse_capability_list()?))
            }
            "deny" => {
                self.bump();
                Some(PackageDeclaration::Deny(self.parse_capability_list()?))
            }
            "policy" => self.parse_policy(),
            "lints" => self.parse_lints(),
            "profile" => self.parse_profile(),
            "allocator" => {
                self.bump();
                self.parse_package_path().map(PackageDeclaration::Allocator)
            }
            "overrides" => self.parse_overrides(),
            "description" => {
                self.bump();
                self.parse_string().map(PackageDeclaration::Description)
            }
            "license" => {
                self.bump();
                self.parse_string().map(PackageDeclaration::License)
            }
            "repository" => {
                self.bump();
                self.parse_string().map(PackageDeclaration::Repository)
            }
            "authors" => {
                self.bump();
                let authors =
                    self.parse_braced_list(true, ListSeparator::CommaOrLine, |parser| {
                        parser.parse_string()
                    })?;
                Some(PackageDeclaration::Authors(authors))
            }
            _ => {
                let token = self.position;
                self.bump();
                let suggestion = declaration_suggestion(&keyword);
                self.error(
                    PackageParseErrorKind::UnknownDeclaration {
                        name: keyword.into_boxed_str(),
                        suggestion,
                    },
                    self.tokens[token].span.clone(),
                );
                None
            }
        }?;
        Some(Spanned {
            value: declaration,
            span: self.span_from(start),
        })
    }

    fn parse_artifact(&mut self) -> Option<PackageDeclaration> {
        self.bump(); // `artifact`
        let is_component = self.at_word("component");
        if !is_component {
            if self.at_kind(PackageTokenKind::Word) {
                let token = self.position;
                let kind = self.token_text(token).to_owned().into_boxed_str();
                self.bump();
                self.error(
                    PackageParseErrorKind::UnknownArtifactKind(kind),
                    self.tokens[token].span.clone(),
                );
            } else {
                self.error_here(PackageParseErrorKind::ExpectedToken("`component`".into()));
                return None;
            }
        } else {
            self.bump();
        }
        let entries = self.parse_braced_list_inner(
            false,
            ListSeparator::LinesOnly,
            ListRecovery::ArtifactEntries,
            |parser| {
                let target = parser.parse_word()?;
                let path = parser.parse_content_path()?;
                Some(ArtifactEntry { target, path })
            },
        )?;
        is_component.then_some(PackageDeclaration::ArtifactComponents(entries))
    }

    fn parse_export_world(&mut self) -> Option<PackageDeclaration> {
        self.bump(); // `export`
        self.expect_word("world")?;
        let world = if self.at_kind(PackageTokenKind::PackagePath) {
            let value = self.token_text(self.position).to_owned().into_boxed_str();
            self.bump();
            Some(value)
        } else {
            let mut segments = vec![self.parse_word()?];
            while self.eat_punctuator(PackagePunctuator::Slash).is_some() {
                segments.push(self.parse_word()?);
            }
            Some(segments.join("/").into_boxed_str())
        };
        world.map(PackageDeclaration::ExportWorld)
    }

    fn parse_requires(&mut self) -> Option<PackageDeclaration> {
        self.bump(); // `requires`
        let test = if self.at_word("test") {
            self.bump();
            true
        } else {
            false
        };
        let entries = self.parse_braced_list(true, ListSeparator::LinesOnly, |parser| {
            parser.parse_requirement()
        })?;
        Some(PackageDeclaration::Requires { test, entries })
    }

    fn parse_requirement(&mut self) -> Option<Requirement> {
        let package = self.parse_package_path()?;
        self.expect_punctuator(PackagePunctuator::At, "`@`")?;
        let version = self.parse_version_requirement()?;
        let source = if self.at_word("from") {
            Some(self.parse_requirement_source()?)
        } else {
            None
        };
        let only = if self.at_word("only") {
            let only_start = self.position;
            self.bump();
            let items = self.parse_braced_list(false, ListSeparator::CommaOrLine, |parser| {
                parser.parse_word()
            })?;
            Some(Spanned {
                value: items,
                span: self.span_from(only_start),
            })
        } else {
            None
        };
        let grant = if self.at_word("grant") {
            let grant_start = self.position;
            self.bump();
            let items = self.parse_braced_list(true, ListSeparator::CommaOrLine, |parser| {
                parser.parse_capability()
            })?;
            Some(Spanned {
                value: items,
                span: self.span_from(grant_start),
            })
        } else {
            None
        };
        Some(Requirement {
            package,
            version,
            source,
            only,
            grant,
        })
    }

    fn parse_requirement_source(&mut self) -> Option<Spanned<RequirementSource>> {
        let start = self.position;
        self.expect_word("from")?;
        let source = if self.at_word("git") {
            self.bump();
            let url = self.parse_string()?;
            let directory = if self.at_word("dir") {
                self.bump();
                Some(self.parse_repository_package_directory()?)
            } else {
                None
            };
            RequirementSource::Git { url, directory }
        } else if self.at_word("path") {
            self.bump();
            RequirementSource::Path(self.parse_dependency_path()?)
        } else {
            self.error_here(PackageParseErrorKind::ExpectedToken(
                "`git` or `path`".into(),
            ));
            return None;
        };
        Some(Spanned {
            value: source,
            span: self.span_from(start),
        })
    }

    fn parse_policy(&mut self) -> Option<PackageDeclaration> {
        self.bump();
        let items = self.parse_braced_list(true, ListSeparator::CommaOrLine, |parser| {
            parser.expect_word("deny")?;
            parser.parse_word()
        })?;
        Some(PackageDeclaration::Policy(items))
    }

    fn parse_lints(&mut self) -> Option<PackageDeclaration> {
        self.bump();
        let items = self.parse_braced_list(true, ListSeparator::CommaOrLine, |parser| {
            let level = match parser.current_word() {
                Some("allow") => LintLevel::Allow,
                Some("warn") => LintLevel::Warn,
                Some("deny") => LintLevel::Deny,
                _ => {
                    parser.error_here(PackageParseErrorKind::ExpectedToken(
                        "`allow`, `warn`, or `deny`".into(),
                    ));
                    return None;
                }
            };
            parser.bump();
            let code = parser.expect_kind(PackageTokenKind::UpperWord, "lint code")?;
            Some(LintItem {
                level,
                code: parser.token_text(code).into(),
            })
        })?;
        Some(PackageDeclaration::Lints(items))
    }

    fn parse_profile(&mut self) -> Option<PackageDeclaration> {
        self.bump();
        let name = self.parse_word()?;
        let role = self.role;
        let mut seen = HashSet::new();
        let items = self.parse_braced_list_inner(
            true,
            ListSeparator::LinesOnly,
            ListRecovery::ProfileItems,
            |parser| {
                let item_start = parser.position;
                let item = match parser.current_word() {
                    Some("allow") => {
                        parser.bump();
                        ProfileItem::Allow(parser.parse_capability_list()?)
                    }
                    Some("budget") => {
                        parser.bump();
                        let entries = parser.parse_braced_list(
                            true,
                            ListSeparator::CommaOrLine,
                            |parser| {
                                let metric = parser.parse_word()?;
                                parser.expect_punctuator(PackagePunctuator::LessEqual, "`<=`")?;
                                let amount = parser
                                    .expect_kind(PackageTokenKind::Integer, "size integer")?;
                                let amount = parser.token_text(amount).into();
                                let unit =
                                    parser.expect_kind(PackageTokenKind::UpperWord, "size unit")?;
                                Some(BudgetItem {
                                    metric,
                                    amount,
                                    unit: parser.token_text(unit).into(),
                                })
                            },
                        )?;
                        ProfileItem::Budget(entries)
                    }
                    _ => {
                        parser.error_here(PackageParseErrorKind::ExpectedProfileItem);
                        return None;
                    }
                };
                let key = match &item {
                    ProfileItem::Allow(_) => "allow",
                    ProfileItem::Budget(_) => "budget",
                };
                if !seen.insert(key) && role == PackageRole::Root {
                    parser.error(
                        PackageParseErrorKind::DuplicateDeclaration(key.into()),
                        parser.span_from(item_start),
                    );
                }
                Some(item)
            },
        )?;
        Some(PackageDeclaration::Profile { name, items })
    }

    fn parse_overrides(&mut self) -> Option<PackageDeclaration> {
        self.bump();
        let overrides = self.parse_braced_list(true, ListSeparator::LinesOnly, |parser| {
            let package = parser.parse_package_path()?;
            parser.expect_word("use")?;
            let source = if parser.at_word("artifact") {
                parser.bump();
                OverrideSource::Artifact
            } else if parser.at_word("path") {
                parser.bump();
                OverrideSource::Path(parser.parse_dependency_path()?)
            } else if parser.at_word("git") {
                parser.bump();
                let url = parser.parse_string()?;
                let directory = if parser.at_word("dir") {
                    parser.bump();
                    Some(parser.parse_repository_package_directory()?)
                } else {
                    None
                };
                OverrideSource::Git { url, directory }
            } else {
                parser.error_here(PackageParseErrorKind::ExpectedToken(
                    "`artifact`, `path`, or `git`".into(),
                ));
                return None;
            };
            Some(Override { package, source })
        })?;
        Some(PackageDeclaration::Overrides(overrides))
    }

    fn parse_capability_list(&mut self) -> Option<Vec<Spanned<Box<str>>>> {
        self.parse_braced_list(true, ListSeparator::CommaOrLine, |parser| {
            parser.parse_capability()
        })
    }

    fn parse_targets_list(&mut self) -> Option<Vec<Spanned<Box<str>>>> {
        self.parse_braced_list_inner(
            true,
            ListSeparator::CommaOrLine,
            ListRecovery::Declarations,
            |parser| parser.parse_word(),
        )
    }

    fn parse_braced_list<T>(
        &mut self,
        allow_empty: bool,
        separator: ListSeparator,
        parse_item: impl FnMut(&mut Self) -> Option<T>,
    ) -> Option<Vec<Spanned<T>>> {
        self.parse_braced_list_inner(
            allow_empty,
            separator,
            ListRecovery::Declarations,
            parse_item,
        )
    }

    fn parse_braced_list_inner<T>(
        &mut self,
        allow_empty: bool,
        separator: ListSeparator,
        recovery: ListRecovery,
        mut parse_item: impl FnMut(&mut Self) -> Option<T>,
    ) -> Option<Vec<Spanned<T>>> {
        self.expect_punctuator(PackagePunctuator::LeftBrace, "`{`")?;
        let initial_separator = self.position;
        self.skip_newlines();
        if self.stop_before_declaration(initial_separator, recovery) {
            return None;
        }
        let mut items = Vec::new();
        if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
            if !allow_empty {
                self.error_previous(PackageParseErrorKind::ExpectedToken(
                    "at least one list item".into(),
                ));
                return None;
            }
            return Some(items);
        }

        let mut closed = false;
        loop {
            if self.at_eof() {
                self.error_here(PackageParseErrorKind::ExpectedToken("`}`".into()));
                break;
            }
            let item_position = self.position;
            match parse_item(self) {
                Some(value) => items.push(Spanned {
                    value,
                    span: self.span_from(item_position),
                }),
                None => {
                    if self.recovered_declaration_boundary.is_some() {
                        return None;
                    }
                    if self.stop_before_declaration(self.position, recovery) {
                        return None;
                    }
                    if self.position == item_position {
                        self.bump();
                    }
                    self.recover_list_item(separator);
                }
            }

            if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                closed = true;
                break;
            }
            if self.at_newline() {
                let separator_position = self.position;
                self.skip_newlines();
                if self.stop_before_declaration(separator_position, recovery) {
                    break;
                }
                if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                    closed = true;
                    break;
                }
                continue;
            }
            if separator == ListSeparator::CommaOrLine
                && self.eat_punctuator(PackagePunctuator::Comma).is_some()
            {
                let separator_position = self.position;
                self.skip_newlines();
                if self.stop_before_declaration(separator_position, recovery) {
                    break;
                }
                if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                    closed = true;
                    break;
                }
                continue;
            }
            if self.at_eof() {
                self.error_here(PackageParseErrorKind::ExpectedToken("`}`".into()));
                break;
            }
            self.error_here(PackageParseErrorKind::ExpectedListSeparator);
            if self.stop_before_declaration(self.position, recovery) {
                break;
            }
            let mut separator_position = self.position;
            if self.eat_punctuator(PackagePunctuator::Comma).is_some() {
                separator_position = self.position;
                self.skip_newlines();
            }
            if self.stop_before_declaration(separator_position, recovery) {
                break;
            }
            self.recover_list_item(separator);
            if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                closed = true;
                break;
            }
            if self.at_newline() {
                let separator_position = self.position;
                self.skip_newlines();
                if self.stop_before_declaration(separator_position, recovery) {
                    break;
                }
                if self.eat_punctuator(PackagePunctuator::RightBrace).is_some() {
                    closed = true;
                    break;
                }
            } else if separator == ListSeparator::CommaOrLine {
                let _ = self.eat_punctuator(PackagePunctuator::Comma);
            }
        }

        if !closed || !allow_empty && items.is_empty() {
            None
        } else {
            Some(items)
        }
    }

    fn stop_before_declaration(
        &mut self,
        separator_position: usize,
        recovery: ListRecovery,
    ) -> bool {
        if !self.has_physical_line_break_before_current(separator_position)
            || !self.looks_like_declaration_start()
        {
            return false;
        }
        let current_word = self.token_text(self.position);
        if recovery == ListRecovery::ProfileItems && current_word == "allow" {
            return false;
        }
        if recovery == ListRecovery::ArtifactEntries
            && self.tokens.get(self.position + 1).is_some_and(|token| {
                token.kind == PackageTokenKind::String && is_target_name(current_word)
            })
        {
            return false;
        }
        let declaration_span = self.current().span.clone();
        self.position = separator_position;
        self.recovered_declaration_boundary = Some(separator_position);
        self.error(
            PackageParseErrorKind::ExpectedToken("`}`".into()),
            declaration_span,
        );
        true
    }

    fn has_physical_line_break_before_current(&self, separator_position: usize) -> bool {
        let Some(previous_token) = separator_position
            .checked_sub(1)
            .and_then(|index| self.tokens.get(index))
        else {
            return false;
        };
        let current_start = self.current().span.start;
        previous_token.span.end <= current_start
            && self.text[previous_token.span.end..current_start]
                .bytes()
                .any(|byte| matches!(byte, b'\n' | b'\r'))
    }

    fn looks_like_declaration_start(&self) -> bool {
        if !self.at_kind(PackageTokenKind::Word) {
            return false;
        }
        let next = self.tokens.get(self.position + 1);
        let next_is = |kind| next.is_some_and(|token| token.kind == kind);
        let next_word_is = |word: &str| {
            next.is_some_and(|token| {
                token.kind == PackageTokenKind::Word && &self.text[token.span.clone()] == word
            })
        };
        let punctuator = |offset: usize, expected| {
            self.tokens
                .get(self.position + offset)
                .is_some_and(|token| token.kind == PackageTokenKind::Punctuator(expected))
        };

        match self.token_text(self.position) {
            "compiler" => {
                next_is(PackageTokenKind::Integer)
                    || next_is(PackageTokenKind::Version)
                    || [
                        PackagePunctuator::Caret,
                        PackagePunctuator::Tilde,
                        PackagePunctuator::Equal,
                        PackagePunctuator::GreaterEqual,
                        PackagePunctuator::Greater,
                        PackagePunctuator::LessEqual,
                        PackagePunctuator::Less,
                    ]
                    .into_iter()
                    .any(|operator| next_is(PackageTokenKind::Punctuator(operator)))
            }
            "source" | "wit" | "description" | "license" | "repository" => {
                next_is(PackageTokenKind::String)
            }
            "artifact" => {
                (next_word_is("component") || next_is(PackageTokenKind::Word))
                    && punctuator(2, PackagePunctuator::LeftBrace)
            }
            "targets" | "allow" | "deny" | "policy" | "lints" | "overrides" | "authors" => {
                punctuator(1, PackagePunctuator::LeftBrace)
            }
            "export" => {
                next_word_is("world")
                    && self.tokens.get(self.position + 2).is_some_and(|token| {
                        matches!(
                            token.kind,
                            PackageTokenKind::Word | PackageTokenKind::PackagePath
                        )
                    })
            }
            "requires" => {
                if next_word_is("test") {
                    punctuator(2, PackagePunctuator::LeftBrace)
                } else {
                    punctuator(1, PackagePunctuator::LeftBrace)
                }
            }
            "profile" => {
                next_is(PackageTokenKind::Word) && punctuator(2, PackagePunctuator::LeftBrace)
            }
            "allocator" => next_is(PackageTokenKind::PackagePath),
            "package" => {
                next_is(PackageTokenKind::PackagePath)
                    && punctuator(2, PackagePunctuator::At)
                    && self
                        .tokens
                        .get(self.position + 3)
                        .is_some_and(|token| token.kind == PackageTokenKind::Version)
            }
            _ => false,
        }
    }

    fn recover_list_item(&mut self, separator: ListSeparator) {
        while !self.at_eof()
            && !self.at_punctuator(PackagePunctuator::RightBrace)
            && !self.at_newline()
            && !(separator == ListSeparator::CommaOrLine
                && self.at_punctuator(PackagePunctuator::Comma))
        {
            self.bump();
        }
    }

    fn parse_version_requirement(&mut self) -> Option<VersionReq> {
        let mut comparators = Vec::new();
        let mut operators = Vec::new();
        let mut operator_spans = Vec::new();
        let mut version_spans = Vec::new();
        let mut versions = Vec::new();
        let mut unsupported_operator_span = None;
        if matches!(
            self.current().kind,
            PackageTokenKind::Integer | PackageTokenKind::Version
        ) {
            let token = self.position;
            let spelling = self.token_text(token).to_owned();
            self.bump();
            let error = VersionReq::parse(&spelling)
                .expect_err("a bare version cannot be a version requirement");
            self.error(
                PackageParseErrorKind::InvalidVersionRequirement(error),
                self.tokens[token].span.clone(),
            );
            return None;
        }
        loop {
            let operator = match self.current_punctuator() {
                Some(PackagePunctuator::Caret) => "^",
                Some(PackagePunctuator::Tilde) => "~",
                Some(PackagePunctuator::Equal) => "=",
                Some(PackagePunctuator::GreaterEqual) => ">=",
                Some(PackagePunctuator::Greater) => ">",
                Some(PackagePunctuator::LessEqual) => "<=",
                Some(PackagePunctuator::Less) => "<",
                _ => {
                    self.error_here(PackageParseErrorKind::ExpectedToken(
                        "version comparator".into(),
                    ));
                    return None;
                }
            };
            if operator == ">" && unsupported_operator_span.is_none() {
                unsupported_operator_span = Some(self.current().span.clone());
            }
            operators.push(operator);
            operator_spans.push(self.current().span.clone());
            self.bump();
            if !matches!(
                self.current().kind,
                PackageTokenKind::Integer | PackageTokenKind::Version
            ) {
                self.error_here(PackageParseErrorKind::ExpectedToken(
                    "version after comparator".into(),
                ));
                return None;
            }
            version_spans.push(self.current().span.clone());
            let version = self.token_text(self.position).to_owned();
            versions.push(version.clone());
            self.bump();
            comparators.push(format!("{operator}{version}"));
            if self.eat_punctuator(PackagePunctuator::Comma).is_none() {
                break;
            }
        }
        let spelling = comparators.join(", ");
        match VersionReq::parse(&spelling) {
            Ok(requirement) => Some(requirement),
            Err(error) => {
                let span = version_requirement_error_span(
                    error,
                    &operators,
                    &versions,
                    &operator_spans,
                    &version_spans,
                    unsupported_operator_span,
                    self.current().span.clone(),
                );
                self.error(
                    PackageParseErrorKind::InvalidVersionRequirement(error),
                    span,
                );
                None
            }
        }
    }

    fn parse_capability(&mut self) -> Option<Box<str>> {
        match self.current().kind {
            PackageTokenKind::Word | PackageTokenKind::PackagePath => {
                let value = self.token_text(self.position).to_owned().into_boxed_str();
                self.bump();
                Some(value)
            }
            _ => {
                self.error_here(PackageParseErrorKind::ExpectedToken(
                    "capability name".into(),
                ));
                None
            }
        }
    }

    fn parse_package_path(&mut self) -> Option<Box<str>> {
        let token = self.expect_kind(PackageTokenKind::PackagePath, "package path")?;
        Some(self.token_text(token).into())
    }

    fn parse_word(&mut self) -> Option<Box<str>> {
        let token = self.expect_kind(PackageTokenKind::Word, "word")?;
        Some(self.token_text(token).into())
    }

    fn parse_string(&mut self) -> Option<Box<str>> {
        let token = self.expect_kind(PackageTokenKind::String, "string")?;
        if self.token_has_lexical_error(token) {
            return None;
        }
        self.tokens[token].string_value.clone()
    }

    fn parse_content_path(&mut self) -> Option<Box<str>> {
        let token = self.expect_kind(PackageTokenKind::String, "path string")?;
        if self.token_has_lexical_error(token) {
            return None;
        }
        let value = self.tokens[token].string_value.clone()?;
        if ContentPackagePath::parse(&value).is_err() {
            self.error(
                PackageParseErrorKind::InvalidContentPath(PackagePathError::InvalidPackagePath),
                self.tokens[token].span.clone(),
            );
            return None;
        }
        Some(value)
    }

    fn parse_dependency_path(&mut self) -> Option<Box<str>> {
        let token = self.expect_kind(PackageTokenKind::String, "path string")?;
        if self.token_has_lexical_error(token) {
            return None;
        }
        let value = self.tokens[token].string_value.clone()?;
        if DependencyPackagePath::parse(&value).is_err() {
            self.error(
                PackageParseErrorKind::InvalidDependencyPath(PackagePathError::InvalidPackagePath),
                self.tokens[token].span.clone(),
            );
            return None;
        }
        Some(value)
    }

    fn parse_repository_package_directory(&mut self) -> Option<Box<str>> {
        let token = self.expect_kind(PackageTokenKind::String, "repository directory")?;
        if self.token_has_lexical_error(token) {
            return None;
        }
        let value = self.tokens[token].string_value.clone()?;
        if RepositoryPackageDirectory::parse(&value).is_err() {
            self.error(
                PackageParseErrorKind::InvalidRepositoryDirectory(
                    PackagePathError::InvalidPackagePath,
                ),
                self.tokens[token].span.clone(),
            );
            return None;
        }
        Some(value)
    }

    fn token_has_lexical_error(&self, token: usize) -> bool {
        let token_span = &self.tokens[token].span;
        self.lexical_error_spans.iter().any(|error_span| {
            token_span.start < error_span.end && error_span.start < token_span.end
        })
    }

    fn expect_word(&mut self, word: &str) -> Option<usize> {
        if self.at_word(word) {
            let position = self.position;
            self.bump();
            Some(position)
        } else {
            self.error_here(PackageParseErrorKind::ExpectedToken(
                format!("`{word}`").into_boxed_str(),
            ));
            None
        }
    }

    fn expect_kind(&mut self, kind: PackageTokenKind, expected: &str) -> Option<usize> {
        if self.at_kind(kind) {
            let position = self.position;
            self.bump();
            Some(position)
        } else {
            self.error_here(PackageParseErrorKind::ExpectedToken(expected.into()));
            None
        }
    }

    fn expect_punctuator(
        &mut self,
        punctuator: PackagePunctuator,
        expected: &str,
    ) -> Option<usize> {
        self.eat_punctuator(punctuator).or_else(|| {
            self.error_here(PackageParseErrorKind::ExpectedToken(expected.into()));
            None
        })
    }

    fn at_word(&self, word: &str) -> bool {
        self.at_kind(PackageTokenKind::Word) && self.token_text(self.position) == word
    }

    fn current_word(&self) -> Option<&str> {
        (self.current().kind == PackageTokenKind::Word).then(|| self.token_text(self.position))
    }

    fn at_kind(&self, kind: PackageTokenKind) -> bool {
        self.current().kind == kind
    }

    fn at_punctuator(&self, punctuator: PackagePunctuator) -> bool {
        self.current().kind == PackageTokenKind::Punctuator(punctuator)
    }

    fn current_punctuator(&self) -> Option<PackagePunctuator> {
        match self.current().kind {
            PackageTokenKind::Punctuator(punctuator) => Some(punctuator),
            _ => None,
        }
    }

    fn eat_punctuator(&mut self, punctuator: PackagePunctuator) -> Option<usize> {
        if self.at_punctuator(punctuator) {
            let position = self.position;
            self.bump();
            Some(position)
        } else {
            None
        }
    }

    fn at_newline(&self) -> bool {
        self.at_kind(PackageTokenKind::Newline)
    }

    fn at_eof(&self) -> bool {
        self.at_kind(PackageTokenKind::Eof)
    }

    fn skip_newlines(&mut self) {
        while self.at_newline() {
            self.bump();
        }
    }

    fn current(&self) -> &PackageToken {
        &self.tokens[self.position.min(self.tokens.len() - 1)]
    }

    fn token_text(&self, position: usize) -> &str {
        let span = &self.tokens[position].span;
        &self.text[span.clone()]
    }

    fn span_from(&self, start_position: usize) -> Range<usize> {
        let start = self.tokens[start_position].span.start;
        let end = if self.position > start_position {
            self.tokens[self.position - 1].span.end
        } else {
            start
        };
        start..end
    }

    fn bump(&mut self) {
        if !self.at_eof() {
            self.position += 1;
        }
    }

    fn recover_to_newline(&mut self) {
        while !self.at_newline() && !self.at_eof() {
            self.bump();
        }
        self.skip_newlines();
    }

    fn error_here(&mut self, kind: PackageParseErrorKind) {
        let span = self.current().span.clone();
        self.error(kind, span);
    }

    fn error_previous(&mut self, kind: PackageParseErrorKind) {
        let span = if self.position > 0 {
            self.tokens[self.position - 1].span.clone()
        } else {
            self.current().span.clone()
        };
        self.error(kind, span);
    }

    fn error(&mut self, kind: PackageParseErrorKind, span: Range<usize>) {
        self.errors.push(PackageParseError { kind, span });
    }
}

fn version_requirement_error_span(
    error: VersionReqParseError,
    operators: &[&str],
    versions: &[String],
    operator_spans: &[Range<usize>],
    version_spans: &[Range<usize>],
    unsupported_operator_span: Option<Range<usize>>,
    fallback: Range<usize>,
) -> Range<usize> {
    let operator_span = |index: usize| {
        operator_spans
            .get(index)
            .cloned()
            .unwrap_or_else(|| fallback.clone())
    };
    let version_span = |index: usize| {
        version_spans
            .get(index)
            .cloned()
            .unwrap_or_else(|| fallback.clone())
    };

    match error {
        VersionReqParseError::UnsupportedVersionOperator => {
            unsupported_operator_span.unwrap_or(fallback)
        }
        VersionReqParseError::MissingLowerBound => operator_span(0),
        VersionReqParseError::InvalidVersion(_) => versions
            .iter()
            .position(|version| PartialVersion::parse(version).is_err())
            .map(version_span)
            .unwrap_or_else(|| version_span(versions.len().saturating_sub(1))),
        VersionReqParseError::InvalidRequirement => {
            // These constraints are invalid on their own, so point to the
            // offending written version even when another comparator follows.
            if let Some(index) = operators
                .iter()
                .zip(versions)
                .position(|(operator, version)| {
                    let Ok(partial) = PartialVersion::parse(version) else {
                        return false;
                    };
                    (*operator == "=" || *operator == "<=") && !partial.is_full()
                        || *operator == "^"
                            && partial.completed().major() == 0
                            && (partial.component_count() == 1
                                || partial.completed().minor() == 0
                                    && partial.component_count() == 2)
                })
            {
                return version_span(index);
            }

            let mut lower_seen = false;
            let mut upper_seen = false;
            for (index, operator) in operators.iter().enumerate() {
                let duplicate = match *operator {
                    "^" | "~" | "=" | ">=" => {
                        let duplicate = lower_seen;
                        lower_seen = true;
                        duplicate
                    }
                    "<" | "<=" => {
                        let duplicate = upper_seen;
                        upper_seen = true;
                        duplicate
                    }
                    _ => false,
                };
                if duplicate {
                    return operator_span(index);
                }
            }

            if let Some(index) = operators.iter().position(|operator| *operator == "=") {
                return operator_span((index + 1).min(operators.len().saturating_sub(1)));
            }
            operator_span(operators.len().saturating_sub(1))
        }
        VersionReqParseError::EmptyVersionRequirement => operators
            .iter()
            .rposition(|operator| matches!(*operator, "<" | "<="))
            .map(operator_span)
            .unwrap_or_else(|| operator_span(operators.len().saturating_sub(1))),
        VersionReqParseError::EmptyRequirement | VersionReqParseError::BareVersionRequirement => {
            version_span(versions.len().saturating_sub(1))
        }
    }
}

fn collect_list_diagnostics(
    file: &PackageFile,
    role: PackageRole,
    warnings: &mut Vec<PackageParseWarning>,
    errors: &mut Vec<PackageParseError>,
) {
    for declaration in &file.declarations {
        match &declaration.value {
            PackageDeclaration::Targets(items)
            | PackageDeclaration::Allow(items)
            | PackageDeclaration::Deny(items)
            | PackageDeclaration::Authors(items) => {
                add_duplicate_text_warnings(items, |value| value, warnings);
            }
            PackageDeclaration::Policy(items) => {
                add_duplicate_policy_warnings(items, role, warnings);
            }
            PackageDeclaration::Requires { test, entries }
                if !(*test && role == PackageRole::Dependency) =>
            {
                for entry in entries {
                    if let Some(only) = &entry.value.only {
                        add_duplicate_text_warnings(&only.value, |value| value, warnings);
                    }
                    if let Some(grant) = &entry.value.grant {
                        add_duplicate_text_warnings(&grant.value, |value| value, warnings);
                    }
                }
            }
            PackageDeclaration::ArtifactComponents(entries) => {
                let mut seen = HashSet::new();
                for entry in entries {
                    if !seen.insert(entry.value.target.as_ref()) {
                        errors.push(PackageParseError {
                            kind: PackageParseErrorKind::DuplicateDeclaration(
                                format!("artifact target {}", entry.value.target).into_boxed_str(),
                            ),
                            span: entry.span.clone(),
                        });
                    }
                }
            }
            PackageDeclaration::Lints(items) if role == PackageRole::Root => {
                let mut seen = HashMap::<&str, (LintLevel, Range<usize>)>::new();
                for item in items {
                    let code = item.value.code.as_ref();
                    if let Some((level, _)) = seen.get(code) {
                        if *level == item.value.level {
                            warnings.push(PackageParseWarning {
                                kind: PackageParseWarningKind::DuplicateListItem,
                                span: item.span.clone(),
                            });
                        } else {
                            errors.push(PackageParseError {
                                kind: PackageParseErrorKind::ConflictingLintLevel,
                                span: item.span.clone(),
                            });
                        }
                    } else {
                        seen.insert(code, (item.value.level, item.span.clone()));
                    }
                }
            }
            PackageDeclaration::Profile { items, .. } if role == PackageRole::Root => {
                for item in items {
                    match &item.value {
                        ProfileItem::Allow(capabilities) => {
                            add_duplicate_text_warnings(capabilities, |value| value, warnings);
                        }
                        ProfileItem::Budget(budgets) => {
                            add_duplicate_text_warnings(
                                budgets,
                                |value| value.metric.as_ref(),
                                warnings,
                            );
                        }
                    }
                }
            }
            PackageDeclaration::Compiler(_)
            | PackageDeclaration::Source(_)
            | PackageDeclaration::Wit(_)
            | PackageDeclaration::ExportWorld(_)
            | PackageDeclaration::Requires { .. }
            | PackageDeclaration::Lints(_)
            | PackageDeclaration::Profile { .. }
            | PackageDeclaration::Allocator(_)
            | PackageDeclaration::Overrides(_)
            | PackageDeclaration::Description(_)
            | PackageDeclaration::License(_)
            | PackageDeclaration::Repository(_) => {}
        }
    }
}

fn add_duplicate_text_warnings<'a, T>(
    items: &'a [Spanned<T>],
    text: impl Fn(&'a T) -> &'a str,
    warnings: &mut Vec<PackageParseWarning>,
) {
    let mut seen = HashSet::new();
    for item in items {
        if !seen.insert(text(&item.value)) {
            warnings.push(PackageParseWarning {
                kind: PackageParseWarningKind::DuplicateListItem,
                span: item.span.clone(),
            });
        }
    }
}

fn add_duplicate_policy_warnings(
    items: &[Spanned<Box<str>>],
    role: PackageRole,
    warnings: &mut Vec<PackageParseWarning>,
) {
    let mut seen = HashSet::new();
    for item in items {
        let value = item.value.as_ref();
        if role == PackageRole::Dependency && value == "duplicate_lines" {
            continue;
        }
        if !seen.insert(value) {
            warnings.push(PackageParseWarning {
                kind: PackageParseWarningKind::DuplicateListItem,
                span: item.span.clone(),
            });
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ListSeparator {
    LinesOnly,
    CommaOrLine,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ListRecovery {
    Declarations,
    ArtifactEntries,
    ProfileItems,
}

pub(crate) fn is_target_name(name: &str) -> bool {
    matches!(
        name,
        "wasm32-core"
            | "wasm64-core"
            | "wasm32-wasi"
            | "wasm64-wasi"
            | "x86_64-linux"
            | "aarch64-linux"
            | "aarch64-macos"
            | "x86_64-windows"
    )
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum DeclarationKey {
    Compiler,
    Source,
    Wit,
    Artifact,
    Targets,
    ExportWorld,
    Requires(bool),
    Allow,
    Deny,
    Policy,
    Lints,
    Profile(Box<str>),
    Allocator,
    Overrides,
    Description,
    License,
    Repository,
    Authors,
}

impl DeclarationKey {
    fn is_root_only(&self) -> bool {
        matches!(
            self,
            Self::Requires(true)
                | Self::Lints
                | Self::Profile(_)
                | Self::Allocator
                | Self::Overrides
        )
    }

    fn name(&self) -> Box<str> {
        match self {
            Self::Compiler => "compiler".into(),
            Self::Source => "source".into(),
            Self::Wit => "wit".into(),
            Self::Artifact => "artifact".into(),
            Self::Targets => "targets".into(),
            Self::ExportWorld => "export world".into(),
            Self::Requires(false) => "requires".into(),
            Self::Requires(true) => "requires test".into(),
            Self::Allow => "allow".into(),
            Self::Deny => "deny".into(),
            Self::Policy => "policy".into(),
            Self::Lints => "lints".into(),
            Self::Profile(name) => format!("profile {name}").into_boxed_str(),
            Self::Allocator => "allocator".into(),
            Self::Overrides => "overrides".into(),
            Self::Description => "description".into(),
            Self::License => "license".into(),
            Self::Repository => "repository".into(),
            Self::Authors => "authors".into(),
        }
    }
}

fn declaration_suggestion(name: &str) -> Option<Box<str>> {
    match name {
        "dependencies" => return Some("requires".into()),
        "name" | "version" => return Some("package".into()),
        "edition" => return Some("compiler".into()),
        _ => {}
    }
    const DECLARATIONS: &[&str] = &[
        "allocator",
        "allow",
        "artifact",
        "authors",
        "compiler",
        "description",
        "deny",
        "export",
        "license",
        "lints",
        "overrides",
        "package",
        "policy",
        "profile",
        "repository",
        "requires",
        "source",
        "targets",
        "wit",
    ];
    if name.len() > 64 {
        return None;
    }
    DECLARATIONS
        .iter()
        .map(|candidate| (*candidate, edit_distance(name, candidate)))
        .filter(|(_, distance)| *distance <= 3)
        .min_by_key(|(_, distance)| *distance)
        .map(|(candidate, _)| candidate.into())
}

fn edit_distance(left: &str, right: &str) -> usize {
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    for (left_index, left_byte) in left.bytes().enumerate() {
        let mut current = Vec::with_capacity(right.len() + 1);
        current.push(left_index + 1);
        for (right_index, right_byte) in right.bytes().enumerate() {
            let substitution = previous[right_index] + usize::from(left_byte != right_byte);
            current.push(
                (previous[right_index + 1] + 1)
                    .min(current[right_index] + 1)
                    .min(substitution),
            );
        }
        previous = current;
    }
    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use super::{
        LintLevel, MAX_PACKAGE_MANIFEST_BYTES, OverrideSource, PackageDeclaration,
        PackageIdentityError, PackageParseErrorKind, PackageRole, ProfileItem, RequirementSource,
        parse, parse_for_role,
    };

    fn parse_text(source: &str) -> super::PackageParseOutput {
        parse(source.as_bytes())
    }

    fn parse_valid(source: &str) -> super::PackageParseOutput {
        let output = parse_text(source);
        assert!(
            output.is_syntax_valid(),
            "unexpected diagnostics: {:?} / {:?}",
            output.lexical_errors,
            output.errors
        );
        output
    }

    #[test]
    fn package_parse_output_equality_ignores_exact_byte_provenance() {
        let first = parse_for_role(&[0xff], PackageRole::Root);
        let second = parse_for_role(&[0xfe], PackageRole::Root);

        assert_eq!(first, second);
        assert_ne!(first.manifest_digest(), second.manifest_digest());
    }

    #[test]
    fn oversized_manifests_fail_before_lexing_and_return_the_requested_role() {
        let source = vec![0xff; MAX_PACKAGE_MANIFEST_BYTES + 1];
        let parsed = parse_for_role(&source, PackageRole::Dependency);

        assert_eq!(parsed.role(), PackageRole::Dependency);
        assert!(parsed.lexical_errors().is_empty());
        assert_eq!(parsed.errors().len(), 1);
        assert_eq!(
            parsed.errors()[0].kind,
            PackageParseErrorKind::ManifestTooLarge
        );
        assert_eq!(parsed.errors()[0].span, 0..0);
        assert!(!parsed.is_syntax_valid());
        assert!(parsed.manifest_digest().is_none());
    }

    #[test]
    fn dependency_parsing_suppresses_diagnostics_for_ignored_root_only_declarations() {
        let source = "package acme:dep @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core }\n\
             requires test { acme:ignored @ ^1 from path \"../ignored\" }\n\
             requires test { acme:ignored @ ^1 from path \"../ignored\" }\n\
             lints { allow NAMING_CONVENTION, deny NAMING_CONVENTION }\n\
             lints { allow UNKNOWN_LINT }\n\
             profile tiny {\n\
                 allow { host, host }\n\
                 allow { threads }\n\
                 budget { wasm <= 1 B, wasm <= 2 B }\n\
             }\n\
             profile tiny { allow { host } }\n\
             allocator acme:alloc\n\
             allocator acme:other-alloc\n\
             overrides { acme:other use artifact }\n\
             overrides { acme:another use artifact }\n";
        let dependency = parse_for_role(source.as_bytes(), PackageRole::Dependency);
        assert!(dependency.is_syntax_valid(), "{:?}", dependency.errors);
        assert!(dependency.warnings.is_empty(), "{:?}", dependency.warnings);
        assert!(
            dependency
                .file
                .declarations
                .iter()
                .filter(|declaration| matches!(
                    declaration.value,
                    PackageDeclaration::Profile { .. }
                ))
                .count()
                == 2
        );

        let root = parse_text(source);
        assert!(
            root.errors.iter().any(|error| matches!(
                error.kind,
                PackageParseErrorKind::DuplicateDeclaration(_)
                    | PackageParseErrorKind::ConflictingLintLevel
            )),
            "{:?}",
            root.errors
        );
        assert!(!root.warnings.is_empty());
    }

    #[test]
    fn dependency_policy_suppresses_only_root_only_item_duplicates() {
        let source = "package acme:dep @ 1.0.0\n\
             source \"src\"\n\
             targets { wasm32-core }\n\
             policy { deny duplicate_lines, deny duplicate_lines, deny unsafe, deny unsafe }\n";
        let dependency = parse_for_role(source.as_bytes(), PackageRole::Dependency);
        assert_eq!(dependency.warnings.len(), 1);
        assert_eq!(&source[dependency.warnings[0].span.clone()], "deny unsafe");

        let root = parse_text(source);
        assert_eq!(root.warnings.len(), 2);
    }

    #[test]
    fn parses_header_simple_declarations_and_preserves_spans() {
        let source = "\u{feff}// contract\npackage acme:image @ 1.2.3\ncompiler ^0.1\nsource \"src\"\nwit \"wit\"\ntargets { wasm32-core, wasm32-wasi }\nexport world api/client\n";
        let output = parse_valid(source);
        let header = output.file.header.expect("header");
        assert_eq!(header.value.identity.to_string(), "acme:image");
        assert_eq!(header.value.version.to_string(), "1.2.3");
        assert_eq!(&source[header.span], "package acme:image @ 1.2.3");
        assert_eq!(output.file.declarations.len(), 5);
        assert!(matches!(
            output.file.declarations[0].value,
            PackageDeclaration::Compiler(_)
        ));
        assert!(matches!(
            output.file.declarations[3].value,
            PackageDeclaration::Targets(_)
        ));
        assert!(matches!(
            &output.file.declarations[4].value,
            PackageDeclaration::ExportWorld(name) if name.as_ref() == "api/client"
        ));
        assert!(!output.file.trivia.is_empty());
    }

    #[test]
    fn parses_requirements_and_continued_clauses() {
        let output = parse_valid(
            "package acme:app @ 1.0.0\n\
             requires {\n\
                 acme:http @ >=1.2, <2.0\n\
                     from git \"https://example.test/repo\" dir \"http\"\n\
                     only { wasm32-wasi, wasm64-wasi }\n\
                     grant { alloc, wasi:filesystem/types }\n\
                 acme:local @ ^0.3\n\
                     from path \"../local\"\n\
             }\n",
        );
        let PackageDeclaration::Requires { entries, .. } = &output.file.declarations[0].value
        else {
            panic!("expected requirements");
        };
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].value.version.to_string(), ">=1.2, <2.0");
        assert!(matches!(
            entries[0].value.source.as_ref().map(|value| &value.value),
            Some(RequirementSource::Git { directory: Some(directory), .. })
                if directory.as_ref() == "http"
        ));
        assert_eq!(entries[0].value.only.as_ref().unwrap().value.len(), 2);
        assert_eq!(entries[0].value.grant.as_ref().unwrap().value.len(), 2);
        assert!(matches!(
            entries[1].value.source.as_ref().map(|value| &value.value),
            Some(RequirementSource::Path(path)) if path.as_ref() == "../local"
        ));
    }

    #[test]
    fn git_repository_directories_may_be_under_tool_state_or_output_names() {
        let output = parse_valid(
            "package acme:app @ 1.0.0\n\
             requires {\n\
                 acme:output @ ^1 from git \"https://example.test/repo\" dir \"out/sibling\"\n\
             }\n\
             overrides {\n\
                 acme:state use git \"https://example.test/repo\" dir \".sai/sibling\"\n\
             }\n",
        );
        let PackageDeclaration::Requires { entries, .. } = &output.file.declarations[0].value
        else {
            panic!("expected requirements");
        };
        assert!(matches!(
            entries[0].value.source.as_ref().map(|value| &value.value),
            Some(RequirementSource::Git { directory: Some(directory), .. })
                if directory.as_ref() == "out/sibling"
        ));
        let PackageDeclaration::Overrides(overrides) = &output.file.declarations[1].value else {
            panic!("expected overrides");
        };
        assert!(matches!(
            &overrides[0].value.source,
            OverrideSource::Git { directory: Some(directory), .. }
                if directory.as_ref() == ".sai/sibling"
        ));
    }

    #[test]
    fn honors_newline_generation_and_contextual_keywords() {
        let output = parse_valid(
            "// leading comment\n\
             package acme:app @ 1.0.0\n\
             compiler >=1.2,\n\
                 <2.0 /* continuation starts in comment\n\
                 and ends here */\n\
             targets {\n\
                 test,\n\
                 allow\n\
             }\n\
             export world test/allow\n\
             requires {\n\
                 acme:thing @ ^1\n\
                     from git \"https://example.test/thing\"\n\
                     dir \"sub\"\n\
                     only { test, allow }\n\
                     grant { alloc }\n\
             }\n",
        );
        let PackageDeclaration::Targets(targets) = &output.file.declarations[1].value else {
            panic!("expected targets");
        };
        assert_eq!(targets[0].value.as_ref(), "test");
        assert_eq!(targets[1].value.as_ref(), "allow");
        let PackageDeclaration::Requires { entries, .. } = &output.file.declarations[3].value
        else {
            panic!("expected requirements");
        };
        assert!(entries[0].value.only.is_some());
        assert!(entries[0].value.grant.is_some());
    }

    #[test]
    fn rejects_commas_between_long_entries() {
        let output = parse_text(
            "package acme:app @ 1.0.0\n\
             requires {\n\
                 acme:first @ ^1 from git \"https://example.test/first\",\n\
                 acme:second @ ^1 from git \"https://example.test/second\"\n\
             }\n\
             overrides {\n\
                 acme:first use artifact,\n\
                 acme:second use artifact\n\
             }\n",
        );
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.kind == PackageParseErrorKind::ExpectedListSeparator)
                .count(),
            2,
            "{:#?}",
            output.errors
        );
    }

    #[test]
    fn parses_all_block_declaration_forms() {
        let output = parse_valid(
            "package acme:app @ 1.0.0\n\
             artifact component { wasm32-wasi \"dist/app.wasm\" }\n\
             allow { alloc, wasi:cli }\n\
             deny { wasi:filesystem }\n\
             policy { deny unsafe, deny ambient_imports }\n\
             lints { deny NAMING_CONVENTION, warn CONTRACT_RUNTIME_CHECK }\n\
             profile tiny {\n\
                 allow { alloc }\n\
                 budget { wasm <= 16 KiB }\n\
             }\n\
             allocator acme:allocator\n\
             overrides {\n\
                 acme:one use artifact\n\
                 acme:two use path \"../two\"\n\
                 acme:three use git \"https://example.test/three\" dir \"sub\"\n\
             }\n\
             description \"Image\\ncodec\"\n\
             license \"MIT OR Apache-2.0\"\n\
             authors { \"Ada\", \"Grace\" }\n\
             repository \"https://example.test/project\"\n",
        );
        assert_eq!(output.file.declarations.len(), 12);
        assert!(matches!(
            output.file.declarations[0].value,
            PackageDeclaration::ArtifactComponents(_)
        ));
        assert!(matches!(
            output.file.declarations[5].value,
            PackageDeclaration::Profile { .. }
        ));
        let PackageDeclaration::Profile { items, .. } = &output.file.declarations[5].value else {
            panic!("expected profile");
        };
        assert!(matches!(items[0].value, ProfileItem::Allow(_)));
        let ProfileItem::Budget(budgets) = &items[1].value else {
            panic!("expected budget");
        };
        assert_eq!(budgets[0].value.amount.as_ref(), "16");
        let PackageDeclaration::Overrides(overrides) = &output.file.declarations[7].value else {
            panic!("expected overrides");
        };
        assert!(matches!(
            overrides[0].value.source,
            OverrideSource::Artifact
        ));
        assert!(matches!(
            overrides[2].value.source,
            OverrideSource::Git {
                directory: Some(_),
                ..
            }
        ));
        assert!(matches!(
            output.file.declarations[8].value,
            PackageDeclaration::Description(_)
        ));
    }

    #[test]
    fn accepts_empty_optional_lists_and_separate_test_requirements() {
        let output = parse_valid(
            "package acme:app @ 1.0.0\n\
             requires {}\n\
             requires test {\n\
                 acme:test-helper @ ^1 from git \"https://example.test/test\" grant {}\n\
             }\n\
             allow {}\n\
             profile dev { budget {} }\n\
             authors {}\n",
        );
        let PackageDeclaration::Requires {
            test: false,
            entries,
        } = &output.file.declarations[0].value
        else {
            panic!("expected production requirements");
        };
        assert!(entries.is_empty());
        assert!(matches!(
            &output.file.declarations[1].value,
            PackageDeclaration::Requires { test: true, entries }
                if entries.len() == 1
                    && entries[0].value.grant.as_ref().is_some_and(|grant| grant.value.is_empty())
        ));
    }

    #[test]
    fn retains_empty_targets_for_semantic_validation_and_reports_trailing_tokens() {
        let output = parse_text(
            "package acme:app @ 1.0.0\n\
             targets {}\n\
             source \"src\" trailing\n",
        );
        assert!(output.file.declarations.iter().any(|declaration| matches!(
            &declaration.value,
            PackageDeclaration::Targets(items) if items.is_empty()
        )));
        assert!(!output.errors.iter().any(|error| matches!(
            error.kind,
            PackageParseErrorKind::ExpectedToken(ref expected)
                if expected.as_ref() == "at least one list item"
        )));
        assert!(output.errors.iter().any(|error| {
            error.kind
                == PackageParseErrorKind::ExpectedToken("newline between declarations".into())
        }));
    }

    #[test]
    fn emits_duplicate_list_warnings_and_conflicting_lint_errors() {
        let source = "package acme:app @ 1.0.0\n\
             targets { wasm32-core, wasm32-core }\n\
             allow { alloc, alloc }\n\
             policy { deny unsafe, deny unsafe }\n\
             lints { allow NAMING_CONVENTION, allow NAMING_CONVENTION, warn NAMING_CONVENTION }\n\
             requires {\n\
                 acme:dep @ ^1 from git \"https://example.test/dep\"\n\
                     only { wasm32-core, wasm32-core }\n\
                     grant { alloc, alloc }\n\
             }\n\
             profile dev {\n\
                 allow { alloc, alloc }\n\
                 budget { wasm <= 1 B, wasm <= 2 B }\n\
             }\n\
             authors { \"Ada\", \"Ada\" }\n";
        let output = parse_text(source);
        assert_eq!(output.warnings.len(), 9);
        assert!(output.warnings.iter().all(|warning| matches!(
            warning.kind,
            super::PackageParseWarningKind::DuplicateListItem
        )));
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.kind == PackageParseErrorKind::ConflictingLintLevel)
                .count(),
            1
        );
        assert_eq!(&source[output.warnings[0].span.clone()], "wasm32-core");
        assert_eq!(&source[output.warnings[1].span.clone()], "alloc");
    }

    #[test]
    fn omits_unknown_artifact_kinds_from_the_syntax_tree() {
        let output = parse_text(
            "package acme:app @ 1.0.0\n\
             artifact module { wasm32-wasi \"dist/app.wasm\" }\n\
             source \"src\"\n",
        );
        assert!(
            output
                .errors
                .iter()
                .any(|error| matches!(error.kind, PackageParseErrorKind::UnknownArtifactKind(_)))
        );
        assert!(output.file.declarations.iter().all(|declaration| !matches!(
            declaration.value,
            PackageDeclaration::ArtifactComponents(_)
        )));
        assert!(
            output
                .file
                .declarations
                .iter()
                .any(|declaration| matches!(declaration.value, PackageDeclaration::Source(_)))
        );
    }

    #[test]
    fn duplicate_profile_blocks_point_to_the_second_block() {
        let source = "package acme:app @ 1.0.0\nprofile dev {\n allow {}\n allow {}\n}\n";
        let output = parse_text(source);
        let duplicate = output
            .errors
            .iter()
            .find(|error| {
                matches!(
                    &error.kind,
                    PackageParseErrorKind::DuplicateDeclaration(name) if name.as_ref() == "allow"
                )
            })
            .expect("duplicate profile item");
        assert_eq!(&source[duplicate.span.clone()], "allow {}");
    }

    #[test]
    fn rejects_duplicate_artifact_targets_as_duplicate_declarations() {
        let output = parse_text(
            "package acme:app @ 1.0.0\n\
             artifact component {\n\
                 wasm32-wasi \"dist/first.wasm\"\n\
                 wasm32-wasi \"dist/second.wasm\"\n\
             }\n",
        );
        assert!(output.errors.iter().any(|error| matches!(
            &error.kind,
            PackageParseErrorKind::DuplicateDeclaration(name)
                if name.as_ref() == "artifact target wasm32-wasi"
        )));
    }

    #[test]
    fn reports_missing_late_and_duplicate_package_declarations() {
        let missing = parse_text("source \"src\"\n");
        assert!(
            missing.errors.iter().any(|error| matches!(
                error.kind,
                PackageParseErrorKind::MissingPackageDeclaration
            ))
        );

        let late = parse_text("targets { wasm32-core }\npackage acme:app @ 1.0.0\n");
        assert!(late.errors.iter().any(|error| matches!(
            error.kind,
            PackageParseErrorKind::PackageDeclarationNotFirst
        )));

        let duplicate = parse_text("package acme:app @ 1.0.0\npackage acme:app @ 1.0.0\n");
        assert!(
            duplicate
                .errors
                .iter()
                .any(|error| matches!(error.kind, PackageParseErrorKind::DuplicateDeclaration(_)))
        );
    }

    #[test]
    fn distinguishes_requirement_declarations_and_rejects_other_duplicates() {
        let output = parse_text(
            "package acme:app @ 1.0.0\n\
             requires {}\n\
             requires test {}\n\
             compiler ^1\n\
             compiler ^2\n\
             profile dev {}\n\
             profile dev {}\n",
        );
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| matches!(
                    error.kind,
                    PackageParseErrorKind::DuplicateDeclaration(_)
                ))
                .count(),
            2
        );
    }

    #[test]
    fn tracks_duplicate_declarations_even_when_the_first_value_is_invalid() {
        let output = parse_text(
            "package acme:app @ 1.0.0\n\
             source \"../outside\"\n\
             source \"src\"\n",
        );
        assert!(
            output
                .errors
                .iter()
                .any(|error| matches!(error.kind, PackageParseErrorKind::InvalidContentPath(_)))
        );
        assert!(output.errors.iter().any(|error| matches!(
            &error.kind,
            PackageParseErrorKind::DuplicateDeclaration(name) if name.as_ref() == "source"
        )));
        assert_eq!(
            output
                .file
                .declarations
                .iter()
                .filter(|declaration| matches!(declaration.value, PackageDeclaration::Source(_)))
                .count(),
            1
        );
    }

    #[test]
    fn reports_unknown_declarations_with_suggestions() {
        let output = parse_text("package acme:app @ 1.0.0\ndependencies {}\n");
        assert!(output.errors.iter().any(|error| matches!(
            &error.kind,
            PackageParseErrorKind::UnknownDeclaration { name, suggestion }
                if name.as_ref() == "dependencies"
                    && suggestion.as_deref() == Some("requires")
        )));
    }

    #[test]
    fn rejects_malformed_identity_version_and_requirement_values() {
        let output = parse_text(
            "package wasi:app @ 1.0\n\
             compiler ^0\n",
        );
        assert!(output.errors.iter().any(|error| matches!(
            error.kind,
            PackageParseErrorKind::InvalidPackageIdentity(PackageIdentityError::ReservedNamespace)
        )));
        assert!(
            output
                .errors
                .iter()
                .any(|error| matches!(error.kind, PackageParseErrorKind::InvalidPackageVersion(_)))
        );
        assert!(output.errors.iter().any(|error| matches!(
            error.kind,
            PackageParseErrorKind::InvalidVersionRequirement(_)
        )));
    }

    #[test]
    fn reports_bare_versions_and_unsupported_greater_than() {
        let source = "package acme:app @ 1.0.0\n\
             compiler 1.2\n\
             compiler >1.2\n";
        let output = parse_text(source);
        assert!(output.errors.iter().any(|error| matches!(
            error.kind,
            PackageParseErrorKind::InvalidVersionRequirement(
                crate::version_req::VersionReqParseError::BareVersionRequirement
            )
        )));
        assert!(output.errors.iter().any(|error| {
            matches!(
                error.kind,
                PackageParseErrorKind::InvalidVersionRequirement(
                    crate::version_req::VersionReqParseError::UnsupportedVersionOperator
                )
            ) && &source[error.span.clone()] == ">"
        }));
    }

    #[test]
    fn conflicting_comparators_point_to_the_later_operator() {
        let source = "package acme:app @ 1.0.0\ncompiler >=1, ^2\n";
        let output = parse_text(source);
        let invalid = output
            .errors
            .iter()
            .find(|error| {
                matches!(
                    error.kind,
                    PackageParseErrorKind::InvalidVersionRequirement(
                        crate::version_req::VersionReqParseError::InvalidRequirement
                    )
                )
            })
            .expect("conflicting comparator diagnostic");
        assert_eq!(&source[invalid.span.clone()], "^");
    }

    #[test]
    fn partial_upper_comparator_points_to_its_version() {
        let source = "package acme:app @ 1.0.0\ncompiler <=1.0, >=2.0\n";
        let output = parse_text(source);
        let invalid = output
            .errors
            .iter()
            .find(|error| {
                matches!(
                    error.kind,
                    PackageParseErrorKind::InvalidVersionRequirement(
                        crate::version_req::VersionReqParseError::InvalidRequirement
                    )
                )
            })
            .expect("invalid partial upper comparator diagnostic");
        assert_eq!(&source[invalid.span.clone()], "1.0");
    }

    #[test]
    fn declarations_without_opening_braces_are_omitted_during_recovery() {
        let output = parse_text(
            "package acme:app @ 1.0.0\n\
             targets\n\
             allow\n\
             authors\n\
             source \"src\"\n\
             description \"retained\"\n",
        );
        assert!(!output.file.declarations.iter().any(|declaration| matches!(
            declaration.value,
            PackageDeclaration::Targets(_)
                | PackageDeclaration::Allow(_)
                | PackageDeclaration::Authors(_)
        )));
        assert!(output.file.declarations.iter().any(|declaration| matches!(
            &declaration.value,
            PackageDeclaration::Source(path) if path.as_ref() == "src"
        )));
        assert!(output.file.declarations.iter().any(|declaration| matches!(
            &declaration.value,
            PackageDeclaration::Description(text) if text.as_ref() == "retained"
        )));
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| matches!(
                    error.kind,
                    PackageParseErrorKind::ExpectedToken(ref token) if token.as_ref() == "`{`"
                ))
                .count(),
            3
        );
    }

    #[test]
    fn validates_decoded_paths_in_their_manifest_context() {
        let source = "package acme:app @ 1.0.0\n\
             source \"..\\u{2f}outside\"\n\
             requires {\n\
                 acme:local @ ^1\n\
                     from path \"..\\u{2f}outside\"\n\
             }\n";
        let output = parse_text(source);
        assert!(
            output
                .errors
                .iter()
                .any(|error| matches!(error.kind, PackageParseErrorKind::InvalidContentPath(_)))
        );
        assert!(
            !output
                .errors
                .iter()
                .any(|error| matches!(error.kind, PackageParseErrorKind::InvalidDependencyPath(_)))
        );
        let source_error = output
            .errors
            .iter()
            .find(|error| matches!(error.kind, PackageParseErrorKind::InvalidContentPath(_)))
            .expect("source traversal diagnostic");
        assert_eq!(&source[source_error.span.clone()], "\"..\\u{2f}outside\"");
        assert!(
            output
                .file
                .declarations
                .iter()
                .all(|declaration| !matches!(declaration.value, PackageDeclaration::Source(_)))
        );
    }

    #[test]
    fn recovers_after_malformed_declarations_and_lists() {
        let output = parse_text(
            "package acme:app @ 1.0.0\n\
             targets { wasm32-core wasm32-wasi }\n\
             source \"src\" trailing\n\
             wit \"wit\"\n",
        );
        assert!(output.errors.len() >= 2);
        assert!(
            output
                .file
                .declarations
                .iter()
                .any(|declaration| { matches!(declaration.value, PackageDeclaration::Source(_)) })
        );
        assert!(
            output
                .file
                .declarations
                .iter()
                .any(|declaration| { matches!(declaration.value, PackageDeclaration::Wit(_)) })
        );
    }

    #[test]
    fn list_recovery_keeps_items_after_a_bad_short_list_item() {
        let output = parse_text(
            "package acme:app @ 1.0.0\n\
             targets {\n\
                 wasm32-core\n\
                 123\n\
                 wasm64-core\n\
             }\n",
        );
        let PackageDeclaration::Targets(targets) = &output.file.declarations[0].value else {
            panic!("expected targets");
        };
        assert_eq!(targets.len(), 2);
        assert_eq!(targets[1].value.as_ref(), "wasm64-core");
    }

    #[test]
    fn unclosed_target_list_recovers_at_a_later_declaration() {
        let source = "package acme:app @ 1.0.0\n\
             targets {\n\
                 wasm32-core\n\
             source \"src\"\n";
        let output = parse_text(source);
        assert!(
            !output
                .file
                .declarations
                .iter()
                .any(|declaration| matches!(declaration.value, PackageDeclaration::Targets(_)))
        );
        assert!(output.file.declarations.iter().any(|declaration| matches!(
            &declaration.value,
            PackageDeclaration::Source(path) if path.as_ref() == "src"
        )));
        assert!(output.errors.iter().any(|error| matches!(
            error.kind,
            PackageParseErrorKind::ExpectedToken(ref token) if token.as_ref() == "`}`"
        )));
        assert_eq!(
            &source[output
                .errors
                .iter()
                .find(|error| matches!(
                    error.kind,
                    PackageParseErrorKind::ExpectedToken(ref token)
                        if token.as_ref() == "`}`"
                ))
                .unwrap()
                .span
                .clone()],
            "source"
        );
    }

    #[test]
    fn unclosed_nested_lists_propagate_recovery_to_top_level() {
        for body in [
            "allow {\n alloc\nsource \"src\"\n",
            "requires {\n acme:dep @ ^1\nsource \"src\"\n",
            "profile dev {\n budget { wasm <= 1 B\nsource \"src\"\n",
        ] {
            let source = format!("package acme:app @ 1.0.0\n{body}");
            let output = parse_text(&source);
            assert!(
                output.file.declarations.iter().any(|declaration| matches!(
                    &declaration.value,
                    PackageDeclaration::Source(path) if path.as_ref() == "src"
                )),
                "failed to recover after {body:?}: {:#?}",
                output.errors
            );
            assert!(!output.file.declarations.iter().any(|declaration| matches!(
                declaration.value,
                PackageDeclaration::Allow(_)
                    | PackageDeclaration::Requires { .. }
                    | PackageDeclaration::Profile { .. }
            )));
            assert!(output.errors.iter().any(|error| matches!(
                error.kind,
                PackageParseErrorKind::ExpectedToken(ref token) if token.as_ref() == "`}`"
            )));
        }
    }

    #[test]
    fn recovery_uses_physical_newlines_suppressed_after_braces_and_commas() {
        for body in [
            "requires {\nsource \"src\"\n",
            "allow {\nalloc,\nsource \"src\"\n",
            "requires {\n acme:dep @\nsource \"src\"\n",
            "profile dev {\n allow { alloc },\nsource \"src\"\n",
        ] {
            let source = format!("package acme:app @ 1.0.0\n{body}");
            let output = parse_text(&source);
            assert!(
                output.file.declarations.iter().any(|declaration| matches!(
                    &declaration.value,
                    PackageDeclaration::Source(path) if path.as_ref() == "src"
                )),
                "failed to recover after {body:?}: {:#?}",
                output.errors
            );
        }
    }

    #[test]
    fn recovery_respects_artifact_entries_and_profile_items() {
        let artifact_source = "package acme:app @ 1.0.0\n\
             artifact component {\n\
                 wasm32-wasi \"dist/app.wasm\"\n\
             targets { wasm32-core }\n";
        let artifact_output = parse_text(artifact_source);
        assert!(
            artifact_output
                .file
                .declarations
                .iter()
                .any(|declaration| { matches!(declaration.value, PackageDeclaration::Targets(_)) })
        );
        assert!(
            !artifact_output.file.declarations.iter().any(|declaration| {
                matches!(declaration.value, PackageDeclaration::ArtifactComponents(_))
            })
        );

        let profile_source = "package acme:app @ 1.0.0\n\
             profile dev {\n\
                 allow { alloc }\n\
             source \"src\"\n";
        let profile_output = parse_text(profile_source);
        assert!(
            profile_output
                .file
                .declarations
                .iter()
                .any(|declaration| matches!(
                    &declaration.value,
                    PackageDeclaration::Source(path) if path.as_ref() == "src"
                ))
        );
        assert!(!profile_output.file.declarations.iter().any(|declaration| {
            matches!(declaration.value, PackageDeclaration::Profile { .. })
        }));
    }

    #[test]
    fn overflowing_budget_integer_is_retained_for_size_validation() {
        let source = "package acme:app @ 1.0.0\n\
             profile dev { budget { wasm <= 18446744073709551616 B } }\n";
        let output = parse_text(source);
        let PackageDeclaration::Profile { items, .. } = &output.file.declarations[0].value else {
            panic!("expected profile");
        };
        let ProfileItem::Budget(budgets) = &items[0].value else {
            panic!("expected budget");
        };
        assert_eq!(budgets[0].value.amount.as_ref(), "18446744073709551616");
        assert!(output.lexical_errors.iter().any(|error| {
            error.kind == crate::lexer::PackageLexErrorKind::InvalidVersion
                && &source[error.span.clone()] == "18446744073709551616"
        }));
    }

    #[test]
    fn reports_one_error_for_a_stray_top_level_closing_brace() {
        let output = parse_text("package acme:app @ 1.0.0\n}\n");
        assert_eq!(
            output
                .errors
                .iter()
                .filter(|error| error.kind == PackageParseErrorKind::ExpectedDeclaration)
                .count(),
            1
        );
    }

    #[test]
    fn retains_lexical_diagnostics_and_stops_after_invalid_utf8() {
        let output = parse(b"package acme:app @ 1.0.0\ndescription \"bad\\q\"\n");
        assert!(!output.lexical_errors.is_empty());
        assert!(!output.is_syntax_valid());
        assert!(
            output.file.declarations.iter().all(|declaration| !matches!(
                declaration.value,
                PackageDeclaration::Description(_)
            ))
        );

        let invalid_utf8 = parse(b"package acme:app @ 1.0.0\n\xff");
        assert_eq!(invalid_utf8.lexical_errors.len(), 1);
        assert!(invalid_utf8.errors.is_empty());
    }

    #[test]
    fn parses_lint_levels_and_escaped_metadata() {
        let output = parse_valid(
            "package acme:app @ 1.0.0\n\
             lints { allow PUBLIC_EFFECTS_INFERRED, warn CONTRACT_RUNTIME_CHECK, deny NAMING_CONVENTION }\n\
             description \"line\\nnext\"\n",
        );
        let PackageDeclaration::Lints(items) = &output.file.declarations[0].value else {
            panic!("expected lints");
        };
        assert_eq!(items[0].value.level, LintLevel::Allow);
        assert_eq!(items[1].value.level, LintLevel::Warn);
        assert_eq!(items[2].value.level, LintLevel::Deny);
        let PackageDeclaration::Description(description) = &output.file.declarations[1].value
        else {
            panic!("expected description");
        };
        assert_eq!(description.as_ref(), "line\nnext");
    }
}
