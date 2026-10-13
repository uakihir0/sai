//! Classify a parsed use path as local or foreign, then resolve local modules.
//!
//! Package selection, interfaces, dependencies, WIT, and import bindings are
//! intentionally outside this adapter.

use super::module_target::{ModuleTargetError, resolve_module_target};
use super::use_path::{ParsedUsePath, UsePathError, parse_use_path};
use super::{ModuleId, ModuleTree};
use sai_hir::item_tree::UseEntry;
use sai_intern::{Interner, PackageIdError, PackageInterner};
use sai_vfs::PackageKey;

/// A use target after comparing its package identity with the current package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UseTarget {
    /// An exact module target in the current SAI package.
    LocalModule(ModuleId),
    /// A path belonging to another package, preserved for package selection.
    ForeignPackage(ParsedUsePath),
}

/// An error while parsing a use path or resolving a local module target.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum UseTargetError {
    Path(UsePathError),
    Module(ModuleTargetError),
    PackageInterner(PackageIdError),
    PackageKeyMismatch {
        tree_package_key: String,
        supplied_package_key: String,
    },
    UnsupportedPackageKey(String),
}

impl From<UsePathError> for UseTargetError {
    fn from(error: UsePathError) -> Self {
        Self::Path(error)
    }
}

impl From<ModuleTargetError> for UseTargetError {
    fn from(error: ModuleTargetError) -> Self {
        Self::Module(error)
    }
}

impl From<PackageIdError> for UseTargetError {
    fn from(error: PackageIdError) -> Self {
        Self::PackageInterner(error)
    }
}

/// Resolves the package-local module case of a HIR `use` target.
///
/// `current_package_key` must be the canonical key for `tree.package` in
/// `packages`. This function verifies that relationship, then derives the
/// versionless `namespace:package` identity from an ordinary SAI key. The
/// implicit `sai:core` and `sai:alloc` keys are also accepted; WIT keys are
/// rejected because they do not name SAI module trees. Paths with another
/// identity are returned as `ForeignPackage` for a later package-selection
/// stage.
pub fn resolve_use_target(
    entry: &UseEntry,
    current_package_key: &PackageKey,
    packages: &PackageInterner,
    current_module: ModuleId,
    tree: &ModuleTree,
    symbols: &Interner,
) -> Result<UseTarget, UseTargetError> {
    let tree_package_key = packages.resolve(tree.package)?;
    if tree_package_key.as_ref() != current_package_key.as_str() {
        return Err(UseTargetError::PackageKeyMismatch {
            tree_package_key: tree_package_key.to_string(),
            supplied_package_key: current_package_key.as_str().to_owned(),
        });
    }
    let current_package_identity = versionless_sai_identity(current_package_key)?;

    let parsed = parse_use_path(entry, symbols)?;
    if parsed.package_identity != current_package_identity {
        return Ok(UseTarget::ForeignPackage(parsed));
    }

    let module = resolve_module_target(tree, current_module, &parsed.segments, symbols)?;
    Ok(UseTarget::LocalModule(module))
}

