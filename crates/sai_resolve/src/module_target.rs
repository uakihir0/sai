//! Exact path resolution for module targets in the current SAI package.
//!
//! This is the module-only part of spec/04 §4.3.2. Package identity
//! selection, interfaces, dependencies, WIT, and import bindings are handled
//! by later resolver stages.

use super::{ChildEntry, ModuleId, ModuleTree, NamespaceId};
use sai_intern::{Interner, LookupError, Symbol};
use std::collections::BTreeMap;

/// Failure to resolve an exact path to a module in the current package.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ModuleTargetError {
    /// The package source tree has no root module.
    MissingRootModule,
    /// The supplied current module does not belong to this module tree.
    InvalidCurrentModule,
    /// A module or namespace child points outside its corresponding vector.
    InvalidModuleTree,
    /// One of the supplied path symbols does not belong to `symbols`.
    Symbol(LookupError),
    /// The path names an existing module, but that module is the current one.
    UseOfCurrentModule(ModuleId),
    /// No module exactly matches the path.
    ///
    /// `matched_prefix` is the longest prefix that names an existing module
    /// or namespace directory. `module_candidates` contains direct child
    /// modules of that prefix in deterministic source-spelling order.
    UnknownModule {
        matched_prefix: Vec<String>,
        module_candidates: Vec<String>,
    },
}

#[derive(Clone, Copy)]
enum Directory {
    Module(ModuleId),
    Namespace(NamespaceId),
}

/// Resolves path segments, after the package identity, to an exact module.
///
/// A zero-segment path names the package root. A path that ends at a
/// namespace directory is not a module target and returns `UnknownModule`
/// with that directory as the matched prefix.
pub fn resolve_module_target(
    tree: &ModuleTree,
    current_module: ModuleId,
    segments: &[Symbol],
    symbols: &Interner,
) -> Result<ModuleId, ModuleTargetError> {
    if tree.module(ModuleId::ROOT).is_none() {
        return Err(ModuleTargetError::MissingRootModule);
    }
    if tree.module(current_module).is_none() {
        return Err(ModuleTargetError::InvalidCurrentModule);
    }

    let mut directory = Directory::Module(ModuleId::ROOT);
    let mut matched_prefix = Vec::with_capacity(segments.len());

    if segments.is_empty() {
        return finish_target(ModuleId::ROOT, current_module);
    }

    for (index, segment) in segments.iter().enumerate() {
        let spelling = symbols
            .resolve(*segment)
            .map_err(ModuleTargetError::Symbol)?
            .to_string();
        let children = match directory {
            Directory::Module(id) => {
                &tree
                    .module(id)
                    .ok_or(ModuleTargetError::InvalidModuleTree)?
                    .children
            }
            Directory::Namespace(id) => {
                &tree
                    .namespace(id)
                    .ok_or(ModuleTargetError::InvalidModuleTree)?
                    .children
            }
        };

        let Some(entry) = children.get(&spelling).copied() else {
            let module_candidates = module_candidates(tree, children)?;
            return Err(ModuleTargetError::UnknownModule {
                matched_prefix,
                module_candidates,
            });
        };
        matched_prefix.push(spelling);
        let is_last = index + 1 == segments.len();

        match entry {
            ChildEntry::Module(id) => {
                if tree.module(id).is_none() {
                    return Err(ModuleTargetError::InvalidModuleTree);
                }
                if is_last {
                    return finish_target(id, current_module);
                }
                directory = Directory::Module(id);
            }
            ChildEntry::Namespace(id) => {
                if tree.namespace(id).is_none() {
                    return Err(ModuleTargetError::InvalidModuleTree);
                }
                if is_last {
                    let namespace = tree
                        .namespace(id)
                        .ok_or(ModuleTargetError::InvalidModuleTree)?;
                    let module_candidates = module_candidates(tree, &namespace.children)?;
                    return Err(ModuleTargetError::UnknownModule {
                        matched_prefix,
                        module_candidates,
                    });
                }
                directory = Directory::Namespace(id);
            }
            ChildEntry::Both(module, namespace) => {
                if tree.module(module).is_none() || tree.namespace(namespace).is_none() {
                    return Err(ModuleTargetError::InvalidModuleTree);
                }
                if is_last {
                    return finish_target(module, current_module);
                }
                // A path continuing beyond a segment represented by both a
                // module and a directory must follow the namespace directory.
                directory = Directory::Namespace(namespace);
            }
        }
    }

    // Every nonempty exact path returns or errors from the loop.
    Err(ModuleTargetError::InvalidModuleTree)
}

