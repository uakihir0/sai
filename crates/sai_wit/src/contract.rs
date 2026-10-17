//! Stable, owned projection of the package contract fields consumed by WIT.

use sai_package::{
    identity::PackageIdentity,
    manifest::{PackageDeclaration, PackageFile, PackageParseErrorKind, PackageParseOutput},
    package_path::{ContentPackagePath, PackagePathError},
    version::Version,
};
use sai_stable_hash::{StableHash, StableHasher};
use std::ops::Range;

/// The content form declared by a package contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WitPackageKind {
    Source,
    Artifact,
    Hybrid,
}

/// A package's declared export world and its location in `sai.package`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitContractExportWorld {
    value: Box<str>,
    span: Range<usize>,
}

impl WitContractExportWorld {
    /// The declared local or external world spelling.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// The half-open byte range of the `export world` value's source tokens.
    ///
    /// A local path range may include whitespace or comments between path
    /// segments, so the corresponding source slice need not equal [`Self::value`].
    pub fn span(&self) -> &Range<usize> {
        &self.span
    }
}

/// One content declaration that cannot occur more than once in the WIT
/// contract projection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WitContractField {
    PackageHeader,
    Source,
    Artifact,
    Wit,
    ExportWorld,
}

/// A malformed or incomplete package-file projection used by WIT queries.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitContractError {
    /// The parser did not retain a valid package header.
    MissingHeader,
    /// The package header was present but its identity or version was invalid.
    MalformedHeader { span: Range<usize> },
    /// Neither `source` nor `artifact` was retained in the parsed package.
    MissingContentKind,
    /// A content path in the AST did not satisfy package-path validation.
    InvalidContentPath {
        field: WitContractField,
        span: Range<usize>,
        error: PackagePathError,
    },
    /// A declaration that must be unique appeared more than once.
    DuplicateDeclaration {
        field: WitContractField,
        first_span: Range<usize>,
        duplicate_span: Range<usize>,
    },
    /// A parser-reported malformed declaration cannot be projected
    /// losslessly from the recovered AST.
    MalformedDeclaration { span: Range<usize> },
    /// The parser found errors that make the recovered AST unsafe to project,
    /// but its bounded diagnostic list did not retain a more specific error.
    MalformedManifest {
        lexical_error_count: usize,
        parser_error_count: usize,
    },
}

/// The owned package-contract fields needed to construct WIT units and views.
///
/// The WIT directory and export world remain optional even for artifact and
/// hybrid packages. Their required-field diagnostics belong to package
/// validation, where the original manifest context is available.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitContract {
    identity: PackageIdentity,
    version: Version,
    kind: WitPackageKind,
    wit_directory: Option<ContentPackagePath>,
    export_world: Option<WitContractExportWorld>,
}

impl WitContract {
    /// Projects the WIT fields from parser output, retaining errors that
    /// cannot be represented by `PackageFile` alone, such as a duplicate
    /// package header.
    ///
    /// Relevant retained parser errors are surfaced specifically. Any other
    /// parser or lexer error rejects the recovered manifest with a generic
    /// error and counts, so callers cannot accidentally consume a partial AST.
    pub fn from_package_parse_output(
        output: &PackageParseOutput,
    ) -> Result<Self, WitContractError> {
        for error in output.errors() {
            match &error.kind {
                PackageParseErrorKind::MissingPackageDeclaration => {
                    return Err(WitContractError::MissingHeader);
                }
                PackageParseErrorKind::InvalidPackageIdentity(_)
                | PackageParseErrorKind::InvalidPackageVersion(_) => {
                    return Err(WitContractError::MalformedHeader {
                        span: error.span.clone(),
                    });
                }
                PackageParseErrorKind::DuplicateDeclaration(name) if name.as_ref() == "package" => {
                    let first_header_span = output
                        .file()
                        .header
                        .as_ref()
                        .filter(|header| header.span.start < error.span.start)
                        .map(|header| header.span.clone());
                    return Err(match first_header_span {
                        Some(first_span) => WitContractError::DuplicateDeclaration {
                            field: WitContractField::PackageHeader,
                            first_span,
                            duplicate_span: error.span.clone(),
                        },
                        // Recovery can retain a later valid header while the
                        // first header was malformed and discarded. Do not
                        // claim that the retained span identifies the first
                        // declaration in that case.
                        None => WitContractError::MalformedDeclaration {
                            span: error.span.clone(),
                        },
                    });
                }
                PackageParseErrorKind::InvalidContentPath(_)
                | PackageParseErrorKind::UnknownArtifactKind(_) => {
                    return Err(WitContractError::MalformedDeclaration {
                        span: error.span.clone(),
                    });
                }
                _ => {}
            }
        }
        let has_parse_errors = output.lexical_error_count() != 0 || output.error_count() != 0;
        let projection = Self::from_package_file(output.file());
        if has_parse_errors {
            match projection {
                // Duplicate fields retained in the AST and invalid paths
                // retained in manually constructed AST values are concrete
                // contract errors even when the parser's detail list is
                // bounded.
                Err(error @ WitContractError::DuplicateDeclaration { .. })
                | Err(error @ WitContractError::InvalidContentPath { .. }) => {
                    return Err(error);
                }
                _ => {
                    return Err(WitContractError::MalformedManifest {
                        lexical_error_count: output.lexical_error_count(),
                        parser_error_count: output.error_count(),
                    });
                }
            }
        }
        projection
    }

