//! Hook contract and parser input for WIT generated from SAI source.
//!
//! `sai_wit` owns this boundary so it can load generated WIT without depending
//! on the crate that computes it (`sai_ty`).

use crate::WitSourceFile;
use sai_hir::{DefId, PackageId};
use sai_stable_hash::{StableHash, StableHasher};
use std::sync::Arc;

/// The synthetic direct-child filename used for generated WIT source.
///
/// Generated WIT is canonicalized into one text snapshot, so it has one stable
/// source file regardless of the package or the order of its original files.
pub const GENERATED_WIT_FILENAME: &str = "generated.wit";

/// The generated WIT text and the source declarations represented by it.
///
/// Equality and the standard `Hash` implementation compare all three fields
/// by value. The cross-session [`StableHash`] omits session-local `DefId`
/// identities but retains origin names. Origins still participate in equality
/// because a view can need rebuilding when declarations retain identical WIT
/// spelling but map to different SAI definitions.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct GeneratedText {
    /// Canonical WIT text for one source package.
    pub text: Arc<str>,
    /// Whether generation omitted no item.
    pub complete: bool,
    /// WIT interface name to originating SAI interface, in WIT order.
    pub origins: Arc<[(String, DefId)]>,
}

impl GeneratedText {
    /// Package the snapshot as an in-memory WIT source input.
    ///
    /// The generated snapshot is a single canonical document. Its parser path
    /// is always [`GENERATED_WIT_FILENAME`], making source-map identities
    /// deterministic across recomputations. Origin and completeness metadata
    /// travel with the source files for downstream view construction.
    pub fn source_input(&self) -> GeneratedWitSourceInput {
        let file = WitSourceFile::new(GENERATED_WIT_FILENAME, self.text.as_bytes().to_vec());
        GeneratedWitSourceInput {
            files: Arc::from([file]),
            complete: self.complete,
            origins: Arc::clone(&self.origins),
        }
    }
}

impl StableHash for GeneratedText {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE8);
        self.text.stable_hash(hasher);
        self.complete.stable_hash(hasher);

        // DefIds are session-local. Their value equality still participates
        // in query early cutoff, but only the stable WIT interface names are
        // included in this cross-session fingerprint. The resulting hash may
        // collide for different session-local origins; query values are also
        // compared by Eq before an old result is reused.
        hasher.write_len(u64::try_from(self.origins.len()).unwrap_or(u64::MAX));
        for (name, _) in self.origins.iter() {
            name.stable_hash(hasher);
        }
    }
}

/// In-memory parser files and metadata for a generated WIT snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GeneratedWitSourceInput {
    files: Arc<[WitSourceFile]>,
    complete: bool,
    origins: Arc<[(String, DefId)]>,
}

impl GeneratedWitSourceInput {
    /// The generated WIT files, in deterministic parser order.
    pub fn files(&self) -> &[WitSourceFile] {
        &self.files
    }

    /// Whether generation omitted no item.
    pub fn is_complete(&self) -> bool {
        self.complete
    }

    /// WIT interface name to originating SAI interface, in WIT order.
    pub fn origins(&self) -> &[(String, DefId)] {
        &self.origins
    }

    /// Return all parts for a parser and view-building caller.
    pub fn into_parts(self) -> (Arc<[WitSourceFile]>, bool, Arc<[(String, DefId)]>) {
        (self.files, self.complete, self.origins)
    }
}

/// Supplies generated WIT snapshots to `sai_wit` without coupling it to
/// `sai_ty` or the driver.
pub trait WitHooks {
    /// Return the generated WIT text for `package`.
    fn generated_wit_text(&self, package: PackageId) -> Arc<GeneratedText>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_hir::IndexVec;
    use sai_intern::{DefIndex, PackageInterner};
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    fn sample_origin() -> DefId {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:graphics#1.2").expect("package id");
        let mut definitions = IndexVec::<DefIndex, ()>::new();
        let index = definitions.push(()).expect("definition index");
        DefId::from_foreign(package, index)
    }