fn versionless_sai_identity(package_key: &PackageKey) -> Result<&str, UseTargetError> {
    let spelling = package_key.as_str();
    if matches!(spelling, "sai:core" | "sai:alloc") {
        return Ok(spelling);
    }
    if spelling.starts_with("wit:") {
        return Err(UseTargetError::UnsupportedPackageKey(spelling.to_owned()));
    }
    spelling
        .split_once('#')
        .map(|(identity, _)| identity)
        .ok_or_else(|| UseTargetError::UnsupportedPackageKey(spelling.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChildEntry, ModuleData};
    use sai_hir::item_tree::{ErasedAstId, UseItem};
    use sai_intern::{PackageInterner, Symbol};
    use sai_vfs::{FileId, PackageKey};
    use std::collections::BTreeMap;

    fn entry(pkg_path: &str) -> UseEntry {
        UseEntry {
            ast_id: ErasedAstId::new(0),
            is_pub: false,
            pkg_path: pkg_path.to_owned(),
            alias: None,
            has_items: false,
            items: Vec::<UseItem>::new(),
            raw_attrs: Vec::new(),
        }
    }

    fn symbol(interner: &Interner, name: &str) -> Symbol {
        interner.intern(name).expect("symbol is interned")
    }

    fn tree(interner: &Interner, packages: &PackageInterner, package_key: &str) -> ModuleTree {
        let package = packages
            .intern(package_key)
            .expect("package key is interned");
        ModuleTree {
            package,
            modules: vec![
                ModuleData {
                    path: Vec::new(),
                    file: FileId::new(0),
                    parent_dir: None,
                    children: BTreeMap::from([(
                        "nested".to_owned(),
                        ChildEntry::Module(ModuleId::from_raw(1)),
                    )]),
                },
                ModuleData {
                    path: vec![symbol(interner, "nested")],
                    file: FileId::new(1),
                    parent_dir: Some(ModuleId::ROOT),
                    children: BTreeMap::new(),
                },
            ],
            namespaces: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    fn setup(symbols: &Interner, package_key: &str) -> (ModuleTree, PackageInterner, PackageKey) {
        let packages = PackageInterner::new();
        let tree = tree(symbols, &packages, package_key);
        let key = PackageKey::new(package_key).expect("package key is valid");
        (tree, packages, key)
    }

    fn segment_spellings(path: &ParsedUsePath, symbols: &Interner) -> Vec<String> {
        path.segments
            .iter()
            .map(|segment| symbols.resolve(*segment).unwrap().to_string())
            .collect()
    }

    #[test]
    fn resolves_a_local_root_target() {
        let symbols = Interner::new();
        let (tree, packages, current_key) = setup(&symbols, "acme:app#0.1");

        assert_eq!(
            resolve_use_target(
                &entry("acme:app"),
                &current_key,
                &packages,
                ModuleId::from_raw(1),
                &tree,
                &symbols,
            ),
            Ok(UseTarget::LocalModule(ModuleId::ROOT))
        );
    }

    #[test]
    fn resolves_a_nested_local_module_target() {
        let symbols = Interner::new();
        let (tree, packages, current_key) = setup(&symbols, "acme:app#0.1");

        assert_eq!(
            resolve_use_target(
                &entry("acme:app/nested"),
                &current_key,
                &packages,
                ModuleId::ROOT,
                &tree,
                &symbols,
            ),
            Ok(UseTarget::LocalModule(ModuleId::from_raw(1)))
        );
    }

    #[test]
    fn preserves_foreign_identity_and_suffix() {
        let symbols = Interner::new();
        let (tree, packages, current_key) = setup(&symbols, "acme:app#0.1");

        let target = resolve_use_target(
            &entry("acme:other/some-module/child"),
            &current_key,
            &packages,
            ModuleId::ROOT,
            &tree,
            &symbols,
        )
        .unwrap();

        let UseTarget::ForeignPackage(path) = target else {
            panic!("a different package identity must remain foreign");
        };
        assert_eq!(path.package_identity, "acme:other");
        assert_eq!(
            segment_spellings(&path, &symbols),
            ["some-module".to_owned(), "child".to_owned()]
        );
    }

    #[test]
    fn reports_a_use_of_the_current_module_as_a_module_error() {
        let symbols = Interner::new();
        let (tree, packages, current_key) = setup(&symbols, "acme:app#0.1");

        assert_eq!(
            resolve_use_target(
                &entry("acme:app/nested"),
                &current_key,
                &packages,
                ModuleId::from_raw(1),
                &tree,
                &symbols,
            ),
            Err(UseTargetError::Module(
                ModuleTargetError::UseOfCurrentModule(ModuleId::from_raw(1))
            ))
        );
    }

    #[test]
    fn preserves_unknown_local_paths_as_module_errors() {
        let symbols = Interner::new();
        let (tree, packages, current_key) = setup(&symbols, "acme:app#0.1");

        assert_eq!(
            resolve_use_target(
                &entry("acme:app/missing"),
                &current_key,
                &packages,
                ModuleId::ROOT,
                &tree,
                &symbols,
            ),
            Err(UseTargetError::Module(ModuleTargetError::UnknownModule {
                matched_prefix: Vec::new(),
                module_candidates: vec!["nested".to_owned()],
            }))
        );
    }

    #[test]
    fn returns_malformed_paths_as_path_errors() {
        let symbols = Interner::new();
        let (tree, packages, current_key) = setup(&symbols, "acme:app#0.1");

        assert_eq!(
            resolve_use_target(
                &entry("acme:app/foo--bar"),
                &current_key,
                &packages,
                ModuleId::ROOT,
                &tree,
                &symbols,
            ),
            Err(UseTargetError::Path(UsePathError::InvalidPathSegment {
                index: 0
            }))
        );
    }

    #[test]
    fn rejects_a_supplied_package_key_that_does_not_match_the_tree() {
        let symbols = Interner::new();
        let (tree, packages, _) = setup(&symbols, "acme:app#0.1");
        let supplied_key = PackageKey::new("acme:other#0.1").unwrap();

        assert_eq!(
            resolve_use_target(
                &entry("acme:other/nested"),
                &supplied_key,
                &packages,
                ModuleId::ROOT,
                &tree,
                &symbols,
            ),
            Err(UseTargetError::PackageKeyMismatch {
                tree_package_key: "acme:app#0.1".to_owned(),
                supplied_package_key: "acme:other#0.1".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_a_different_compatibility_key_for_the_same_identity() {
        let symbols = Interner::new();
        let (tree, packages, _) = setup(&symbols, "acme:app#0.1");
        let supplied_key = PackageKey::new("acme:app#0.2").unwrap();

        assert_eq!(
            resolve_use_target(
                &entry("acme:app/nested"),
                &supplied_key,
                &packages,
                ModuleId::ROOT,
                &tree,
                &symbols,
            ),
            Err(UseTargetError::PackageKeyMismatch {
                tree_package_key: "acme:app#0.1".to_owned(),
                supplied_package_key: "acme:app#0.2".to_owned(),
            })
        );
    }

    #[test]
    fn rejects_package_ids_from_a_different_interner() {
        let symbols = Interner::new();
        let (tree, _packages, current_key) = setup(&symbols, "acme:app#0.1");
        let other_packages = PackageInterner::new();

        assert_eq!(
            resolve_use_target(
                &entry("acme:app/nested"),
                &current_key,
                &other_packages,
                ModuleId::ROOT,
                &tree,
                &symbols,
            ),
            Err(UseTargetError::PackageInterner(
                PackageIdError::UnknownPackage
            ))
        );
    }

    #[test]
    fn accepts_implicit_sai_package_keys() {
        let symbols = Interner::new();

        for identity in ["sai:core", "sai:alloc"] {
            let (tree, packages, current_key) = setup(&symbols, identity);
            assert_eq!(
                resolve_use_target(
                    &entry(&format!("{identity}/nested")),
                    &current_key,
                    &packages,
                    ModuleId::ROOT,
                    &tree,
                    &symbols,
                ),
                Ok(UseTarget::LocalModule(ModuleId::from_raw(1)))
            );
        }
    }

    #[test]
    fn rejects_wit_package_keys_for_sai_module_resolution() {
        let symbols = Interner::new();
        let wit_key = "wit:dir:acme:app#0.1";
        let (tree, packages, current_key) = setup(&symbols, wit_key);

        assert_eq!(
            resolve_use_target(
                &entry("acme:app/nested"),
                &current_key,
                &packages,
                ModuleId::ROOT,
                &tree,
                &symbols,
            ),
            Err(UseTargetError::UnsupportedPackageKey(wit_key.to_owned()))
        );
    }
}