    /// Projects the fields consumed by WIT from a parsed package AST.
    ///
    /// This AST-only entry point can reject missing structure, duplicate
    /// retained fields, and invalid retained content paths. Use
    /// [`Self::from_package_parse_output`] when parser diagnostics are
    /// available so malformed or duplicate headers omitted from the AST are
    /// also rejected.
    pub fn from_package_file(file: &PackageFile) -> Result<Self, WitContractError> {
        let header = file
            .header
            .as_ref()
            .ok_or(WitContractError::MissingHeader)?;

        let mut source_span = None;
        let mut artifact_span = None;
        let mut wit_span = None;
        let mut export_world_span = None;
        let mut wit_directory = None;
        let mut export_world = None;

        for declaration in &file.declarations {
            match &declaration.value {
                PackageDeclaration::Source(path) => {
                    claim_field(
                        WitContractField::Source,
                        &mut source_span,
                        &declaration.span,
                    )?;
                    ContentPackagePath::parse(path).map_err(|error| {
                        WitContractError::InvalidContentPath {
                            field: WitContractField::Source,
                            span: declaration.span.clone(),
                            error,
                        }
                    })?;
                }
                PackageDeclaration::ArtifactComponents(_) => {
                    claim_field(
                        WitContractField::Artifact,
                        &mut artifact_span,
                        &declaration.span,
                    )?;
                }
                PackageDeclaration::Wit(path) => {
                    claim_field(WitContractField::Wit, &mut wit_span, &declaration.span)?;
                    let path = ContentPackagePath::parse(path).map_err(|error| {
                        WitContractError::InvalidContentPath {
                            field: WitContractField::Wit,
                            span: declaration.span.clone(),
                            error,
                        }
                    })?;
                    wit_directory = Some(path);
                }
                PackageDeclaration::ExportWorld(value) => {
                    claim_field(
                        WitContractField::ExportWorld,
                        &mut export_world_span,
                        &declaration.span,
                    )?;
                    export_world = Some(WitContractExportWorld {
                        value: value.value.clone(),
                        span: value.span.clone(),
                    });
                }
                _ => {}
            }
        }

        let kind = match (source_span.is_some(), artifact_span.is_some()) {
            (true, true) => WitPackageKind::Hybrid,
            (true, false) => WitPackageKind::Source,
            (false, true) => WitPackageKind::Artifact,
            (false, false) => return Err(WitContractError::MissingContentKind),
        };

        Ok(Self {
            identity: header.value.identity.clone(),
            version: header.value.version.clone(),
            kind,
            wit_directory,
            export_world,
        })
    }

    pub fn identity(&self) -> &PackageIdentity {
        &self.identity
    }

    pub fn version(&self) -> &Version {
        &self.version
    }

    pub const fn kind(&self) -> WitPackageKind {
        self.kind
    }

    pub fn wit_directory(&self) -> Option<&str> {
        self.wit_directory.as_ref().map(ContentPackagePath::as_str)
    }

    pub fn export_world(&self) -> Option<&WitContractExportWorld> {
        self.export_world.as_ref()
    }
}

fn claim_field(
    field: WitContractField,
    first_span: &mut Option<Range<usize>>,
    span: &Range<usize>,
) -> Result<(), WitContractError> {
    if let Some(first_span) = first_span {
        return Err(WitContractError::DuplicateDeclaration {
            field,
            first_span: first_span.clone(),
            duplicate_span: span.clone(),
        });
    }
    *first_span = Some(span.clone());
    Ok(())
}

