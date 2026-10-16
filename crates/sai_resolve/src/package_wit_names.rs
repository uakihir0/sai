//! Package-scope SAI-to-WIT names for local interfaces and worlds.
//!
//! This collector follows WIT generation order: interfaces are ordered by
//! declaring module path and source order, followed by worlds in that same
//! order. The returned map is an identity lookup table; consumers that
//! fingerprint this result must use canonical paths and generation order,
//! never the session-local key order of the map.

use super::{ModuleId, ModuleTree};
use crate::package_scopes::{FilteredModule, Phase1PackageScopes, TopLevelOwnerKind};
use crate::wit_name::{sai_identifier_to_wit_name, suggested_representable_name};
use sai_diagnostics::{Child, ChildKind, Code, Diagnostic, Location, Related, Severity, Value};
use sai_hir::LocalDefId;
use sai_hir::item_tree::{ErasedAstId, ItemTreeEntry, ItemTreeKind};
use sai_intern::{Interner, LookupError, PackageIdError, PackageInterner};
use sai_syntax::{
    SyntaxNode,
    ast::{self, AstNode},
};
use sai_vfs::{FileId, Span};
use smol_str::SmolStr;
use std::collections::BTreeMap;

/// The representable WIT names of a package's local interfaces and worlds.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PackageWitNames {
    /// Every representable interface and world, keyed by its local definition.
    ///
    /// Invalid names have no entry. The value excludes WIT's `%` keyword
    /// escape; the printer adds that syntax when writing the name.
    pub names: BTreeMap<LocalDefId, SmolStr>,
    /// Name-conversion and package-scope collision diagnostics in generation
    /// order.
    pub diagnostics: Vec<Diagnostic>,
}

/// Failure to collect package-level WIT names from the current source inputs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PackageWitNamesError {
    Package(PackageIdError),
    Symbol(LookupError),
    TooManyModules,
    PackageScopeMismatch,
    MissingModuleInput(ModuleId),
    UnexpectedModuleInput(ModuleId),
    ModuleInputMismatch(ModuleId),
    DuplicateModuleFile(FileId),
    MissingOwnerModule(FileId),
    MissingOwnerEntry,
    MismatchedOwnerKind,
    MissingAstNode,
    MissingName,
    InvalidSourceRange,
    WrongOwnerPackage,
}

impl From<PackageIdError> for PackageWitNamesError {
    fn from(error: PackageIdError) -> Self {
        Self::Package(error)
    }
}

impl From<LookupError> for PackageWitNamesError {
    fn from(error: LookupError) -> Self {
        Self::Symbol(error)
    }
}