fn finish_target(
    target: ModuleId,
    current_module: ModuleId,
) -> Result<ModuleId, ModuleTargetError> {
    if target == current_module {
        Err(ModuleTargetError::UseOfCurrentModule(target))
    } else {
        Ok(target)
    }
}

fn module_candidates(
    tree: &ModuleTree,
    children: &BTreeMap<String, ChildEntry>,
) -> Result<Vec<String>, ModuleTargetError> {
    for entry in children.values() {
        let valid = match entry {
            ChildEntry::Module(module) => tree.module(*module).is_some(),
            ChildEntry::Namespace(namespace) => tree.namespace(*namespace).is_some(),
            ChildEntry::Both(module, namespace) => {
                tree.module(*module).is_some() && tree.namespace(*namespace).is_some()
            }
        };
        if !valid {
            return Err(ModuleTargetError::InvalidModuleTree);
        }
    }

    Ok(children
        .iter()
        .filter_map(|(name, entry)| match entry {
            ChildEntry::Module(_) | ChildEntry::Both(_, _) => Some(name.clone()),
            ChildEntry::Namespace(_) => None,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_intern::PackageInterner;
    use sai_vfs::FileId;

    fn symbol(interner: &Interner, name: &str) -> Symbol {
        interner.intern(name).expect("symbol is interned")
    }

    fn tree(
        interner: &Interner,
        modules: Vec<(Vec<&str>, BTreeMap<String, ChildEntry>)>,
        namespaces: Vec<(Vec<&str>, BTreeMap<String, ChildEntry>)>,
    ) -> ModuleTree {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:app#0.1")
            .expect("package identity is valid");
        ModuleTree {
            package,
            modules: modules
                .into_iter()
                .map(|(path, children)| super::super::ModuleData {
                    path: path
                        .into_iter()
                        .map(|segment| symbol(interner, segment))
                        .collect(),
                    file: FileId::new(0),
                    parent_dir: None,
                    children,
                })
                .collect(),
            namespaces: namespaces
                .into_iter()
                .map(|(path, children)| super::super::NamespaceData {
                    path: path
                        .into_iter()
                        .map(|segment| symbol(interner, segment))
                        .collect(),
                    children,
                })
                .collect(),
            diagnostics: Vec::new(),
        }
    }

    fn children(entries: &[(&str, ChildEntry)]) -> BTreeMap<String, ChildEntry> {
        entries
            .iter()
            .map(|(name, entry)| ((*name).to_owned(), *entry))
            .collect()
    }

    #[test]
    fn empty_path_resolves_to_the_package_root() {
        let symbols = Interner::new();
        let tree = tree(
            &symbols,
            vec![
                (
                    vec![],
                    children(&[("nested", ChildEntry::Module(ModuleId::from_raw(1)))]),
                ),
                (vec!["nested"], BTreeMap::new()),
            ],
            vec![],
        );

        assert_eq!(
            resolve_module_target(&tree, ModuleId::from_raw(1), &[], &symbols),
            Ok(ModuleId::ROOT)
        );
    }

    #[test]
    fn exact_nested_path_traverses_a_namespace_and_a_both_directory() {
        let symbols = Interner::new();
        let tree = tree(
            &symbols,
            vec![
                (
                    vec![],
                    children(&[("group", ChildEntry::Namespace(NamespaceId::from_raw(0)))]),
                ),
                (vec!["group", "entry"], BTreeMap::new()),
                (vec!["group", "entry", "nested"], BTreeMap::new()),
            ],
            vec![
                (
                    vec!["group"],
                    children(&[(
                        "entry",
                        ChildEntry::Both(ModuleId::from_raw(1), NamespaceId::from_raw(1)),
                    )]),
                ),
                (
                    vec!["group", "entry"],
                    children(&[("nested", ChildEntry::Module(ModuleId::from_raw(2)))]),
                ),
            ],
        );
        let path = ["group", "entry", "nested"].map(|segment| symbol(&symbols, segment));

        assert_eq!(
            resolve_module_target(&tree, ModuleId::ROOT, &path, &symbols),
            Ok(ModuleId::from_raw(2))
        );
    }

    #[test]
    fn unknown_path_reports_longest_namespace_prefix_and_sorted_module_candidates() {
        let symbols = Interner::new();
        let tree = tree(
            &symbols,
            vec![
                (
                    vec![],
                    children(&[("group", ChildEntry::Namespace(NamespaceId::from_raw(0)))]),
                ),
                (vec!["group", "zeta"], BTreeMap::new()),
                (vec!["group", "alpha"], BTreeMap::new()),
            ],
            vec![
                (
                    vec!["group"],
                    children(&[
                        ("zeta", ChildEntry::Module(ModuleId::from_raw(1))),
                        ("alpha", ChildEntry::Module(ModuleId::from_raw(2))),
                        ("nested", ChildEntry::Namespace(NamespaceId::from_raw(1))),
                    ]),
                ),
                (vec!["group", "nested"], BTreeMap::new()),
            ],
        );
        let path = ["group", "missing"].map(|segment| symbol(&symbols, segment));

        assert_eq!(
            resolve_module_target(&tree, ModuleId::ROOT, &path, &symbols),
            Err(ModuleTargetError::UnknownModule {
                matched_prefix: vec!["group".to_owned()],
                module_candidates: vec!["alpha".to_owned(), "zeta".to_owned()],
            })
        );
    }

    #[test]
    fn unknown_path_rejects_any_dangling_direct_child_reference() {
        let symbols = Interner::new();
        let invalid_entries = [
            ChildEntry::Module(ModuleId::from_raw(99)),
            ChildEntry::Namespace(NamespaceId::from_raw(99)),
            ChildEntry::Both(ModuleId::ROOT, NamespaceId::from_raw(99)),
            ChildEntry::Both(ModuleId::from_raw(99), NamespaceId::from_raw(0)),
        ];

        for entry in invalid_entries {
            let tree = tree(
                &symbols,
                vec![(vec![], children(&[("dangling", entry)]))],
                vec![(vec!["namespace"], BTreeMap::new())],
            );
            let path = [symbol(&symbols, "missing")];

            assert_eq!(
                resolve_module_target(&tree, ModuleId::ROOT, &path, &symbols),
                Err(ModuleTargetError::InvalidModuleTree),
                "entry {entry:?} must be validated even though the requested name is absent"
            );
        }
    }

    #[test]
    fn namespace_only_terminal_path_is_unknown_module_with_its_child_modules() {
        let symbols = Interner::new();
        let tree = tree(
            &symbols,
            vec![
                (
                    vec![],
                    children(&[("group", ChildEntry::Namespace(NamespaceId::from_raw(0)))]),
                ),
                (vec!["group", "child"], BTreeMap::new()),
            ],
            vec![(
                vec!["group"],
                children(&[("child", ChildEntry::Module(ModuleId::from_raw(1)))]),
            )],
        );
        let path = [symbol(&symbols, "group")];

        assert_eq!(
            resolve_module_target(&tree, ModuleId::ROOT, &path, &symbols),
            Err(ModuleTargetError::UnknownModule {
                matched_prefix: vec!["group".to_owned()],
                module_candidates: vec!["child".to_owned()],
            })
        );
    }

    #[test]
    fn namespace_terminal_rejects_any_dangling_direct_child_reference() {
        let symbols = Interner::new();
        let invalid_entries = [
            ChildEntry::Module(ModuleId::from_raw(99)),
            ChildEntry::Namespace(NamespaceId::from_raw(99)),
            ChildEntry::Both(ModuleId::ROOT, NamespaceId::from_raw(99)),
            ChildEntry::Both(ModuleId::from_raw(99), NamespaceId::from_raw(0)),
        ];

        for entry in invalid_entries {
            let tree = tree(
                &symbols,
                vec![(
                    vec![],
                    children(&[("group", ChildEntry::Namespace(NamespaceId::from_raw(0)))]),
                )],
                vec![(vec!["group"], children(&[("dangling", entry)]))],
            );
            let path = [symbol(&symbols, "group")];

            assert_eq!(
                resolve_module_target(&tree, ModuleId::ROOT, &path, &symbols),
                Err(ModuleTargetError::InvalidModuleTree),
                "entry {entry:?} must be validated when listing namespace children"
            );
        }
    }

    #[test]
    fn targeting_the_current_module_is_rejected() {
        let symbols = Interner::new();
        let tree = tree(
            &symbols,
            vec![
                (
                    vec![],
                    children(&[("nested", ChildEntry::Module(ModuleId::from_raw(1)))]),
                ),
                (vec!["nested"], BTreeMap::new()),
            ],
            vec![],
        );
        let path = [symbol(&symbols, "nested")];

        assert_eq!(
            resolve_module_target(&tree, ModuleId::from_raw(1), &path, &symbols),
            Err(ModuleTargetError::UseOfCurrentModule(ModuleId::from_raw(1)))
        );
    }

    #[test]
    fn missing_root_module_is_reported() {
        let symbols = Interner::new();
        let tree = tree(&symbols, vec![], vec![]);

        assert_eq!(
            resolve_module_target(&tree, ModuleId::ROOT, &[], &symbols),
            Err(ModuleTargetError::MissingRootModule)
        );
    }
}