impl StableHash for WitContract {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD0);
        self.identity.to_string().stable_hash(hasher);
        self.version.to_string().stable_hash(hasher);
        hasher.write_tag(match self.kind {
            WitPackageKind::Source => 0x01,
            WitPackageKind::Artifact => 0x02,
            WitPackageKind::Hybrid => 0x03,
        });
        match &self.wit_directory {
            Some(directory) => {
                hasher.write_tag(0x01);
                directory.as_str().stable_hash(hasher);
            }
            None => hasher.write_tag(0x00),
        }
        match &self.export_world {
            Some(export_world) => {
                hasher.write_tag(0x01);
                export_world.value().stable_hash(hasher);
                stable_byte_offset(export_world.span.start, hasher);
                stable_byte_offset(export_world.span.end, hasher);
            }
            None => hasher.write_tag(0x00),
        }
    }
}

fn stable_byte_offset(offset: usize, hasher: &mut StableHasher) {
    hasher.write_tag(0x04);
    hasher.write_u64(u64::try_from(offset).unwrap_or(u64::MAX));
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_package::manifest::{
        PackageDeclaration, PackageRole, Spanned, parse, parse_for_role_with_error_limit,
    };
    use sai_stable_hash::fingerprint;

    fn project(source: &str) -> Result<WitContract, WitContractError> {
        let parsed = parse(source.as_bytes());
        WitContract::from_package_parse_output(&parsed)
    }

    #[test]
    fn projects_identity_version_and_each_content_kind() {
        let source =
            project("package acme:codec@1.2.3-alpha.1\nsource \"src\"\n").expect("source contract");
        assert_eq!(source.identity().to_string(), "acme:codec");
        assert_eq!(source.version().to_string(), "1.2.3-alpha.1");
        assert_eq!(source.kind(), WitPackageKind::Source);

        let artifact = project(
            "package acme:codec@1.2.3\nartifact component { wasm32-wasi \"dist/codec.wasm\" }\n",
        )
        .expect("artifact contract");
        assert_eq!(artifact.kind(), WitPackageKind::Artifact);

        let hybrid = project(
            "package acme:codec@1.2.3\nsource \"src\"\nartifact component { wasm32-wasi \"dist/codec.wasm\" }\n",
        )
        .expect("hybrid contract");
        assert_eq!(hybrid.kind(), WitPackageKind::Hybrid);
    }

    #[test]
    fn preserves_optional_wit_and_export_world_for_every_kind() {
        for source in [
            "package acme:source@1.0.0\nsource \"src\"\n",
            "package acme:artifact@1.0.0\nartifact component {}\n",
            "package acme:hybrid@1.0.0\nsource \"src\"\nartifact component {}\n",
        ] {
            let contract = project(source).expect("contract with omitted optional fields");
            assert_eq!(contract.wit_directory(), None);
            assert_eq!(contract.export_world(), None);
        }

        let contract = project(
            "package acme:codec@1.0.0\nsource \"src\"\nwit \"interface\"\nexport world app\n",
        )
        .expect("contract with WIT fields");
        assert_eq!(contract.wit_directory(), Some("interface"));
        let world = contract.export_world().expect("world declaration");
        assert_eq!(world.value(), "app");
        let start = "package acme:codec@1.0.0\nsource \"src\"\nwit \"interface\"\n".len();
        assert_eq!(
            world.span(),
            &(start + "export world ".len()..start + "export world app".len())
        );
    }

    #[test]
    fn export_world_projection_preserves_raw_value_span_after_utf8_trivia() {
        let source = "\u{feff}// 日本語 🙂\n\
            package acme:codec@1.0.0\n\
            source \"src\"\n\
            export world api /* path note 🧭 */ / server\n";
        let contract = project(source).expect("contract with a spaced local world path");
        let world = contract.export_world().expect("export world declaration");

        assert_eq!(world.value(), "api/server");
        assert_eq!(
            &source[world.span().clone()],
            "api /* path note 🧭 */ / server"
        );
    }

    #[test]
    fn rejects_missing_header_and_content_kind() {
        let parsed = parse(b"");
        assert_eq!(
            WitContract::from_package_parse_output(&parsed),
            Err(WitContractError::MissingHeader)
        );

        assert_eq!(
            project("package acme:codec@1.0.0\nwit \"interface\"\n"),
            Err(WitContractError::MissingContentKind)
        );
    }

    #[test]
    fn rejects_malformed_and_duplicate_headers() {
        let malformed = parse(b"package wasi:codec@1.0.0\nsource \"src\"\n");
        assert!(matches!(
            WitContract::from_package_parse_output(&malformed),
            Err(WitContractError::MalformedHeader { .. })
        ));

        let duplicate =
            parse(b"package acme:codec@1.0.0\npackage acme:codec@2.0.0\nsource \"src\"\n");
        assert!(matches!(
            WitContract::from_package_parse_output(&duplicate),
            Err(WitContractError::DuplicateDeclaration {
                field: WitContractField::PackageHeader,
                ..
            })
        ));

        let malformed_first =
            parse(b"package acme:codec\npackage acme:codec@2.0.0\nsource \"src\"\n");
        let error = WitContract::from_package_parse_output(&malformed_first)
            .expect_err("the malformed first header prevents projection");
        assert!(
            !matches!(
                error,
                WitContractError::DuplicateDeclaration {
                    field: WitContractField::PackageHeader,
                    ..
                }
            ),
            "recovery must not claim a first-header span when it retained only the later header"
        );
    }

    #[test]
    fn rejects_parser_reported_malformed_content_declarations() {
        for source in [
            "package acme:codec@1.0.0\nsource \"../src\"\n",
            "package acme:codec@1.0.0\nsource \"src\"\nwit \"../interface\"\n",
            "package acme:codec@1.0.0\nartifact module {}\n",
        ] {
            assert!(
                matches!(
                    project(source),
                    Err(WitContractError::MalformedDeclaration { .. })
                ),
                "malformed declaration should fail projection: {source:?}"
            );
        }
    }

    #[test]
    fn bounded_parse_without_retained_errors_fails_closed_with_counts() {
        let source = b"package acme:codec@1.0.0\nsource \"../src\"\n";
        let parsed = parse_for_role_with_error_limit(source, PackageRole::Root, 0);
        assert!(parsed.errors().is_empty());
        assert_eq!(parsed.error_count(), 1);
        assert!(matches!(
            WitContract::from_package_parse_output(&parsed),
            Err(WitContractError::MalformedManifest {
                lexical_error_count: 0,
                parser_error_count: 1,
            })
        ));
    }

    #[test]
    fn rejects_duplicate_projected_declarations_with_both_spans() {
        let cases = [
            (
                "package acme:codec@1.0.0\nsource \"src\"\nsource \"lib\"\n",
                WitContractField::Source,
            ),
            (
                "package acme:codec@1.0.0\nartifact component {}\nartifact component {}\n",
                WitContractField::Artifact,
            ),
            (
                "package acme:codec@1.0.0\nsource \"src\"\nwit \"one\"\nwit \"two\"\n",
                WitContractField::Wit,
            ),
            (
                "package acme:codec@1.0.0\nsource \"src\"\nexport world one\nexport world two\n",
                WitContractField::ExportWorld,
            ),
        ];

        for (source, expected_field) in cases {
            let parsed = parse(source.as_bytes());
            let error =
                WitContract::from_package_file(parsed.file()).expect_err("duplicate rejected");
            assert!(
                matches!(
                    error,
                    WitContractError::DuplicateDeclaration { field, .. }
                        if field == expected_field
                ),
                "unexpected error for {source:?}: {error:?}"
            );
        }
    }

    #[test]
    fn validates_retained_content_paths() {
        let file = PackageFile {
            header: Some(Spanned {
                value: sai_package::manifest::PackageHeader {
                    identity: PackageIdentity::parse("acme:codec").unwrap(),
                    version: Version::parse("1.0.0").unwrap(),
                },
                span: 0..25,
            }),
            declarations: vec![Spanned {
                value: PackageDeclaration::Wit("../outside".into()),
                span: 26..46,
            }],
            trivia: Vec::new(),
        };
        assert!(matches!(
            WitContract::from_package_file(&file),
            Err(WitContractError::InvalidContentPath {
                field: WitContractField::Wit,
                ..
            })
        ));
    }

    #[test]
    fn stable_hash_is_deterministic_and_includes_export_world_span() {
        let source =
            project("package acme:codec@1.0.0\nsource \"src\"\nexport world app\n").unwrap();
        let same = project("package acme:codec@1.0.0\nsource \"src\"\nexport world app\n").unwrap();
        let moved =
            project("package acme:codec@1.0.0\nsource \"src\"\n\nexport world app\n").unwrap();

        assert_eq!(fingerprint(&source), fingerprint(&same));
        assert_ne!(fingerprint(&source), fingerprint(&moved));
    }
}