/// Collects package-scope WIT names for the interfaces and worlds in `scopes`.
///
/// The package scope supplies canonical local identities and source-order
/// owner descriptors. Source declaration names and locations are recovered
/// from the matching target-filtered item trees. Module paths, rather than
/// `ModuleId`, determine cross-module diagnostic order.
pub fn package_wit_names(
    module_tree: &ModuleTree,
    scopes: &Phase1PackageScopes,
    module_inputs: &BTreeMap<ModuleId, FilteredModule<'_>>,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Result<PackageWitNames, PackageWitNamesError> {
    if module_tree.modules.len() != scopes.modules.len() {
        return Err(PackageWitNamesError::PackageScopeMismatch);
    }

    let package_identity = packages.resolve(module_tree.package)?.to_string();
    let mut modules_by_file = BTreeMap::<FileId, (ModuleId, Vec<String>)>::new();
    for (index, module) in module_tree.modules.iter().enumerate() {
        let raw = u32::try_from(index).map_err(|_| PackageWitNamesError::TooManyModules)?;
        let module_id = ModuleId::from_raw(raw);
        let input = module_inputs
            .get(&module_id)
            .ok_or(PackageWitNamesError::MissingModuleInput(module_id))?;
        if input.filtered.item_tree.file != module.file || input.source.id() != module.file {
            return Err(PackageWitNamesError::ModuleInputMismatch(module_id));
        }
        let path = module
            .path
            .iter()
            .map(|segment| symbols.resolve(*segment).map(|name| name.to_string()))
            .collect::<Result<Vec<_>, _>>()?;
        if modules_by_file
            .insert(module.file, (module_id, path))
            .is_some()
        {
            return Err(PackageWitNamesError::DuplicateModuleFile(module.file));
        }
    }

    for module_id in module_inputs.keys() {
        if module_id.raw() as usize >= module_tree.modules.len() {
            return Err(PackageWitNamesError::UnexpectedModuleInput(*module_id));
        }
    }

    let mut candidates = Vec::new();
    for (source_order, owner) in scopes.top_level_owners.iter().enumerate() {
        let kind = match owner.kind {
            TopLevelOwnerKind::Interface => PackageWitNameKind::Interface,
            TopLevelOwnerKind::World => PackageWitNameKind::World,
            _ => continue,
        };
        let def_id = owner.owner.def_id;
        if def_id.package() != module_tree.package {
            return Err(PackageWitNamesError::WrongOwnerPackage);
        }

        let Some((module_id, module_path)) = modules_by_file.get(&owner.file) else {
            return Err(PackageWitNamesError::MissingOwnerModule(owner.file));
        };
        let input = module_inputs
            .get(module_id)
            .ok_or(PackageWitNamesError::MissingModuleInput(*module_id))?;
        if input.filtered.item_tree.file != owner.file {
            return Err(PackageWitNamesError::ModuleInputMismatch(*module_id));
        }
        let entry = input
            .filtered
            .item_tree
            .items
            .iter()
            .find(|entry| entry.ast_id == owner.item_id)
            .ok_or(PackageWitNamesError::MissingOwnerEntry)?;
        let expected_item_kind = match kind {
            PackageWitNameKind::Interface => ItemTreeKind::Interface,
            PackageWitNameKind::World => ItemTreeKind::World,
        };
        if entry.kind != expected_item_kind {
            return Err(PackageWitNamesError::MismatchedOwnerKind);
        }

        // Recovery names already have parser diagnostics and cannot be
        // converted into a meaningful WIT name.
        if entry.name.is_error() {
            continue;
        }
        let source_name = symbols.resolve(entry.name)?.to_string();
        let location = declaration_name_location(entry, input)?;
        let duplicate_of = scopes.diagnostics.iter().find_map(|diagnostic| {
            if diagnostic.code != Code::DuplicateBinding
                || !diagnostic
                    .primary()
                    .is_some_and(|primary| primary == &location)
            {
                return None;
            }
            diagnostic
                .related()
                .iter()
                .find_map(|related| match related {
                    Related::DeclaredHere { location, .. } => Some(location.clone()),
                    _ => None,
                })
        });
        candidates.push(PackageWitNameCandidate {
            def_id,
            kind,
            module_path: module_path.clone(),
            source_order,
            source_name,
            location,
            duplicate_of,
        });
    }

    Ok(collect_package_wit_names(&package_identity, candidates))
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum PackageWitNameKind {
    Interface,
    World,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct PackageWitNameCandidate {
    def_id: LocalDefId,
    kind: PackageWitNameKind,
    module_path: Vec<String>,
    source_order: usize,
    source_name: String,
    location: Location,
    duplicate_of: Option<Location>,
}

#[derive(Clone, Debug)]
struct EarlierPackageWitName {
    def_id: LocalDefId,
    location: Location,
}

fn collect_package_wit_names(
    package_identity: &str,
    mut candidates: Vec<PackageWitNameCandidate>,
) -> PackageWitNames {
    candidates.sort_by(|left, right| {
        left.kind
            .cmp(&right.kind)
            .then_with(|| left.module_path.cmp(&right.module_path))
            .then_with(|| left.source_order.cmp(&right.source_order))
    });

    let mut names = BTreeMap::new();
    let mut first_by_wit_name = BTreeMap::<String, EarlierPackageWitName>::new();
    let mut diagnostics = Vec::new();

    for candidate in candidates {
        let Some(wit_name) = sai_identifier_to_wit_name(&candidate.source_name) else {
            let mut diagnostic = Diagnostic::new(
                Code::WitNameNotRepresentable,
                Severity::Error,
                format!(
                    "SAI name `{}` cannot be represented as a WIT name",
                    candidate.source_name
                ),
                Some(candidate.location.clone()),
            );
            diagnostic.primary_label = "not representable as a WIT name".to_owned();
            diagnostic.package = Some(package_identity.to_owned());
            if let Some(suggestion) = suggested_representable_name(&candidate.source_name) {
                diagnostic
                    .data
                    .insert("suggestion".to_owned(), Value::String(suggestion.clone()));
                diagnostic
                    .push_child(Child {
                        kind: ChildKind::Help,
                        message: format!("try `{suggestion}`"),
                        location: None,
                    })
                    .expect("unbound diagnostics accept help children");
            }
            diagnostics.push(diagnostic);
            continue;
        };

        let value = SmolStr::new(&wit_name);
        names.insert(candidate.def_id, value);

        if let Some(earlier) = first_by_wit_name.get(&wit_name) {
            let duplicate_pair = candidate
                .duplicate_of
                .as_ref()
                .is_some_and(|location| location == &earlier.location);
            if earlier.def_id != candidate.def_id && !duplicate_pair {
                let mut diagnostic = Diagnostic::new(
                    Code::WitNameCollision,
                    Severity::Error,
                    format!("multiple declarations map to WIT name `{wit_name}`"),
                    Some(candidate.location.clone()),
                );
                diagnostic.primary_label = "colliding WIT name".to_owned();
                diagnostic.package = Some(package_identity.to_owned());
                diagnostic
                    .push_related(Related::DeclaredHere {
                        location: earlier.location.clone(),
                        what: "earlier declaration".to_owned(),
                    })
                    .expect("unbound diagnostics accept related locations");
                diagnostics.push(diagnostic);
            }
        } else {
            first_by_wit_name.insert(
                wit_name,
                EarlierPackageWitName {
                    def_id: candidate.def_id,
                    location: candidate.location,
                },
            );
        }
    }

    PackageWitNames { names, diagnostics }
}

fn declaration_name_location(
    entry: &ItemTreeEntry,
    input: &FilteredModule<'_>,
) -> Result<Location, PackageWitNamesError> {
    let syntax = resolve_entry_syntax(entry.ast_id, input)?;
    let range = match entry.kind {
        ItemTreeKind::Interface => ast::InterfaceDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        ItemTreeKind::World => ast::WorldDecl::cast(syntax)
            .and_then(|declaration| declaration.name())
            .map(|name| name.syntax().text_range()),
        _ => None,
    }
    .ok_or(PackageWitNamesError::MissingName)?;
    Location::from_decoded_source(
        input.source_ref.clone(),
        Span::new(input.filtered.item_tree.file, range),
        input.source,
    )
    .ok_or(PackageWitNamesError::InvalidSourceRange)
}

fn resolve_entry_syntax(
    item_id: ErasedAstId,
    input: &FilteredModule<'_>,
) -> Result<SyntaxNode, PackageWitNamesError> {
    input
        .ast_ids
        .pointer(item_id)
        .and_then(|pointer| pointer.resolve(input.filtered.item_tree.file, input.root))
        .ok_or(PackageWitNamesError::MissingAstNode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_scopes::{Phase1BuildContext, Phase1CollectionConfig, collect_phase1};
    use crate::{ChildEntry, ModuleData};
    use sai_diagnostics::{Related, SourceRef, Value};
    use sai_hir::attrs::TargetCfg;
    use sai_hir::item_tree::{ItemTree, TargetFilterResult};
    use sai_hir::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, PackageId};
    use sai_intern::PackageInterner;
    use sai_syntax::{Parse, ast};
    use sai_vfs::{SnapshotFile, SourceFile, TextRange, TextSize, WorkspaceSnapshot};
    use std::sync::Arc;

    fn local_def_id(
        package: sai_hir::PackageId,
        packages: &PackageInterner,
        symbols: &Interner,
        paths: &DefPathInterner,
        name: &str,
        kind: PackageWitNameKind,
    ) -> LocalDefId {
        let name = symbols.intern(name).expect("symbol interns");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: match kind {
                    PackageWitNameKind::Interface => DefPathData::Interface(name),
                    PackageWitNameKind::World => DefPathData::World(name),
                },
                disambiguator: u32::try_from(paths.len().expect("path count reads"))
                    .expect("test definition count fits"),
            }],
            packages,
            symbols,
        )
        .expect("path belongs to its interners");
        paths.intern(path).expect("definition path interns")
    }

    fn candidate(
        def_id: LocalDefId,
        kind: PackageWitNameKind,
        module_path: &[&str],
        source_order: usize,
        source_name: &str,
        offset: u32,
        duplicate_of: Option<Location>,
    ) -> PackageWitNameCandidate {
        let start = TextSize::new(offset);
        PackageWitNameCandidate {
            def_id,
            kind,
            module_path: module_path.iter().map(|part| (*part).to_owned()).collect(),
            source_order,
            source_name: source_name.to_owned(),
            location: Location::new(
                format!("src/{}.sai", module_path.join("/")),
                Span::new(
                    FileId::new(offset + 1),
                    TextRange::new(start, TextSize::new(offset + 1))
                        .expect("source range is ordered"),
                ),
            ),
            duplicate_of,
        }
    }

    fn fixture() -> (PackageInterner, Interner, DefPathInterner, PackageId) {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:app#1.0")
            .expect("package key interns");
        let symbols = Interner::new();
        let paths = DefPathInterner::new(package, &packages, &symbols)
            .expect("definition path interner builds");
        (packages, symbols, paths, package)
    }

    struct ModuleSource {
        source: SourceFile,
        parsed: Parse,
        ast_ids: ast::AstIdMap,
        filtered: TargetFilterResult,
    }

    impl ModuleSource {
        fn new(
            file: FileId,
            text: &str,
            snapshot: &WorkspaceSnapshot,
            symbols: &Interner,
            target_cfg: &TargetCfg,
        ) -> Self {
            let source = SourceFile::new(file, text, false).expect("source is valid");
            let parsed = sai_syntax::parse(snapshot, file).expect("source parses");
            assert!(
                parsed.errors.is_empty(),
                "parse errors: {:?}",
                parsed.errors
            );
            let ast_ids = ast::AstIdMap::new(file, &parsed.root).expect("AST IDs bind");
            let filtered = ItemTree::collect_for_target(
                file,
                text,
                &parsed.root,
                &ast_ids,
                symbols,
                snapshot,
                target_cfg,
            )
            .expect("item tree collects and target-filters");
            Self {
                source,
                parsed,
                ast_ids,
                filtered,
            }
        }
    }

    #[test]
    fn public_collector_reads_phase_one_owners_and_suppresses_reported_duplicates() {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:app#1.0")
            .expect("package key interns");
        let symbols = Interner::new();
        let target_cfg =
            TargetCfg::new("wasm32-core", std::iter::empty::<&str>()).expect("target is valid");
        let root_file = FileId::new(71);
        let child_file = FileId::new(72);
        let root_text = "interface net {}\ninterface net {}\ninterface utf_8 {}\n";
        let child_text = "fn net() {}\ninterface net {}\n";
        let root_source = SourceFile::new(root_file, root_text, false).expect("source is valid");
        let child_source = SourceFile::new(child_file, child_text, false).expect("source is valid");
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(Arc::new(root_source.clone()), "src/lib.sai", None, None),
            SnapshotFile::new(Arc::new(child_source.clone()), "src/child.sai", None, None),
        ])
        .expect("snapshot is valid");
        let root = ModuleSource::new(root_file, root_text, &snapshot, &symbols, &target_cfg);
        let child = ModuleSource::new(child_file, child_text, &snapshot, &symbols, &target_cfg);
        let child_name = symbols.intern("child").expect("module name interns");
        let module_tree = ModuleTree {
            package,
            modules: vec![
                ModuleData {
                    path: Vec::new(),
                    file: root_file,
                    parent_dir: None,
                    children: BTreeMap::from([(
                        "child".to_owned(),
                        ChildEntry::Module(ModuleId::from_raw(1)),
                    )]),
                },
                ModuleData {
                    path: vec![child_name],
                    file: child_file,
                    parent_dir: Some(ModuleId::ROOT),
                    children: BTreeMap::new(),
                },
            ],
            namespaces: Vec::new(),
            diagnostics: Vec::new(),
        };
        let module_inputs = BTreeMap::from([
            (
                ModuleId::ROOT,
                FilteredModule {
                    filtered: &root.filtered,
                    ast_ids: &root.ast_ids,
                    root: &root.parsed.root,
                    source: &root.source,
                    source_ref: SourceRef {
                        path: "src/lib.sai".to_owned(),
                        package: None,
                        snapshot: None,
                    },
                },
            ),
            (
                ModuleId::from_raw(1),
                FilteredModule {
                    filtered: &child.filtered,
                    ast_ids: &child.ast_ids,
                    root: &child.parsed.root,
                    source: &child.source,
                    source_ref: SourceRef {
                        path: "src/child.sai".to_owned(),
                        package: None,
                        snapshot: None,
                    },
                },
            ),
        ]);
        let paths =
            DefPathInterner::new(package, &packages, &symbols).expect("definition path interner");
        let scopes = collect_phase1(
            &module_tree,
            &module_inputs,
            Phase1CollectionConfig {
                snapshot: &snapshot,
                target_cfg: &target_cfg,
                build_context: Phase1BuildContext::default(),
            },
            &packages,
            &symbols,
            &paths,
        )
        .expect("phase-one package scopes collect");

        assert!(
            scopes
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::DuplicateBinding)
        );
        let result = package_wit_names(&module_tree, &scopes, &module_inputs, &packages, &symbols)
            .expect("package WIT names collect");

        assert_eq!(result.names.len(), 3);
        assert_eq!(
            result
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::WitNameNotRepresentable, Code::WitNameCollision]
        );
        assert_eq!(
            result.diagnostics[1]
                .primary()
                .expect("collision has a location")
                .file
                .path,
            "src/child.sai"
        );
    }

    #[test]
    fn generation_order_is_deterministic_and_places_all_interfaces_before_worlds() {
        let (packages, symbols, paths, package) = fixture();
        let late_interface = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "net",
            PackageWitNameKind::Interface,
        );
        let early_interface = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "net",
            PackageWitNameKind::Interface,
        );
        let world = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "net",
            PackageWitNameKind::World,
        );
        let candidates = vec![
            candidate(
                late_interface,
                PackageWitNameKind::Interface,
                &["z"],
                0,
                "net",
                30,
                None,
            ),
            candidate(world, PackageWitNameKind::World, &[], 0, "net", 40, None),
            candidate(
                early_interface,
                PackageWitNameKind::Interface,
                &["a"],
                0,
                "net",
                10,
                None,
            ),
        ];

        let forward = collect_package_wit_names("acme:app", candidates.clone());
        let reverse = collect_package_wit_names("acme:app", candidates.into_iter().rev().collect());

        assert_eq!(forward, reverse);
        let primary_offsets = forward
            .diagnostics
            .iter()
            .map(|diagnostic| diagnostic.primary().unwrap().span.range.start().raw())
            .collect::<Vec<_>>();
        assert_eq!(primary_offsets, [30, 40]);
        assert_eq!(forward.names.len(), 3);
    }

    #[test]
    fn cross_module_collision_points_to_later_module_and_notes_earlier_declaration() {
        let (packages, symbols, paths, package) = fixture();
        let first = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "net",
            PackageWitNameKind::Interface,
        );
        let later = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "net",
            PackageWitNameKind::Interface,
        );
        let earlier_candidate = candidate(
            first,
            PackageWitNameKind::Interface,
            &["a"],
            0,
            "net",
            8,
            None,
        );
        let later_candidate = candidate(
            later,
            PackageWitNameKind::Interface,
            &["b"],
            0,
            "net",
            28,
            None,
        );

        let result = collect_package_wit_names(
            "acme:app",
            vec![later_candidate.clone(), earlier_candidate.clone()],
        );

        assert_eq!(result.names.len(), 2);
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.code, Code::WitNameCollision);
        assert_eq!(diagnostic.primary(), Some(&later_candidate.location));
        assert!(matches!(
            diagnostic.related(),
            [Related::DeclaredHere { location, .. }] if location == &earlier_candidate.location
        ));
    }

    #[test]
    fn invalid_names_are_omitted_and_carry_a_representable_suggestion() {
        let (packages, symbols, paths, package) = fixture();
        let invalid = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "utf_8",
            PackageWitNameKind::Interface,
        );
        let valid = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "codec",
            PackageWitNameKind::Interface,
        );

        let result = collect_package_wit_names(
            "acme:app",
            vec![
                candidate(
                    invalid,
                    PackageWitNameKind::Interface,
                    &[],
                    0,
                    "utf_8",
                    3,
                    None,
                ),
                candidate(
                    valid,
                    PackageWitNameKind::Interface,
                    &[],
                    1,
                    "codec",
                    15,
                    None,
                ),
            ],
        );

        assert!(!result.names.contains_key(&invalid));
        assert_eq!(result.names.get(&valid).map(SmolStr::as_str), Some("codec"));
        assert_eq!(result.diagnostics.len(), 1);
        let diagnostic = &result.diagnostics[0];
        assert_eq!(diagnostic.code, Code::WitNameNotRepresentable);
        assert_eq!(
            diagnostic.data.get("suggestion"),
            Some(&Value::String("utf8".to_owned()))
        );
    }

    #[test]
    fn same_scope_duplicate_is_not_reported_again_as_a_wit_collision() {
        let (packages, symbols, paths, package) = fixture();
        let first = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "net",
            PackageWitNameKind::Interface,
        );
        let duplicate = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "net",
            PackageWitNameKind::Interface,
        );
        let other_module = local_def_id(
            package,
            &packages,
            &symbols,
            &paths,
            "net",
            PackageWitNameKind::Interface,
        );

        let first_candidate = candidate(
            first,
            PackageWitNameKind::Interface,
            &["a"],
            0,
            "net",
            2,
            None,
        );
        let other_module_candidate = candidate(
            other_module,
            PackageWitNameKind::Interface,
            &["b"],
            0,
            "net",
            32,
            None,
        );
        let result = collect_package_wit_names(
            "acme:app",
            vec![
                candidate(
                    duplicate,
                    PackageWitNameKind::Interface,
                    &["a"],
                    1,
                    "net",
                    12,
                    Some(first_candidate.location.clone()),
                ),
                other_module_candidate.clone(),
                first_candidate.clone(),
            ],
        );

        assert_eq!(result.names.len(), 3);
        assert_eq!(result.diagnostics.len(), 1);
        assert_eq!(result.diagnostics[0].code, Code::WitNameCollision);
        assert_eq!(
            result.diagnostics[0].primary(),
            Some(&other_module_candidate.location)
        );
        assert!(matches!(
            result.diagnostics[0].related(),
            [Related::DeclaredHere { location, .. }] if location == &first_candidate.location
        ));
    }
}