    fn hash(value: &GeneratedText) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn generated_text_uses_value_equality_and_hashing() {
        let origin = sample_origin();
        let left = GeneratedText {
            text: Arc::from("package acme:graphics@1.2.3;\n"),
            complete: true,
            origins: Arc::from([("canvas".to_owned(), origin)]),
        };
        let equal = GeneratedText {
            text: Arc::from(String::from("package acme:graphics@1.2.3;\n")),
            complete: true,
            origins: Arc::from([("canvas".to_owned(), origin)]),
        };

        assert_eq!(left, equal);
        assert_eq!(hash(&left), hash(&equal));

        let incomplete = GeneratedText {
            complete: false,
            ..equal.clone()
        };
        assert_ne!(left, incomplete);
    }

    #[test]
    fn generated_text_stable_hash_excludes_session_local_origin_ids() {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:graphics#1").expect("package id");
        let mut definitions = IndexVec::<DefIndex, ()>::new();
        let first_index = definitions.push(()).expect("definition index");
        let second_index = definitions.push(()).expect("definition index");
        let first_origin = DefId::from_foreign(package, first_index);
        let second_origin = DefId::from_foreign(package, second_index);
        let first = GeneratedText {
            text: Arc::from("package acme:graphics@1.2.3;\ninterface canvas {}\n"),
            complete: true,
            origins: Arc::from([("canvas".to_owned(), first_origin)]),
        };
        let second = GeneratedText {
            origins: Arc::from([("canvas".to_owned(), second_origin)]),
            ..first.clone()
        };

        assert_ne!(first, second);
        assert_eq!(
            sai_stable_hash::fingerprint(&first),
            sai_stable_hash::fingerprint(&second),
            "session-local DefIds must not enter a stable fingerprint"
        );
    }

    #[test]
    fn source_input_has_stable_filename_and_retains_metadata() {
        let origin = sample_origin();
        let text = GeneratedText {
            text: Arc::from("package acme:graphics@1.2.3;\ninterface canvas {}\n"),
            complete: false,
            origins: Arc::from([("canvas".to_owned(), origin)]),
        };

        let input = text.source_input();
        assert_eq!(input.files().len(), 1);
        assert_eq!(input.files()[0].relative_path, GENERATED_WIT_FILENAME);
        assert_eq!(input.files()[0].contents, text.text.as_bytes().to_vec());
        assert!(!input.is_complete());
        assert_eq!(input.origins(), text.origins.as_ref());

        let second_input = text.source_input();
        assert_eq!(
            input.files()[0].relative_path,
            second_input.files()[0].relative_path
        );
        assert_eq!(input, second_input);
    }

    #[test]
    fn source_input_parses_as_one_virtual_generated_package_file() {
        use sai_package::mvs::MvsPackageVersion;
        use sai_package::version::Version;

        let packages = PackageInterner::new();
        let selected = MvsPackageVersion::new(
            "acme:graphics",
            Version::parse("1.2.3").expect("valid package version"),
        )
        .expect("valid selected package version");
        let package = packages
            .intern("acme:graphics#1")
            .expect("valid generated package key");
        let unit = crate::WitUnit::generated(package, &packages, &selected)
            .expect("generated unit matches selected package");
        let text = GeneratedText {
            text: Arc::from("package acme:graphics@1.2.3;\ninterface canvas {}\n"),
            complete: true,
            origins: Arc::from([]),
        };
        let input = text.source_input();
        let parsed = crate::parse_source_files(unit, input.files().iter().cloned())
            .expect("generated text is accepted by the in-memory parser");

        assert_eq!(parsed.source_names().collect::<Vec<_>>(), ["generated.wit"]);
        assert_eq!(parsed.main_name().to_string(), "acme:graphics@1.2.3");
    }
}
