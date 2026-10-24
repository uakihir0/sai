use super::{OwnedPhase1ModuleInput, PreparedPhase1PackageScopes};
use sai_hir::{
    DefPathData, DefPathHash, DefPathInterner, LocalDefId, PackageId,
    item_tree::{ItemTreeEntry, ItemTreeKind},
};
use sai_intern::{Interner, PackageInterner, Symbol};
use sai_package::mvs::MvsPackageVersion;
use sai_resolve::package_scopes::{
    FunctionOwnerDescriptor, Phase1PackageScopes, RefinementPredicateOwnerDescriptor,
    RefinementPredicateOwnerKind, TopLevelOwnerDescriptor, TopLevelOwnerKind,
};
use sai_semantic::{
    CurrentSymbol, DeclarationFingerprintError, PackageIndexFileName, PackageSymbolInventory,
    SymbolKind, declaration_fingerprint,
};
use sai_stable_hash::Fingerprint;
use sai_syntax::{
    SyntaxElement, SyntaxNode,
    ast::{self, AstNode, AstToken},
};
use sai_vfs::{FileId, PackageKey};
use std::collections::{HashMap, HashSet};

/// Result of collecting the first package inventory slice.
///
/// `Unavailable` means the current Phase 1 scope did not provide the exact
/// package graph node needed to name an index file. It must not be guessed.
/// `Available` may still contain an explicitly incomplete inventory, which
/// callers must not reconcile or persist.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Phase1PackageSymbolInventory {
    Available(PackageSymbolInventory),
    Unavailable { reason: String },
}

impl Phase1PackageSymbolInventory {
    pub fn inventory(&self) -> Option<&PackageSymbolInventory> {
        match self {
            Self::Available(inventory) => Some(inventory),
            Self::Unavailable { .. } => None,
        }
    }
}

/// Collects target-visible module-level functions and their generic
/// parameters, records with their generic parameters and direct named fields,
/// enums with their generic parameters, variants and record-variant fields,
/// non-refined distinct types and their generic parameters, plain type aliases
/// and their generic parameters, non-generic refined types, constants, user
/// effects, tests, and properties. Every other symbol-bearing declaration
/// makes the result incomplete until its symbol kind and path are collected as
/// well.
pub(super) fn collect_module_function_inventory(
    package_key: &PackageKey,
    package_version: Option<&MvsPackageVersion>,
    prepared: &PreparedPhase1PackageScopes,
    scopes: &Phase1PackageScopes,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) -> Phase1PackageSymbolInventory {
    let Some(package_version) = package_version else {
        return Phase1PackageSymbolInventory::Unavailable {
            reason: "the Phase 1 scope has no exact source package graph node".to_owned(),
        };
    };
    let expected_package_key = format!(
        "{}#{}",
        package_version.package(),
        package_version.node().line()
    );
    if package_key.as_str() != expected_package_key {
        return Phase1PackageSymbolInventory::Unavailable {
            reason: "the Phase 1 package key does not match its exact selected graph node"
                .to_owned(),
        };
    }
    let Some((namespace, package)) = package_version.package().split_once(':') else {
        return Phase1PackageSymbolInventory::Unavailable {
            reason: "the selected package graph node has an invalid package identity".to_owned(),
        };
    };
    let compatibility_line = package_version.version().compatibility_line().to_string();
    let file = match PackageIndexFileName::new(namespace, package, &compatibility_line) {
        Ok(file) => file,
        Err(error) => {
            return Phase1PackageSymbolInventory::Unavailable {
                reason: format!(
                    "the selected package graph node cannot name an index file: {error}"
                ),
            };
        }
    };
    let package_node_key = format!(
        "package:{}@{}",
        package_version.package(),
        package_version.version()
    );

    let mut reasons = HashSet::new();
    let mut module_files = HashSet::<FileId>::new();
    if prepared.module_tree.modules.is_empty() {
        reasons.insert("the package module tree contains no source modules".to_owned());
    }
    for module in &prepared.module_tree.modules {
        if !module_files.insert(module.file) {
            reasons.insert("the module tree contains a duplicate source file".to_owned());
        }
    }

    let mut inputs_by_file = HashMap::<FileId, &OwnedPhase1ModuleInput>::new();
    for input in &prepared.modules {
        let file_id = input.filtered.item_tree.file;
        if input.ast_ids.file() != file_id
            || input.source.id() != file_id
            || !input.ast_ids.matches_root(file_id, &input.root)
            || !input.ast_ids.matches_snapshot(&prepared.snapshot)
        {
            reasons.insert(
                "a target-filtered module input is not bound to the current AST snapshot"
                    .to_owned(),
            );
        }
        if syntax_has_errors(&input.root) {
            reasons.insert("a module syntax tree contains recovery or parse errors".to_owned());
        }
        if input
            .filtered
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.severity == sai_diagnostics::Severity::Error)
        {
            reasons.insert(
                "a target-filtered module has an attribute or target-selection error".to_owned(),
            );
        }
        if inputs_by_file.insert(file_id, input).is_some() {
            reasons.insert("Phase 1 contains duplicate inputs for one source file".to_owned());
        }
    }
    if module_files.len() != inputs_by_file.len() {
        reasons.insert("the target-filtered inputs do not cover the exact module tree".to_owned());
    }

    let mut owners_by_item =
        HashMap::<(FileId, sai_hir::item_tree::ErasedAstId), FunctionOwnerDescriptor>::new();
    let mut record_owners_by_item =
        HashMap::<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>::new();
    let mut enum_owners_by_item =
        HashMap::<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>::new();
    let mut distinct_owners_by_item =
        HashMap::<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>::new();
    let mut const_owners_by_item =
        HashMap::<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>::new();
    let mut effect_owners_by_item =
        HashMap::<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>::new();
    let mut alias_owners_by_item =
        HashMap::<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>::new();
    let mut refined_owners_by_item =
        HashMap::<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>::new();
    let mut test_owners_by_item =
        HashMap::<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>::new();
    let mut refinement_predicate_owners_by_item = HashMap::<
        (FileId, sai_hir::item_tree::ErasedAstId),
        Vec<RefinementPredicateOwnerDescriptor>,
    >::new();
    let mut owner_ids = HashSet::<LocalDefId>::new();
    for descriptor in &scopes.function_owners {
        if owners_by_item
            .insert((descriptor.file, descriptor.item_id), *descriptor)
            .is_some()
        {
            reasons.insert("Phase 1 contains duplicate function owner descriptors".to_owned());
        }
        if !owner_ids.insert(descriptor.owner.def_id) {
            reasons.insert(
                "Phase 1 assigns one definition ID to multiple function declarations".to_owned(),
            );
        }
    }
    for descriptor in &scopes.top_level_owners {
        let key = (descriptor.file, descriptor.item_id);
        match descriptor.kind {
            TopLevelOwnerKind::Struct => {
                if record_owners_by_item.insert(key, *descriptor).is_some() {
                    reasons.insert("Phase 1 contains duplicate record owner descriptors".into());
                }
            }
            TopLevelOwnerKind::Enum => {
                if enum_owners_by_item.insert(key, *descriptor).is_some() {
                    reasons.insert("Phase 1 contains duplicate enum owner descriptors".into());
                }
            }
            TopLevelOwnerKind::Distinct => {
                if distinct_owners_by_item.insert(key, *descriptor).is_some() {
                    reasons.insert(
                        "Phase 1 contains duplicate distinct-type owner descriptors".into(),
                    );
                }
            }
            TopLevelOwnerKind::Alias => {
                if alias_owners_by_item.insert(key, *descriptor).is_some() {
                    reasons
                        .insert("Phase 1 contains duplicate type-alias owner descriptors".into());
                }
            }
            TopLevelOwnerKind::Refined => {
                if refined_owners_by_item.insert(key, *descriptor).is_some() {
                    reasons
                        .insert("Phase 1 contains duplicate refined-type owner descriptors".into());
                }
            }
            TopLevelOwnerKind::Const => {
                if const_owners_by_item.insert(key, *descriptor).is_some() {
                    reasons
                        .insert("Phase 1 contains duplicate constant owner descriptors".to_owned());
                }
            }
            TopLevelOwnerKind::Effect => {
                if effect_owners_by_item.insert(key, *descriptor).is_some() {
                    reasons
                        .insert("Phase 1 contains duplicate effect owner descriptors".to_owned());
                }
            }
            TopLevelOwnerKind::Test | TopLevelOwnerKind::Property => {
                if test_owners_by_item.insert(key, *descriptor).is_some() {
                    reasons.insert(
                        "Phase 1 contains duplicate test or property owner descriptors".to_owned(),
                    );
                }
            }
            _ => continue,
        }
        if !owner_ids.insert(descriptor.owner.def_id) {
            reasons.insert(
                "Phase 1 assigns one definition ID to multiple supported declarations".to_owned(),
            );
        }
    }
    for descriptor in &scopes.refinement_predicate_owners {
        refinement_predicate_owners_by_item
            .entry((descriptor.file, descriptor.item_id))
            .or_default()
            .push(*descriptor);
    }

    let mut seen_items = HashSet::new();
    let mut consumed_owners = HashSet::new();
    let mut consumed_record_owners = HashSet::new();
    let mut consumed_enum_owners = HashSet::new();
    let mut consumed_distinct_owners = HashSet::new();
    let mut consumed_const_owners = HashSet::new();
    let mut consumed_effect_owners = HashSet::new();
    let mut consumed_alias_owners = HashSet::new();
    let mut consumed_refined_owners = HashSet::new();
    let mut consumed_test_owners = HashSet::new();
    let mut consumed_refinement_predicate_owners = HashSet::new();
    let mut canonical_paths = HashSet::<String>::new();
    let mut current_symbols = Vec::new();

    for module in &prepared.module_tree.modules {
        let Some(input) = inputs_by_file.get(&module.file).copied() else {
            reasons.insert("a module tree entry has no exact target-filtered AST input".to_owned());
            continue;
        };
        let mut module_path = Vec::with_capacity(module.path.len());
        for segment in &module.path {
            match symbols.resolve(*segment) {
                Ok(name) if !name.is_empty() => module_path.push(name.to_string()),
                _ => {
                    reasons.insert("a module path segment cannot be resolved".to_owned());
                    module_path.clear();
                    break;
                }
            }
        }

        let mut test_name_disambiguators = HashMap::<(ItemTreeKind, String), u32>::new();
        let mut test_property_names = HashSet::<String>::new();
        for entry in &input.filtered.item_tree.items {
            match entry.kind {
                ItemTreeKind::Enum => {
                    collect_enum(
                        entry,
                        module.file,
                        &module_path,
                        package_version.package(),
                        input,
                        prepared.module_tree.package,
                        &enum_owners_by_item,
                        &mut seen_items,
                        &mut consumed_enum_owners,
                        &mut canonical_paths,
                        &mut current_symbols,
                        &mut reasons,
                        def_paths,
                        packages,
                        symbols,
                    );
                }
                ItemTreeKind::Distinct => {
                    collect_distinct(
                        entry,
                        module.file,
                        &module_path,
                        package_version.package(),
                        input,
                        prepared.module_tree.package,
                        &distinct_owners_by_item,
                        &mut seen_items,
                        &mut consumed_distinct_owners,
                        &mut canonical_paths,
                        &mut current_symbols,
                        &mut reasons,
                        def_paths,
                        packages,
                        symbols,
                    );
                }
                ItemTreeKind::Struct => {
                    collect_record(
                        entry,
                        module.file,
                        &module_path,
                        package_version.package(),
                        input,
                        prepared.module_tree.package,
                        &record_owners_by_item,
                        &mut seen_items,
                        &mut consumed_record_owners,
                        &mut canonical_paths,
                        &mut current_symbols,
                        &mut reasons,
                        def_paths,
                        packages,
                        symbols,
                    );
                }
                ItemTreeKind::Function => {
                    collect_function(
                        entry,
                        module.file,
                        &module_path,
                        package_version.package(),
                        input,
                        prepared.module_tree.package,
                        &owners_by_item,
                        &mut seen_items,
                        &mut consumed_owners,
                        &mut canonical_paths,
                        &mut current_symbols,
                        &mut reasons,
                        def_paths,
                        packages,
                        symbols,
                    );
                }
                ItemTreeKind::Alias => {
                    collect_alias(
                        entry,
                        module.file,
                        &module_path,
                        package_version.package(),
                        input,
                        prepared.module_tree.package,
                        &alias_owners_by_item,
                        &mut seen_items,
                        &mut consumed_alias_owners,
                        &mut canonical_paths,
                        &mut current_symbols,
                        &mut reasons,
                        def_paths,
                        packages,
                        symbols,
                    );
                }
                ItemTreeKind::Refined => {
                    collect_refined(
                        entry,
                        module.file,
                        &module_path,
                        package_version.package(),
                        input,
                        prepared.module_tree.package,
                        &refined_owners_by_item,
                        &refinement_predicate_owners_by_item,
                        &mut seen_items,
                        &mut consumed_refined_owners,
                        &mut consumed_refinement_predicate_owners,
                        &mut canonical_paths,
                        &mut current_symbols,
                        &mut reasons,
                        def_paths,
                        packages,
                        symbols,
                    );
                }
                ItemTreeKind::Const => {
                    collect_const(
                        entry,
                        module.file,
                        &module_path,
                        package_version.package(),
                        input,
                        prepared.module_tree.package,
                        &const_owners_by_item,
                        &mut seen_items,
                        &mut consumed_const_owners,
                        &mut canonical_paths,
                        &mut current_symbols,
                        &mut reasons,
                        def_paths,
                        packages,
                        symbols,
                    );
                }
                ItemTreeKind::Effect => {
                    collect_effect(
                        entry,
                        module.file,
                        &module_path,
                        package_version.package(),
                        input,
                        prepared.module_tree.package,
                        &effect_owners_by_item,
                        &mut seen_items,
                        &mut consumed_effect_owners,
                        &mut canonical_paths,
                        &mut current_symbols,
                        &mut reasons,
                        def_paths,
                        packages,
                        symbols,
                    );
                }
                ItemTreeKind::Test | ItemTreeKind::Property => {
                    collect_test_or_property(
                        entry,
                        module.file,
                        &module_path,
                        package_version.package(),
                        input,
                        prepared.module_tree.package,
                        &test_owners_by_item,
                        &mut seen_items,
                        &mut consumed_test_owners,
                        &mut canonical_paths,
                        &mut test_name_disambiguators,
                        &mut test_property_names,
                        &mut current_symbols,
                        &mut reasons,
                        def_paths,
                        packages,
                        symbols,
                    );
                }
                ItemTreeKind::Invariant => {
                    if !entry.children.is_empty() {
                        reasons.insert(
                            "an invariant entry has unexpected child declarations".to_owned(),
                        );
                    }
                }
                ItemTreeKind::ExternBlock if entry.children.is_empty() => {}
                // Every other top-level ItemTree declaration is either a
                // symbol outside this slice or an unknown shape. Extern
                // functions are children of ExternBlock and are symbols.
                _ => {
                    reasons.insert(format!(
                        "target-filtered declaration {:?} is outside the function-generic-parameter-record-generic-parameter-field-enum-generic-parameter-variant-record-variant-field-distinct-type-generic-parameter-type-alias-generic-parameter-refined-type-constant-effect-test-property inventory",
                        entry.kind
                    ));
                }
            }
        }
    }

    if consumed_owners.len() != owners_by_item.len() {
        reasons.insert(
            "the Phase 1 function owner descriptors do not exactly match the filtered ItemTree"
                .to_owned(),
        );
    }
    if consumed_record_owners.len() != record_owners_by_item.len() {
        reasons.insert(
            "the Phase 1 record owner descriptors do not exactly match the filtered ItemTree"
                .to_owned(),
        );
    }
    if consumed_enum_owners.len() != enum_owners_by_item.len() {
        reasons.insert(
            "the Phase 1 enum owner descriptors do not exactly match the filtered ItemTree"
                .to_owned(),
        );
    }
    if consumed_distinct_owners.len() != distinct_owners_by_item.len() {
        reasons.insert(
            "the Phase 1 distinct-type owner descriptors do not exactly match the filtered ItemTree"
                .to_owned(),
        );
    }
    if consumed_const_owners.len() != const_owners_by_item.len() {
        reasons.insert(
            "the Phase 1 constant owner descriptors do not exactly match the filtered ItemTree"
                .to_owned(),
        );
    }
    if consumed_effect_owners.len() != effect_owners_by_item.len() {
        reasons.insert(
            "the Phase 1 effect owner descriptors do not exactly match the filtered ItemTree"
                .to_owned(),
        );
    }
    if consumed_alias_owners.len() != alias_owners_by_item.len() {
        reasons.insert(
            "the Phase 1 type-alias owner descriptors do not exactly match the filtered ItemTree"
                .to_owned(),
        );
    }
    if consumed_refined_owners.len() != refined_owners_by_item.len() {
        reasons.insert(
            "the Phase 1 refined-type owner descriptors do not exactly match the filtered ItemTree"
                .to_owned(),
        );
    }
    if consumed_test_owners.len() != test_owners_by_item.len() {
        reasons.insert(
            "the Phase 1 test and property owner descriptors do not exactly match the filtered ItemTree"
                .to_owned(),
        );
    }
    for ((file_id, item_id), _) in &owners_by_item {
        if !seen_items.contains(&(*file_id, *item_id)) {
            reasons.insert(
                "a Phase 1 function owner has no target-filtered ItemTree declaration".to_owned(),
            );
        }
    }
    for ((file_id, item_id), _) in &record_owners_by_item {
        if !seen_items.contains(&(*file_id, *item_id)) {
            reasons.insert(
                "a Phase 1 record owner has no target-filtered ItemTree declaration".to_owned(),
            );
        }
    }
    for ((file_id, item_id), _) in &enum_owners_by_item {
        if !seen_items.contains(&(*file_id, *item_id)) {
            reasons.insert(
                "a Phase 1 enum owner has no target-filtered ItemTree declaration".to_owned(),
            );
        }
    }
    for ((file_id, item_id), _) in &distinct_owners_by_item {
        if !seen_items.contains(&(*file_id, *item_id)) {
            reasons.insert(
                "a Phase 1 distinct-type owner has no target-filtered ItemTree declaration"
                    .to_owned(),
            );
        }
    }
    for ((file_id, item_id), _) in &const_owners_by_item {
        if !seen_items.contains(&(*file_id, *item_id)) {
            reasons.insert(
                "a Phase 1 constant owner has no target-filtered ItemTree declaration".to_owned(),
            );
        }
    }
    for ((file_id, item_id), _) in &effect_owners_by_item {
        if !seen_items.contains(&(*file_id, *item_id)) {
            reasons.insert(
                "a Phase 1 effect owner has no target-filtered ItemTree declaration".to_owned(),
            );
        }
    }
    for ((file_id, item_id), _) in &alias_owners_by_item {
        if !seen_items.contains(&(*file_id, *item_id)) {
            reasons.insert(
                "a Phase 1 type-alias owner has no target-filtered ItemTree declaration".to_owned(),
            );
        }
    }
    for ((file_id, item_id), _) in &refined_owners_by_item {
        if !seen_items.contains(&(*file_id, *item_id)) {
            reasons.insert(
                "a Phase 1 refined-type owner has no target-filtered ItemTree declaration"
                    .to_owned(),
            );
        }
    }
    for ((file_id, item_id), _) in &test_owners_by_item {
        if !seen_items.contains(&(*file_id, *item_id)) {
            reasons.insert(
                "a Phase 1 test or property owner has no target-filtered ItemTree declaration"
                    .to_owned(),
            );
        }
    }
    for (key, descriptors) in &refinement_predicate_owners_by_item {
        if descriptors
            .iter()
            .any(|descriptor| descriptor.kind == RefinementPredicateOwnerKind::Refined)
            && !consumed_refinement_predicate_owners.contains(key)
        {
            reasons.insert(
                "a refined-type predicate owner has no matching target-filtered refined declaration"
                    .to_owned(),
            );
        }
    }

    if reasons.is_empty() {
        Phase1PackageSymbolInventory::Available(PackageSymbolInventory::complete(
            file,
            package_node_key,
            current_symbols,
        ))
    } else {
        let mut reasons = reasons.into_iter().collect::<Vec<_>>();
        reasons.sort();
        Phase1PackageSymbolInventory::Available(PackageSymbolInventory::incomplete(
            file,
            package_node_key,
            reasons.join("; "),
            current_symbols,
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_test_or_property(
    entry: &ItemTreeEntry,
    file_id: FileId,
    module_path: &[String],
    package_identity: &str,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owners_by_item: &HashMap<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>,
    seen_items: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    canonical_paths: &mut HashSet<String>,
    name_disambiguators: &mut HashMap<(ItemTreeKind, String), u32>,
    names_in_module: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    let key = (file_id, entry.ast_id);
    if !seen_items.insert(key) {
        reasons.insert(
            "the target-filtered ItemTree contains a duplicate test or property AST ID".to_owned(),
        );
    }
    if !entry.children.is_empty() {
        reasons.insert("a test or property entry has unexpected child declarations".to_owned());
    }

    let (declaration, name_literal, ast_id, expected_owner_kind, symbol_kind, component_name) =
        match entry.kind {
            ItemTreeKind::Test => {
                let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
                    reasons
                        .insert("a target-filtered test AST ID has no source pointer".to_owned());
                    return;
                };
                let Some(syntax) = pointer.resolve(file_id, &input.root) else {
                    reasons.insert(
                        "a target-filtered test source pointer cannot resolve in its AST root"
                            .to_owned(),
                    );
                    return;
                };
                let Some(declaration) = ast::TestDecl::cast(syntax) else {
                    reasons.insert(
                        "a target-filtered test source pointer resolves to a non-test node"
                            .to_owned(),
                    );
                    return;
                };
                let ast_id = input.ast_ids.ast_id(&declaration).map(|id| id.erase());
                let Some(name_literal) = declaration.name() else {
                    reasons.insert("a target-filtered test AST has no name string".to_owned());
                    return;
                };
                (
                    declaration.syntax().clone(),
                    name_literal,
                    ast_id,
                    TopLevelOwnerKind::Test,
                    SymbolKind::Test,
                    "test",
                )
            }
            ItemTreeKind::Property => {
                let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
                    reasons.insert(
                        "a target-filtered property AST ID has no source pointer".to_owned(),
                    );
                    return;
                };
                let Some(syntax) = pointer.resolve(file_id, &input.root) else {
                    reasons.insert(
                        "a target-filtered property source pointer cannot resolve in its AST root"
                            .to_owned(),
                    );
                    return;
                };
                let Some(declaration) = ast::PropertyDecl::cast(syntax) else {
                    reasons.insert(
                        "a target-filtered property source pointer resolves to a non-property node"
                            .to_owned(),
                    );
                    return;
                };
                let ast_id = input.ast_ids.ast_id(&declaration).map(|id| id.erase());
                let Some(name_literal) = declaration.name() else {
                    reasons.insert("a target-filtered property AST has no name string".to_owned());
                    return;
                };
                (
                    declaration.syntax().clone(),
                    name_literal,
                    ast_id,
                    TopLevelOwnerKind::Property,
                    SymbolKind::Property,
                    "property",
                )
            }
            _ => {
                reasons.insert(
                    "a non-test ItemTree declaration reached the test/property collector"
                        .to_owned(),
                );
                return;
            }
        };

    if ast_id != Some(entry.ast_id) {
        reasons.insert(
            "a test or property ItemTree AST ID does not exactly match its snapshot AST node"
                .to_owned(),
        );
        return;
    }
    let Some(descriptor) = owners_by_item.get(&key).copied() else {
        reasons.insert("a target-filtered test or property has no matching Phase 1 owner".into());
        return;
    };
    consumed_owners.insert(key);
    if descriptor.kind != expected_owner_kind {
        reasons.insert(
            "a test or property has a Phase 1 owner of the wrong declaration kind".to_owned(),
        );
        return;
    }

    let name_token = name_literal.syntax().clone();
    if syntax_has_errors(&declaration) || name_token.has_error() {
        reasons.insert(
            "a target-filtered test or property AST contains recovery or parse errors".to_owned(),
        );
        return;
    }
    if input
        .parser_diagnostics
        .iter()
        .any(|diagnostic| parser_diagnostic_overlaps_syntax(diagnostic, file_id, &declaration))
    {
        reasons.insert(
            "a test or property declaration has an overlapping parser or target diagnostic"
                .to_owned(),
        );
        return;
    }

    let name = name_literal.value().into_owned();
    if entry.name == Symbol::ERROR {
        reasons.insert("a target-filtered test or property has no valid ItemTree name".to_owned());
        return;
    }
    let item_tree_name = match symbols.resolve(entry.name) {
        Ok(name) => name.to_string(),
        _ => {
            reasons.insert(
                "a target-filtered test or property ItemTree name cannot be resolved".to_owned(),
            );
            return;
        }
    };
    if item_tree_name != name {
        reasons
            .insert("the test/property ItemTree name and decoded AST string disagree".to_owned());
        return;
    }

    let disambiguator = name_disambiguators
        .entry((entry.kind, name.clone()))
        .or_default();
    let expected_disambiguator = *disambiguator;
    let Some(next_disambiguator) = disambiguator.checked_add(1) else {
        reasons.insert("a test or property DefPath disambiguator overflowed".to_owned());
        return;
    };
    *disambiguator = next_disambiguator;

    if !names_in_module.insert(name.clone()) {
        reasons.insert(
            "duplicate test or property names remain in one target-filtered module".to_owned(),
        );
    }
    let invalid_name = name.is_empty() || name.chars().any(char::is_control);
    if invalid_name {
        reasons.insert(
            "a target-filtered test or property name is empty or contains a control character"
                .to_owned(),
        );
    }

    let canonical_string = canonical_test_string(&name);
    let canonical_path = format!(
        "{}{}.{component_name}[{canonical_string}]",
        package_identity,
        module_path
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect::<String>()
    );
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }

    let def_path = match def_paths.resolve(descriptor.owner.def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        Ok(Some(_)) => {
            reasons.insert(
                "a test or property owner resolves to a definition path from another package"
                    .to_owned(),
            );
            return;
        }
        Ok(None) => {
            reasons.insert("a test or property owner has no canonical definition path".to_owned());
            return;
        }
        Err(_) => {
            reasons
                .insert("a test or property owner definition path cannot be resolved".to_owned());
            return;
        }
    };
    let path_matches_declaration = def_path.data().len() == module_path.len() + 1
        && def_path
            .data()
            .iter()
            .take(module_path.len())
            .zip(module_path)
            .all(|(component, name)| {
                matches!(&component.data, DefPathData::Module(segment)
                    if symbols.resolve(*segment).is_ok_and(|resolved| resolved.as_ref() == name))
            })
        && def_path.data().last().is_some_and(|component| {
            component.disambiguator == expected_disambiguator
                && match (&entry.kind, &component.data) {
                    (ItemTreeKind::Test, DefPathData::Test(path_name)) => {
                        path_name.as_ref() == name
                    }
                    (ItemTreeKind::Property, DefPathData::Property(path_name)) => {
                        path_name.as_ref() == name
                    }
                    _ => false,
                }
        });
    if !path_matches_declaration {
        reasons.insert(
            "the test or property DefPath does not match its module, decoded name, kind, and disambiguator"
                .to_owned(),
        );
        return;
    }
    if invalid_name {
        return;
    }

    let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert("a test or property definition path hash cannot be computed".to_owned());
            return;
        }
    };
    let fingerprint = match declaration_fingerprint(symbol_kind, &declaration, Some(&name_token)) {
        Ok(fingerprint) => fingerprint,
        Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
            reasons.insert("a test or property string token is outside its declaration".to_owned());
            return;
        }
    };
    current_symbols.push(CurrentSymbol::new(
        canonical_path,
        def_path_hash,
        symbol_kind,
        fingerprint,
    ));
}

fn canonical_test_string(name: &str) -> String {
    let mut canonical = String::with_capacity(name.len() + 2);
    canonical.push('"');
    for character in name.chars() {
        match character {
            '\\' => canonical.push_str("\\\\"),
            '"' => canonical.push_str("\\\""),
            character => canonical.push(character),
        }
    }
    canonical.push('"');
    canonical
}

#[allow(clippy::too_many_arguments)]
fn collect_const(
    entry: &ItemTreeEntry,
    file_id: FileId,
    module_path: &[String],
    package_identity: &str,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owners_by_item: &HashMap<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>,
    seen_items: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    let key = (file_id, entry.ast_id);
    if !seen_items.insert(key) {
        reasons.insert("the target-filtered ItemTree contains a duplicate constant AST ID".into());
    }
    if !entry.children.is_empty() {
        reasons.insert("a constant entry has unexpected child declarations".to_owned());
    }
    if entry.name == Symbol::ERROR {
        reasons.insert("a target-filtered constant has no valid declaration name".to_owned());
        return;
    }
    let name = match symbols.resolve(entry.name) {
        Ok(name) if !name.is_empty() => name.to_string(),
        _ => {
            reasons.insert("a target-filtered constant name cannot be resolved".to_owned());
            return;
        }
    };
    let Some(descriptor) = owners_by_item.get(&key).copied() else {
        reasons.insert("a target-filtered constant has no matching Phase 1 owner".to_owned());
        return;
    };
    consumed_owners.insert(key);

    let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
        reasons.insert("a target-filtered constant AST ID has no source pointer".to_owned());
        return;
    };
    let Some(syntax) = pointer.resolve(file_id, &input.root) else {
        reasons.insert(
            "a target-filtered constant source pointer cannot resolve in its AST root".to_owned(),
        );
        return;
    };
    let Some(declaration) = ast::ConstDecl::cast(syntax.clone()) else {
        reasons.insert(
            "a target-filtered constant source pointer resolves to a non-constant node".to_owned(),
        );
        return;
    };
    let Some(name_node) = declaration.name() else {
        reasons.insert("a target-filtered constant AST has no name node".to_owned());
        return;
    };
    let Some(name_token) = name_node.token() else {
        reasons.insert("a target-filtered constant AST has no name token".to_owned());
        return;
    };
    let raw_name = name_token
        .text()
        .strip_prefix("r#")
        .unwrap_or(name_token.text());
    if sai_syntax::normalize_identifier(raw_name) != name {
        reasons
            .insert("the ItemTree and constant AST disagree about the declaration name".to_owned());
        return;
    }
    if syntax_has_errors(&syntax) || name_token.has_error() {
        reasons
            .insert("a target-filtered constant AST contains recovery or parse errors".to_owned());
        return;
    }

    let canonical_path = format!(
        "{}{}.{name}",
        package_identity,
        module_path
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect::<String>()
    );
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }

    let def_path = match def_paths.resolve(descriptor.owner.def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        Ok(Some(_)) => {
            reasons.insert(
                "a constant owner resolves to a definition path from another package".to_owned(),
            );
            return;
        }
        Ok(None) => {
            reasons.insert("a constant owner has no canonical definition path".to_owned());
            return;
        }
        Err(_) => {
            reasons.insert("a constant owner definition path cannot be resolved".to_owned());
            return;
        }
    };
    let path_matches_declaration = def_path.data().len() == module_path.len() + 1
        && def_path
            .data()
            .iter()
            .take(module_path.len())
            .zip(module_path)
            .all(|(component, name)| {
                matches!(&component.data, DefPathData::Module(segment)
                    if symbols.resolve(*segment).is_ok_and(|resolved| resolved.as_ref() == name))
            })
        && matches!(
            def_path.data().last().map(|component| &component.data),
            Some(DefPathData::ValueNs(symbol)) if *symbol == entry.name
        );
    if !path_matches_declaration {
        reasons.insert(
            "the canonical constant definition path does not match its module and ItemTree declaration"
                .to_owned(),
        );
        return;
    }
    let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert("a constant definition path hash cannot be computed".to_owned());
            return;
        }
    };
    let fingerprint = match declaration_fingerprint(SymbolKind::Const, &syntax, Some(&name_token)) {
        Ok(fingerprint) => fingerprint,
        Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
            reasons
                .insert("a constant declaration name token is outside its syntax node".to_owned());
            return;
        }
    };
    current_symbols.push(CurrentSymbol::new(
        canonical_path,
        def_path_hash,
        SymbolKind::Const,
        fingerprint,
    ));
}

#[allow(clippy::too_many_arguments)]
fn collect_record(
    entry: &ItemTreeEntry,
    file_id: FileId,
    module_path: &[String],
    package_identity: &str,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owners_by_item: &HashMap<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>,
    seen_items: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    #[derive(Clone)]
    enum RecordChild {
        Generic(ast::GenericParam),
        Field(ast::FieldDecl),
    }

    let key = (file_id, entry.ast_id);
    if !seen_items.insert(key) {
        reasons.insert("the target-filtered ItemTree contains a duplicate record AST ID".into());
    }
    if entry.kind != ItemTreeKind::Struct {
        reasons.insert("a non-record ItemTree entry reached the record collector".into());
        return;
    }
    if entry.name == Symbol::ERROR {
        reasons.insert("a target-filtered record has no valid declaration name".into());
        return;
    }
    let name = match symbols.resolve(entry.name) {
        Ok(name) if !name.is_empty() => name.to_string(),
        _ => {
            reasons.insert("a target-filtered record name cannot be resolved".to_owned());
            return;
        }
    };
    let Some(descriptor) = owners_by_item.get(&key).copied() else {
        reasons.insert("a target-filtered record has no matching Phase 1 owner".to_owned());
        return;
    };
    if descriptor.kind != TopLevelOwnerKind::Struct
        || descriptor.file != file_id
        || descriptor.item_id != entry.ast_id
    {
        reasons.insert("a target-filtered record has a mismatched Phase 1 owner".to_owned());
        return;
    }
    consumed_owners.insert(key);

    let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
        reasons.insert("a target-filtered record AST ID has no source pointer".to_owned());
        return;
    };
    let Some(syntax) = pointer.resolve(file_id, &input.root) else {
        reasons.insert(
            "a target-filtered record source pointer cannot resolve in its AST root".to_owned(),
        );
        return;
    };
    let Some(declaration) = ast::TypeDecl::cast(syntax.clone()) else {
        reasons.insert(
            "a target-filtered record source pointer resolves to a non-type node".to_owned(),
        );
        return;
    };
    let Some(definition) = declaration.struct_def() else {
        reasons.insert("a target-filtered record has no struct definition".to_owned());
        return;
    };
    if declaration.alias_def().is_some() {
        reasons.insert("a target-filtered record has an unsupported type form".to_owned());
        return;
    }
    if input.ast_ids.ast_id(&declaration).map(|id| id.erase()) != Some(entry.ast_id) {
        reasons.insert("a target-filtered record AST ID does not match its declaration".into());
        return;
    }
    let Some(name_node) = declaration.name() else {
        reasons.insert("a target-filtered record AST has no name node".to_owned());
        return;
    };
    let Some(name_token) = name_node.token() else {
        reasons.insert("a target-filtered record AST has no name token".to_owned());
        return;
    };
    let raw_name = name_token
        .text()
        .strip_prefix("r#")
        .unwrap_or(name_token.text());
    if sai_syntax::normalize_identifier(raw_name) != name {
        reasons.insert("the ItemTree and record AST disagree about the declaration name".into());
        return;
    }
    if syntax_has_errors(&syntax) || name_token.has_error() {
        reasons.insert("a target-filtered record AST contains recovery or parse errors".into());
        return;
    }
    if entry
        .raw_attrs
        .iter()
        .any(|attribute| attribute.name == sai_hir::item_tree::AttrName::Derive)
    {
        reasons
            .insert("a record has derived impl symbols outside the supported inventory".to_owned());
    }

    // Record ItemTree children are a source-ordered merge of generic
    // parameters and fields. Validate that complete sequence before passing
    // the generic-only subset to the shared generic collector.
    let mut expected_children = Vec::<(usize, RecordChild)>::new();
    if let Some(list) = declaration.generic_params() {
        expected_children.extend(list.params().map(|parameter| {
            (
                parameter.syntax().text_range().start().to_usize(),
                RecordChild::Generic(parameter),
            )
        }));
    }
    if let Some(list) = definition.fields() {
        expected_children.extend(list.fields().map(|field| {
            (
                field.syntax().text_range().start().to_usize(),
                RecordChild::Field(field),
            )
        }));
    }
    expected_children.sort_by_key(|(offset, _)| *offset);
    if entry.children.len() != expected_children.len() {
        reasons
            .insert("a record's ItemTree child count does not match its snapshot AST".to_owned());
        return;
    }

    let mut generic_children = Vec::<ItemTreeEntry>::new();
    let mut validated_fields = Vec::<(
        ItemTreeEntry,
        ast::FieldDecl,
        String,
        SyntaxNode,
        sai_syntax::SyntaxToken,
    )>::new();
    let mut field_names = HashSet::<String>::new();
    for ((_, ast_child), child) in expected_children.iter().zip(&entry.children) {
        let (expected_kind, ast_id, name_node, child_syntax) = match ast_child {
            RecordChild::Generic(ast::GenericParam::Type(parameter)) => (
                ItemTreeKind::TypeParam,
                input.ast_ids.ast_id(parameter).map(|id| id.erase()),
                parameter.name(),
                parameter.syntax().clone(),
            ),
            RecordChild::Generic(ast::GenericParam::Const(parameter)) => (
                ItemTreeKind::ConstParam,
                input.ast_ids.ast_id(parameter).map(|id| id.erase()),
                parameter.name(),
                parameter.syntax().clone(),
            ),
            RecordChild::Field(field) => (
                ItemTreeKind::Field,
                input.ast_ids.ast_id(field).map(|id| id.erase()),
                field.name(),
                field.syntax().clone(),
            ),
        };
        if child.kind != expected_kind {
            reasons.insert(
                "a record's ItemTree child kind or source order does not match its snapshot AST"
                    .to_owned(),
            );
            return;
        }
        if ast_id != Some(child.ast_id) || input.ast_ids.pointer(child.ast_id).is_none() {
            reasons
                .insert("a record's ItemTree child ID does not match its snapshot AST".to_owned());
            return;
        }
        if !child.children.is_empty() {
            reasons.insert("a record child has unexpected nested declarations".to_owned());
            return;
        }
        if child.name == Symbol::ERROR {
            reasons.insert("a record child has no valid ItemTree name".to_owned());
            return;
        }
        let child_name = match symbols.resolve(child.name) {
            Ok(name) if !name.is_empty() => name.to_string(),
            _ => {
                reasons.insert("a record child name cannot be resolved".to_owned());
                return;
            }
        };
        let Some(name_node) = name_node else {
            reasons.insert("a record child AST has no name node".to_owned());
            return;
        };
        let Some(child_name_token) = name_node.token() else {
            reasons.insert("a record child AST has no name token".to_owned());
            return;
        };
        let raw_child_name = child_name_token
            .text()
            .strip_prefix("r#")
            .unwrap_or(child_name_token.text());
        if sai_syntax::normalize_identifier(raw_child_name) != child_name {
            reasons.insert("the record ItemTree and AST child names disagree".to_owned());
            return;
        }
        if syntax_has_errors(&child_syntax) || child_name_token.has_error() {
            reasons.insert("a record child AST contains recovery or parse errors".to_owned());
            return;
        }
        match ast_child {
            RecordChild::Generic(_) => generic_children.push(child.clone()),
            RecordChild::Field(field) => {
                if !field_names.insert(child_name.clone()) {
                    reasons.insert(format!(
                        "record `{name}` has duplicate field name `{child_name}`"
                    ));
                }
                validated_fields.push((
                    child.clone(),
                    field.clone(),
                    child_name,
                    child_syntax,
                    child_name_token,
                ));
            }
        }
    }

    let canonical_path = format!(
        "{}{}.{name}",
        package_identity,
        module_path
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect::<String>()
    );
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }

    let owner_component = DefPathData::TypeNs(entry.name);
    let Some(item_position) = input
        .filtered
        .item_tree
        .items
        .iter()
        .position(|candidate| candidate.ast_id == entry.ast_id)
    else {
        reasons.insert("a target-filtered record is absent from its module ItemTree".to_owned());
        return;
    };
    let owner_disambiguator = input.filtered.item_tree.items[..item_position]
        .iter()
        .filter(|candidate| DefPathData::TypeNs(candidate.name) == owner_component)
        .count();
    let Ok(owner_disambiguator) = u32::try_from(owner_disambiguator) else {
        reasons.insert("a record owner DefPath disambiguator overflowed".to_owned());
        return;
    };
    let def_path = match def_paths.resolve(descriptor.owner.def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        Ok(Some(_)) => {
            reasons
                .insert("a record owner resolves to a definition path from another package".into());
            return;
        }
        Ok(None) => {
            reasons.insert("a record owner has no canonical definition path".to_owned());
            return;
        }
        Err(_) => {
            reasons.insert("a record owner definition path cannot be resolved".to_owned());
            return;
        }
    };
    let path_matches_declaration = def_path.data().len() == module_path.len() + 1
        && def_path
            .data()
            .iter()
            .take(module_path.len())
            .zip(module_path)
            .all(|(component, name)| {
                component.disambiguator == 0
                    && matches!(&component.data, DefPathData::Module(segment)
                        if symbols.resolve(*segment).is_ok_and(|resolved| resolved.as_ref() == name))
            })
        && def_path.data().last().is_some_and(|component| {
            component.data == owner_component && component.disambiguator == owner_disambiguator
        });
    if !path_matches_declaration {
        reasons.insert(
            "the record DefPath does not match its module, TypeNs name, or disambiguator"
                .to_owned(),
        );
        return;
    }
    let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert("a record definition path hash cannot be computed".to_owned());
            return;
        }
    };
    let fingerprint = match declaration_fingerprint(SymbolKind::Record, &syntax, Some(&name_token))
    {
        Ok(fingerprint) => fingerprint,
        Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
            reasons.insert("a record name token is outside its declaration syntax node".into());
            return;
        }
    };
    current_symbols.push(CurrentSymbol::new(
        canonical_path.clone(),
        def_path_hash,
        SymbolKind::Record,
        fingerprint,
    ));

    collect_item_generic_params(
        &generic_children,
        declaration.generic_params(),
        &canonical_path,
        descriptor.owner.def_id,
        &def_path,
        input,
        package_id,
        "record",
        canonical_paths,
        current_symbols,
        reasons,
        def_paths,
        packages,
        symbols,
    );

    let mut next_disambiguator = HashMap::<Symbol, u32>::new();
    let mut field_paths = Vec::with_capacity(validated_fields.len());
    for (field, _, _, _, _) in &validated_fields {
        let disambiguator = next_disambiguator.entry(field.name).or_default();
        let ordinal = *disambiguator;
        let Some(next) = ordinal.checked_add(1) else {
            reasons.insert("record field DefPath disambiguator overflowed".to_owned());
            return;
        };
        *disambiguator = next;
        field_paths.push((field.name, ordinal));
    }
    let field_def_ids = match def_paths.intern_fields(descriptor.owner.def_id, &field_paths) {
        Ok(def_ids) if def_ids.len() == validated_fields.len() => def_ids,
        Ok(_) => {
            reasons.insert("HIR returned the wrong number of record field definitions".to_owned());
            return;
        }
        Err(_) => {
            reasons.insert("record field definitions cannot be interned".to_owned());
            return;
        }
    };
    for (
        ((field_entry, _, field_name, field_syntax, field_name_token), field_def_id),
        (_, ordinal),
    ) in validated_fields.iter().zip(field_def_ids).zip(field_paths)
    {
        let field_canonical_path = format!("{canonical_path}.field[{field_name}]");
        if !collect_named_field_symbol(
            field_entry,
            field_syntax,
            field_name_token,
            field_def_id,
            &def_path,
            ordinal,
            field_canonical_path,
            "record field",
            package_id,
            reasons,
            canonical_paths,
            current_symbols,
            def_paths,
            packages,
            symbols,
        ) {
            return;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_enum(
    entry: &ItemTreeEntry,
    file_id: FileId,
    module_path: &[String],
    package_identity: &str,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owners_by_item: &HashMap<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>,
    seen_items: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    enum EnumChild {
        Generic(ast::GenericParam),
        Variant(ast::VariantDecl),
    }

    struct EnumVariantField {
        entry: ItemTreeEntry,
        name: String,
        syntax: SyntaxNode,
        name_token: sai_syntax::SyntaxToken,
    }

    struct EnumVariant {
        entry: ItemTreeEntry,
        name: String,
        syntax: SyntaxNode,
        name_token: sai_syntax::SyntaxToken,
        fields: Vec<EnumVariantField>,
    }

    let key = (file_id, entry.ast_id);
    if !seen_items.insert(key) {
        reasons.insert("the target-filtered ItemTree contains a duplicate enum AST ID".into());
    }
    if entry.kind != ItemTreeKind::Enum {
        reasons.insert("a non-enum ItemTree entry reached the enum collector".into());
        return;
    }
    if entry.name == Symbol::ERROR {
        reasons.insert("a target-filtered enum has no valid declaration name".into());
        return;
    }
    let name = match symbols.resolve(entry.name) {
        Ok(name) if !name.is_empty() => name.to_string(),
        _ => {
            reasons.insert("a target-filtered enum name cannot be resolved".to_owned());
            return;
        }
    };
    let Some(descriptor) = owners_by_item.get(&key).copied() else {
        reasons.insert("a target-filtered enum has no matching Phase 1 owner".to_owned());
        return;
    };
    if descriptor.kind != TopLevelOwnerKind::Enum
        || descriptor.file != file_id
        || descriptor.item_id != entry.ast_id
    {
        reasons.insert("a target-filtered enum has a mismatched Phase 1 owner".to_owned());
        return;
    }
    consumed_owners.insert(key);

    let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
        reasons.insert("a target-filtered enum AST ID has no source pointer".to_owned());
        return;
    };
    let Some(syntax) = pointer.resolve(file_id, &input.root) else {
        reasons.insert(
            "a target-filtered enum source pointer cannot resolve in its AST root".to_owned(),
        );
        return;
    };
    let Some(declaration) = ast::EnumDecl::cast(syntax.clone()) else {
        reasons
            .insert("a target-filtered enum source pointer resolves to a non-enum node".to_owned());
        return;
    };
    if input.ast_ids.ast_id(&declaration).map(|id| id.erase()) != Some(entry.ast_id) {
        reasons.insert("a target-filtered enum AST ID does not match its declaration".into());
        return;
    }
    let Some(name_node) = declaration.name() else {
        reasons.insert("a target-filtered enum AST has no name node".to_owned());
        return;
    };
    let Some(name_token) = name_node.token() else {
        reasons.insert("a target-filtered enum AST has no name token".to_owned());
        return;
    };
    let raw_name = name_token
        .text()
        .strip_prefix("r#")
        .unwrap_or(name_token.text());
    if sai_syntax::normalize_identifier(raw_name) != name {
        reasons.insert("the ItemTree and enum AST disagree about the declaration name".into());
        return;
    }
    if syntax_has_errors(&syntax) || name_token.has_error() {
        reasons.insert("a target-filtered enum AST contains recovery or parse errors".into());
        return;
    }
    if input
        .parser_diagnostics
        .iter()
        .any(|diagnostic| parser_diagnostic_overlaps_syntax(diagnostic, file_id, &syntax))
    {
        reasons.insert("an enum syntax node has a parser diagnostic".to_owned());
        return;
    }
    if entry
        .raw_attrs
        .iter()
        .any(|attribute| attribute.name == sai_hir::item_tree::AttrName::Derive)
    {
        reasons
            .insert("an enum has derived impl symbols outside the supported inventory".to_owned());
    }

    // Enum ItemTree children merge declaration generics with variants in
    // source order. Validate that complete sequence, then validate each
    // variant's direct record fields against its own AST child list.
    let mut expected_children = Vec::<(usize, EnumChild)>::new();
    if let Some(list) = declaration.generic_params() {
        expected_children.extend(list.params().map(|parameter| {
            (
                parameter.syntax().text_range().start().to_usize(),
                EnumChild::Generic(parameter),
            )
        }));
    }
    if let Some(variants) = declaration.variants() {
        expected_children.extend(variants.variants().map(|variant| {
            (
                variant.syntax().text_range().start().to_usize(),
                EnumChild::Variant(variant),
            )
        }));
    }
    expected_children.sort_by_key(|(offset, _)| *offset);
    if entry.children.len() != expected_children.len() {
        reasons.insert("an enum's ItemTree child count does not match its snapshot AST".into());
        return;
    }

    let mut generic_children = Vec::<ItemTreeEntry>::new();
    let mut variants = Vec::<EnumVariant>::new();
    for ((_, ast_child), child) in expected_children.iter().zip(&entry.children) {
        let (expected_kind, ast_id, name_node, child_syntax) = match ast_child {
            EnumChild::Generic(ast::GenericParam::Type(parameter)) => (
                ItemTreeKind::TypeParam,
                input.ast_ids.ast_id(parameter).map(|id| id.erase()),
                parameter.name(),
                parameter.syntax().clone(),
            ),
            EnumChild::Generic(ast::GenericParam::Const(parameter)) => (
                ItemTreeKind::ConstParam,
                input.ast_ids.ast_id(parameter).map(|id| id.erase()),
                parameter.name(),
                parameter.syntax().clone(),
            ),
            EnumChild::Variant(variant) => (
                ItemTreeKind::Variant,
                input.ast_ids.ast_id(variant).map(|id| id.erase()),
                variant.name(),
                variant.syntax().clone(),
            ),
        };
        if child.kind != expected_kind {
            reasons.insert(
                "an enum's ItemTree child kind or source order does not match its snapshot AST"
                    .to_owned(),
            );
            return;
        }
        if ast_id != Some(child.ast_id) {
            reasons.insert("an enum ItemTree child ID does not match its snapshot AST".into());
            return;
        }
        let Some(child_pointer) = input.ast_ids.pointer(child.ast_id) else {
            reasons.insert("an enum ItemTree child ID has no source pointer".into());
            return;
        };
        let Some(expected_child_pointer) =
            sai_syntax::SyntaxNodePtr::new(file_id, &child_syntax).ok()
        else {
            reasons.insert("an enum AST child cannot be bound to its source file".into());
            return;
        };
        if !child_pointer.is_same_source_node(&expected_child_pointer) {
            reasons.insert(
                "an enum ItemTree child source pointer identifies a different AST node".into(),
            );
            return;
        }
        if child.name == Symbol::ERROR {
            reasons.insert("an enum ItemTree child has no valid name".to_owned());
            return;
        }
        let child_name = match symbols.resolve(child.name) {
            Ok(name) if !name.is_empty() => name.to_string(),
            _ => {
                reasons.insert("an enum ItemTree child name cannot be resolved".to_owned());
                return;
            }
        };
        let Some(name_node) = name_node else {
            reasons.insert("an enum AST child has no name node".to_owned());
            return;
        };
        let Some(child_name_token) = name_node.token() else {
            reasons.insert("an enum AST child has no name token".to_owned());
            return;
        };
        let raw_child_name = child_name_token
            .text()
            .strip_prefix("r#")
            .unwrap_or(child_name_token.text());
        if sai_syntax::normalize_identifier(raw_child_name) != child_name {
            reasons.insert("the enum ItemTree and AST child names disagree".to_owned());
            return;
        }
        if syntax_has_errors(&child_syntax) || child_name_token.has_error() {
            reasons.insert("an enum AST child contains recovery or parse errors".to_owned());
            return;
        }
        if input
            .parser_diagnostics
            .iter()
            .any(|diagnostic| parser_diagnostic_overlaps_syntax(diagnostic, file_id, &child_syntax))
        {
            reasons.insert("an enum child syntax node has a parser diagnostic".to_owned());
            return;
        }

        match ast_child {
            EnumChild::Generic(_) => {
                if !child.children.is_empty() {
                    reasons.insert(
                        "an enum generic parameter has unexpected child declarations".into(),
                    );
                    return;
                }
                generic_children.push(child.clone());
            }
            EnumChild::Variant(variant) => {
                let mut fields = Vec::new();
                if let Some(shape) = variant.record_shape() {
                    let ast_fields = shape.fields().collect::<Vec<_>>();
                    if child.children.len() != ast_fields.len() {
                        reasons.insert(
                            "a record variant's ItemTree field count does not match its snapshot AST"
                                .to_owned(),
                        );
                        return;
                    }
                    let mut field_names = HashSet::<String>::new();
                    for (field, field_entry) in ast_fields.iter().zip(&child.children) {
                        let Some(field_id) = input.ast_ids.ast_id(field).map(|id| id.erase())
                        else {
                            reasons
                                .insert("a record variant field has no snapshot AST ID".to_owned());
                            return;
                        };
                        if field_entry.kind != ItemTreeKind::VariantField {
                            reasons.insert(
                                "a record variant ItemTree field kind or order does not match its snapshot AST"
                                    .to_owned(),
                            );
                            return;
                        }
                        if field_entry.ast_id != field_id {
                            reasons.insert(
                                "a record variant ItemTree field ID does not match its snapshot AST"
                                    .to_owned(),
                            );
                            return;
                        }
                        let Some(field_pointer) = input.ast_ids.pointer(field_entry.ast_id) else {
                            reasons.insert(
                                "a record variant field ID has no source pointer".to_owned(),
                            );
                            return;
                        };
                        let Some(expected_field_pointer) =
                            sai_syntax::SyntaxNodePtr::new(file_id, field.syntax()).ok()
                        else {
                            reasons.insert(
                                "a record variant field AST cannot be bound to its source file"
                                    .to_owned(),
                            );
                            return;
                        };
                        if !field_pointer.is_same_source_node(&expected_field_pointer) {
                            reasons.insert(
                                "a record variant field source pointer identifies a different AST node"
                                    .to_owned(),
                            );
                            return;
                        }
                        if !field_entry.children.is_empty() {
                            reasons.insert(
                                "a record variant field has unexpected nested declarations"
                                    .to_owned(),
                            );
                            return;
                        }
                        if field_entry.name == Symbol::ERROR {
                            reasons.insert(
                                "a record variant field has no valid ItemTree name".to_owned(),
                            );
                            return;
                        }
                        let field_name = match symbols.resolve(field_entry.name) {
                            Ok(name) if !name.is_empty() => name.to_string(),
                            _ => {
                                reasons.insert(
                                    "a record variant field name cannot be resolved".to_owned(),
                                );
                                return;
                            }
                        };
                        let Some(field_name_node) = field.name() else {
                            reasons
                                .insert("a record variant field AST has no name node".to_owned());
                            return;
                        };
                        let Some(field_name_token) = field_name_node.token() else {
                            reasons
                                .insert("a record variant field AST has no name token".to_owned());
                            return;
                        };
                        let raw_field_name = field_name_token
                            .text()
                            .strip_prefix("r#")
                            .unwrap_or(field_name_token.text());
                        if sai_syntax::normalize_identifier(raw_field_name) != field_name {
                            reasons.insert(
                                "the record variant ItemTree and AST field names disagree"
                                    .to_owned(),
                            );
                            return;
                        }
                        if !field_names.insert(field_name.clone()) {
                            reasons.insert(format!(
                                "enum `{name}` variant `{child_name}` has duplicate field name `{field_name}`"
                            ));
                        }
                        if field.ty().is_none() {
                            reasons
                                .insert("a record variant field has no declared type".to_owned());
                            return;
                        }
                        if syntax_has_errors(field.syntax()) || field_name_token.has_error() {
                            reasons.insert(
                                "a record variant field AST contains recovery or parse errors"
                                    .to_owned(),
                            );
                            return;
                        }
                        fields.push(EnumVariantField {
                            entry: field_entry.clone(),
                            name: field_name,
                            syntax: field.syntax().clone(),
                            name_token: field_name_token,
                        });
                    }
                } else {
                    if !child.children.is_empty() {
                        reasons.insert(
                            "a unit or tuple enum variant has unexpected ItemTree children".into(),
                        );
                        return;
                    }
                    if variant.tuple_shape().is_some() {
                        reasons.insert(
                            "tuple variant payload symbols are outside the supported enum inventory"
                                .to_owned(),
                        );
                    }
                }
                variants.push(EnumVariant {
                    entry: child.clone(),
                    name: child_name,
                    syntax: child_syntax,
                    name_token: child_name_token,
                    fields,
                });
            }
        }
    }

    let canonical_path = format!(
        "{}{}.{name}",
        package_identity,
        module_path
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect::<String>()
    );
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }

    let owner_component = DefPathData::TypeNs(entry.name);
    let Some(item_position) = input
        .filtered
        .item_tree
        .items
        .iter()
        .position(|candidate| candidate.ast_id == entry.ast_id)
    else {
        reasons.insert("a target-filtered enum is absent from its module ItemTree".to_owned());
        return;
    };
    let owner_disambiguator = input.filtered.item_tree.items[..item_position]
        .iter()
        .filter(|candidate| DefPathData::TypeNs(candidate.name) == owner_component)
        .count();
    let Ok(owner_disambiguator) = u32::try_from(owner_disambiguator) else {
        reasons.insert("an enum owner DefPath disambiguator overflowed".to_owned());
        return;
    };
    let def_path = match def_paths.resolve(descriptor.owner.def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        Ok(Some(_)) => {
            reasons
                .insert("an enum owner resolves to a definition path from another package".into());
            return;
        }
        Ok(None) => {
            reasons.insert("an enum owner has no canonical definition path".to_owned());
            return;
        }
        Err(_) => {
            reasons.insert("an enum owner definition path cannot be resolved".to_owned());
            return;
        }
    };
    let path_matches_declaration = def_path.data().len() == module_path.len() + 1
        && def_path
            .data()
            .iter()
            .take(module_path.len())
            .zip(module_path)
            .all(|(component, name)| {
                component.disambiguator == 0
                    && matches!(&component.data, DefPathData::Module(segment)
                        if symbols.resolve(*segment).is_ok_and(|resolved| resolved.as_ref() == name))
            })
        && def_path.data().last().is_some_and(|component| {
            component.data == owner_component && component.disambiguator == owner_disambiguator
        });
    if !path_matches_declaration {
        reasons.insert(
            "the enum DefPath does not match its module, TypeNs name, or disambiguator".into(),
        );
        return;
    }
    let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert("an enum definition path hash cannot be computed".to_owned());
            return;
        }
    };
    let fingerprint = match declaration_fingerprint(SymbolKind::Enum, &syntax, Some(&name_token)) {
        Ok(fingerprint) => fingerprint,
        Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
            reasons.insert("an enum name token is outside its declaration syntax node".into());
            return;
        }
    };
    current_symbols.push(CurrentSymbol::new(
        canonical_path.clone(),
        def_path_hash,
        SymbolKind::Enum,
        fingerprint,
    ));

    collect_item_generic_params(
        &generic_children,
        declaration.generic_params(),
        &canonical_path,
        descriptor.owner.def_id,
        &def_path,
        input,
        package_id,
        "enum",
        canonical_paths,
        current_symbols,
        reasons,
        def_paths,
        packages,
        symbols,
    );

    let mut variant_disambiguators = HashMap::<Symbol, u32>::new();
    let mut variant_paths = Vec::with_capacity(variants.len());
    for variant in &variants {
        let disambiguator = variant_disambiguators
            .entry(variant.entry.name)
            .or_default();
        let ordinal = *disambiguator;
        let Some(next) = ordinal.checked_add(1) else {
            reasons.insert("enum variant DefPath disambiguator overflowed".to_owned());
            return;
        };
        *disambiguator = next;
        variant_paths.push((variant.entry.name, ordinal));
    }
    let variant_def_ids = match def_paths.intern_variants(descriptor.owner.def_id, &variant_paths) {
        Ok(def_ids) if def_ids.len() == variants.len() => def_ids,
        Ok(_) => {
            reasons.insert("HIR returned the wrong number of enum variant definitions".into());
            return;
        }
        Err(_) => {
            reasons.insert("enum variant definitions cannot be interned".to_owned());
            return;
        }
    };
    let mut variant_symbols = Vec::with_capacity(variants.len());
    let mut variant_def_paths = Vec::with_capacity(variants.len());
    for ((variant, def_id), (_, ordinal)) in
        variants.iter().zip(&variant_def_ids).zip(&variant_paths)
    {
        let variant_def_path = match def_paths.resolve(*def_id) {
            Ok(Some(path)) if path.package() == package_id => path,
            _ => {
                reasons.insert(
                    "an enum variant has no definition path in the selected package".into(),
                );
                return;
            }
        };
        let parent_len = def_path.data().len();
        let exact_path = variant_def_path.data().len() == parent_len + 1
            && &variant_def_path.data()[..parent_len] == def_path.data()
            && variant_def_path.data().last().is_some_and(|component| {
                component.data == DefPathData::Variant(variant.entry.name)
                    && component.disambiguator == *ordinal
            });
        if !exact_path {
            reasons.insert(
                "an enum variant DefPath disagrees with its parent, name, or sibling order".into(),
            );
            return;
        }
        let variant_def_path_hash = match DefPathHash::compute(&variant_def_path, packages, symbols)
        {
            Ok(hash) => Fingerprint::new(hash.raw()),
            Err(_) => {
                reasons.insert("an enum variant definition path hash cannot be computed".into());
                return;
            }
        };
        let variant_fingerprint = match declaration_fingerprint(
            SymbolKind::Variant,
            &variant.syntax,
            Some(&variant.name_token),
        ) {
            Ok(fingerprint) => fingerprint,
            Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
                reasons.insert(
                    "an enum variant name token is outside its declaration syntax node".into(),
                );
                return;
            }
        };
        let variant_canonical_path = format!("{canonical_path}.{}", variant.name);
        if !canonical_paths.insert(variant_canonical_path.clone()) {
            reasons.insert(format!(
                "duplicate canonical symbol path {variant_canonical_path}"
            ));
        }
        variant_symbols.push(CurrentSymbol::new(
            variant_canonical_path,
            variant_def_path_hash,
            SymbolKind::Variant,
            variant_fingerprint,
        ));
        variant_def_paths.push(variant_def_path);
    }
    current_symbols.extend(variant_symbols);

    let mut field_disambiguators = Vec::<u32>::new();
    let mut field_paths = Vec::<(sai_hir::LocalDefId, Symbol, u32)>::new();
    for (variant_index, variant) in variants.iter().enumerate() {
        let variant_def_id = variant_def_ids[variant_index];
        let mut next_disambiguator = HashMap::<Symbol, u32>::new();
        for field in &variant.fields {
            let disambiguator = next_disambiguator.entry(field.entry.name).or_default();
            let ordinal = *disambiguator;
            let Some(next) = ordinal.checked_add(1) else {
                reasons.insert("record variant field DefPath disambiguator overflowed".into());
                return;
            };
            *disambiguator = next;
            field_disambiguators.push(ordinal);
            field_paths.push((variant_def_id, field.entry.name, ordinal));
        }
    }
    let field_def_ids = match def_paths.intern_fields_for_parents(&field_paths) {
        Ok(def_ids) if def_ids.len() == field_paths.len() => def_ids,
        Ok(_) => {
            reasons.insert("HIR returned the wrong number of record variant fields".into());
            return;
        }
        Err(_) => {
            reasons.insert("record variant fields cannot be interned".to_owned());
            return;
        }
    };
    let mut field_def_id_index = 0usize;
    for (variant, variant_def_path) in variants.iter().zip(&variant_def_paths) {
        for field in &variant.fields {
            let field_def_id = field_def_ids[field_def_id_index];
            let ordinal = field_disambiguators[field_def_id_index];
            field_def_id_index += 1;
            let field_canonical_path =
                format!("{canonical_path}.{}.field[{}]", variant.name, field.name);
            if !collect_named_field_symbol(
                &field.entry,
                &field.syntax,
                &field.name_token,
                field_def_id,
                variant_def_path,
                ordinal,
                field_canonical_path,
                "record variant field",
                package_id,
                reasons,
                canonical_paths,
                current_symbols,
                def_paths,
                packages,
                symbols,
            ) {
                return;
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_named_field_symbol(
    field_entry: &ItemTreeEntry,
    field_syntax: &SyntaxNode,
    field_name_token: &sai_syntax::SyntaxToken,
    field_def_id: LocalDefId,
    parent_def_path: &sai_hir::DefPath,
    ordinal: u32,
    canonical_path: String,
    field_label: &str,
    package_id: PackageId,
    reasons: &mut HashSet<String>,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) -> bool {
    let field_def_path = match def_paths.resolve(field_def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        _ => {
            reasons.insert(format!(
                "a {field_label} has no definition path in the selected package"
            ));
            return false;
        }
    };
    let parent_len = parent_def_path.data().len();
    let exact_path = field_def_path.data().len() == parent_len + 1
        && &field_def_path.data()[..parent_len] == parent_def_path.data()
        && field_def_path.data().last().is_some_and(|component| {
            component.data == DefPathData::Field(field_entry.name)
                && component.disambiguator == ordinal
        });
    if !exact_path {
        reasons.insert(format!(
            "a {field_label} DefPath disagrees with its parent, name, or sibling order"
        ));
        return false;
    }
    let field_def_path_hash = match DefPathHash::compute(&field_def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert(format!(
                "a {field_label} definition path hash cannot be computed"
            ));
            return false;
        }
    };
    let field_fingerprint =
        match declaration_fingerprint(SymbolKind::Field, field_syntax, Some(field_name_token)) {
            Ok(fingerprint) => fingerprint,
            Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
                reasons.insert(format!(
                    "a {field_label} name token is outside its declaration syntax node"
                ));
                return false;
            }
        };
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }
    current_symbols.push(CurrentSymbol::new(
        canonical_path,
        field_def_path_hash,
        SymbolKind::Field,
        field_fingerprint,
    ));
    true
}

#[allow(clippy::too_many_arguments)]
fn collect_alias(
    entry: &ItemTreeEntry,
    file_id: FileId,
    module_path: &[String],
    package_identity: &str,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owners_by_item: &HashMap<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>,
    seen_items: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    let key = (file_id, entry.ast_id);
    if !seen_items.insert(key) {
        reasons.insert("the target-filtered ItemTree contains a duplicate alias AST ID".into());
    }
    if entry.name == Symbol::ERROR {
        reasons.insert("a target-filtered type alias has no valid declaration name".into());
        return;
    }
    let name = match symbols.resolve(entry.name) {
        Ok(name) if !name.is_empty() => name.to_string(),
        _ => {
            reasons.insert("a target-filtered type-alias name cannot be resolved".to_owned());
            return;
        }
    };
    let Some(descriptor) = owners_by_item.get(&key).copied() else {
        reasons.insert("a target-filtered type alias has no matching Phase 1 owner".to_owned());
        return;
    };
    consumed_owners.insert(key);

    let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
        reasons.insert("a target-filtered type-alias AST ID has no source pointer".to_owned());
        return;
    };
    let Some(syntax) = pointer.resolve(file_id, &input.root) else {
        reasons.insert(
            "a target-filtered type-alias source pointer cannot resolve in its AST root".to_owned(),
        );
        return;
    };
    let Some(declaration) = ast::TypeDecl::cast(syntax.clone()) else {
        reasons.insert(
            "a target-filtered type-alias source pointer resolves to a non-type node".to_owned(),
        );
        return;
    };
    let Some(alias_definition) = declaration.alias_def() else {
        reasons.insert("a target-filtered type alias has no alias definition".to_owned());
        return;
    };
    if declaration.struct_def().is_some() || alias_definition.refinement().is_some() {
        reasons.insert("a target-filtered type alias has an unsupported declaration form".into());
        return;
    }
    if alias_definition.ty().is_none() {
        reasons.insert("a target-filtered type alias has no aliased type".to_owned());
        return;
    }
    let Some(name_node) = declaration.name() else {
        reasons.insert("a target-filtered type-alias AST has no name node".to_owned());
        return;
    };
    let Some(name_token) = name_node.token() else {
        reasons.insert("a target-filtered type-alias AST has no name token".to_owned());
        return;
    };
    let raw_name = name_token
        .text()
        .strip_prefix("r#")
        .unwrap_or(name_token.text());
    if sai_syntax::normalize_identifier(raw_name) != name {
        reasons
            .insert("the ItemTree and type-alias AST disagree about the declaration name".into());
        return;
    }
    if syntax_has_errors(&syntax) || name_token.has_error() {
        reasons.insert(
            "a target-filtered type-alias AST contains recovery or parse errors".to_owned(),
        );
        return;
    }

    let canonical_path = format!(
        "{}{}.{name}",
        package_identity,
        module_path
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect::<String>()
    );
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }

    let def_path = match def_paths.resolve(descriptor.owner.def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        Ok(Some(_)) => {
            reasons.insert(
                "a type-alias owner resolves to a definition path from another package".to_owned(),
            );
            return;
        }
        Ok(None) => {
            reasons.insert("a type-alias owner has no canonical definition path".to_owned());
            return;
        }
        Err(_) => {
            reasons.insert("a type-alias owner definition path cannot be resolved".to_owned());
            return;
        }
    };
    let path_matches_declaration = def_path.data().len() == module_path.len() + 1
        && def_path
            .data()
            .iter()
            .take(module_path.len())
            .zip(module_path)
            .all(|(component, name)| {
                matches!(&component.data, DefPathData::Module(segment)
                    if symbols.resolve(*segment).is_ok_and(|resolved| resolved.as_ref() == name))
            })
        && matches!(
            def_path.data().last().map(|component| &component.data),
            Some(DefPathData::TypeNs(symbol)) if *symbol == entry.name
        );
    if !path_matches_declaration {
        reasons.insert(
            "the canonical type-alias definition path does not match its module and ItemTree declaration"
                .to_owned(),
        );
        return;
    }
    let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert("a type-alias definition path hash cannot be computed".to_owned());
            return;
        }
    };
    let fingerprint = match declaration_fingerprint(SymbolKind::Alias, &syntax, Some(&name_token)) {
        Ok(fingerprint) => fingerprint,
        Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
            reasons.insert(
                "a type-alias declaration name token is outside its syntax node".to_owned(),
            );
            return;
        }
    };
    current_symbols.push(CurrentSymbol::new(
        canonical_path.clone(),
        def_path_hash,
        SymbolKind::Alias,
        fingerprint,
    ));
    collect_item_generic_params(
        &entry.children,
        declaration.generic_params(),
        &canonical_path,
        descriptor.owner.def_id,
        &def_path,
        input,
        package_id,
        "type alias",
        canonical_paths,
        current_symbols,
        reasons,
        def_paths,
        packages,
        symbols,
    );
}

#[allow(clippy::too_many_arguments)]
fn collect_distinct(
    entry: &ItemTreeEntry,
    file_id: FileId,
    module_path: &[String],
    package_identity: &str,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owners_by_item: &HashMap<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>,
    seen_items: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    let key = (file_id, entry.ast_id);
    if !seen_items.insert(key) {
        reasons.insert(
            "the target-filtered ItemTree contains a duplicate distinct-type AST ID".into(),
        );
    }
    if entry.kind != ItemTreeKind::Distinct {
        reasons.insert("a non-distinct ItemTree entry reached the distinct-type collector".into());
        return;
    }
    if entry.name == Symbol::ERROR {
        reasons.insert("a target-filtered distinct type has no valid declaration name".to_owned());
        return;
    }
    let name = match symbols.resolve(entry.name) {
        Ok(name) if !name.is_empty() => name.to_string(),
        _ => {
            reasons.insert("a target-filtered distinct-type name cannot be resolved".to_owned());
            return;
        }
    };
    let Some(descriptor) = owners_by_item.get(&key).copied() else {
        reasons.insert("a target-filtered distinct type has no matching Phase 1 owner".to_owned());
        return;
    };
    if descriptor.kind != TopLevelOwnerKind::Distinct
        || descriptor.file != file_id
        || descriptor.item_id != entry.ast_id
    {
        reasons.insert("a target-filtered distinct type has a mismatched Phase 1 owner".to_owned());
        return;
    }
    consumed_owners.insert(key);

    let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
        reasons.insert("a target-filtered distinct-type AST ID has no source pointer".to_owned());
        return;
    };
    let Some(syntax) = pointer.resolve(file_id, &input.root) else {
        reasons.insert(
            "a target-filtered distinct-type source pointer cannot resolve in its AST root"
                .to_owned(),
        );
        return;
    };
    let Some(declaration) = ast::DistinctDecl::cast(syntax.clone()) else {
        reasons.insert(
            "a target-filtered distinct-type source pointer resolves to a non-distinct node"
                .to_owned(),
        );
        return;
    };
    if input.ast_ids.ast_id(&declaration).map(|id| id.erase()) != Some(entry.ast_id) {
        reasons.insert(
            "a target-filtered distinct-type AST ID does not match its declaration".to_owned(),
        );
        return;
    }
    if declaration.base_type().is_none() {
        reasons.insert("a target-filtered distinct type has no base type".to_owned());
        return;
    }
    if declaration.refinement().is_some() {
        reasons.insert(
            "a refined distinct type is outside the supported distinct inventory".to_owned(),
        );
        return;
    }
    let Some(name_node) = declaration.name() else {
        reasons.insert("a target-filtered distinct-type AST has no name node".to_owned());
        return;
    };
    let Some(name_token) = name_node.token() else {
        reasons.insert("a target-filtered distinct-type AST has no name token".to_owned());
        return;
    };
    let raw_name = name_token
        .text()
        .strip_prefix("r#")
        .unwrap_or(name_token.text());
    if sai_syntax::normalize_identifier(raw_name) != name {
        reasons.insert(
            "the ItemTree and distinct-type AST disagree about the declaration name".into(),
        );
        return;
    }
    if syntax_has_errors(&syntax) || name_token.has_error() {
        reasons.insert(
            "a target-filtered distinct-type AST contains recovery or parse errors".to_owned(),
        );
        return;
    }
    if input
        .parser_diagnostics
        .iter()
        .any(|diagnostic| parser_diagnostic_overlaps_syntax(diagnostic, file_id, &syntax))
    {
        reasons.insert("a distinct-type syntax node has a parser diagnostic".to_owned());
        return;
    }

    // The ItemTree keeps direct generic parameters in source order. Resolve
    // each child back to the same source node before asking the shared generic
    // collector to validate its kind, name, DefPath and fingerprint.
    let ast_params = declaration
        .generic_params()
        .map(|params| params.params().collect::<Vec<_>>())
        .unwrap_or_default();
    if entry.children.len() != ast_params.len() {
        reasons.insert(
            "a distinct type's ItemTree generic-parameter count does not match its snapshot AST"
                .to_owned(),
        );
        return;
    }
    for (parameter, child) in ast_params.iter().zip(&entry.children) {
        let (expected_kind, ast_id, parameter_syntax) = match parameter {
            ast::GenericParam::Type(parameter) => (
                ItemTreeKind::TypeParam,
                input.ast_ids.ast_id(parameter).map(|id| id.erase()),
                parameter.syntax(),
            ),
            ast::GenericParam::Const(parameter) => (
                ItemTreeKind::ConstParam,
                input.ast_ids.ast_id(parameter).map(|id| id.erase()),
                parameter.syntax(),
            ),
        };
        let expected_pointer = sai_syntax::SyntaxNodePtr::new(file_id, parameter_syntax).ok();
        let actual_pointer = input
            .ast_ids
            .pointer(child.ast_id)
            .filter(|pointer| pointer.file() == file_id);
        if child.kind != expected_kind
            || ast_id != Some(child.ast_id)
            || expected_pointer
                .as_ref()
                .zip(actual_pointer)
                .is_none_or(|(expected, actual)| !actual.is_same_source_node(expected))
        {
            reasons.insert(
                "a distinct type's ItemTree generic-parameter kind, order, or source node does not match its snapshot AST"
                    .to_owned(),
            );
            return;
        }
    }

    if entry
        .raw_attrs
        .iter()
        .any(|attribute| attribute.name == sai_hir::item_tree::AttrName::Derive)
    {
        reasons.insert(
            "a distinct type has derived impl symbols outside the supported inventory".to_owned(),
        );
    }

    let canonical_path = format!(
        "{}{}.{name}",
        package_identity,
        module_path
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect::<String>()
    );
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }

    let owner_component = DefPathData::TypeNs(entry.name);
    let Some(item_position) = input
        .filtered
        .item_tree
        .items
        .iter()
        .position(|candidate| candidate.ast_id == entry.ast_id)
    else {
        reasons.insert(
            "a target-filtered distinct type is absent from its module ItemTree".to_owned(),
        );
        return;
    };
    let owner_disambiguator = input.filtered.item_tree.items[..item_position]
        .iter()
        .filter(|candidate| {
            matches!(
                candidate.kind,
                ItemTreeKind::Struct
                    | ItemTreeKind::Alias
                    | ItemTreeKind::Refined
                    | ItemTreeKind::Enum
                    | ItemTreeKind::Distinct
                    | ItemTreeKind::Resource
                    | ItemTreeKind::Trait
            ) && candidate.name == entry.name
        })
        .count();
    let Ok(owner_disambiguator) = u32::try_from(owner_disambiguator) else {
        reasons.insert("a distinct-type owner DefPath disambiguator overflowed".to_owned());
        return;
    };
    let def_path = match def_paths.resolve(descriptor.owner.def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        Ok(Some(_)) => {
            reasons.insert(
                "a distinct-type owner resolves to a definition path from another package"
                    .to_owned(),
            );
            return;
        }
        Ok(None) => {
            reasons.insert("a distinct-type owner has no canonical definition path".to_owned());
            return;
        }
        Err(_) => {
            reasons.insert("a distinct-type owner definition path cannot be resolved".to_owned());
            return;
        }
    };
    let path_matches_declaration = def_path.data().len() == module_path.len() + 1
        && def_path
            .data()
            .iter()
            .take(module_path.len())
            .zip(module_path)
            .all(|(component, name)| {
                component.disambiguator == 0
                    && matches!(&component.data, DefPathData::Module(segment)
                        if symbols.resolve(*segment).is_ok_and(|resolved| resolved.as_ref() == name))
            })
        && def_path.data().last().is_some_and(|component| {
            component.data == owner_component && component.disambiguator == owner_disambiguator
        });
    if !path_matches_declaration {
        reasons.insert(
            "the distinct-type DefPath does not match its module, TypeNs name, or disambiguator"
                .to_owned(),
        );
        return;
    }
    let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert("a distinct-type definition path hash cannot be computed".to_owned());
            return;
        }
    };
    let fingerprint =
        match declaration_fingerprint(SymbolKind::Distinct, &syntax, Some(&name_token)) {
            Ok(fingerprint) => fingerprint,
            Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
                reasons.insert(
                    "a distinct-type name token is outside its declaration syntax node".to_owned(),
                );
                return;
            }
        };
    current_symbols.push(CurrentSymbol::new(
        canonical_path.clone(),
        def_path_hash,
        SymbolKind::Distinct,
        fingerprint,
    ));
    collect_item_generic_params(
        &entry.children,
        declaration.generic_params(),
        &canonical_path,
        descriptor.owner.def_id,
        &def_path,
        input,
        package_id,
        "distinct type",
        canonical_paths,
        current_symbols,
        reasons,
        def_paths,
        packages,
        symbols,
    );
}

#[allow(clippy::too_many_arguments)]
fn collect_refined(
    entry: &ItemTreeEntry,
    file_id: FileId,
    module_path: &[String],
    package_identity: &str,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owners_by_item: &HashMap<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>,
    predicate_owners_by_item: &HashMap<
        (FileId, sai_hir::item_tree::ErasedAstId),
        Vec<RefinementPredicateOwnerDescriptor>,
    >,
    seen_items: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_predicate_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    let key = (file_id, entry.ast_id);
    if !seen_items.insert(key) {
        reasons
            .insert("the target-filtered ItemTree contains a duplicate refined-type AST ID".into());
    }
    if entry.name == Symbol::ERROR {
        reasons.insert("a target-filtered refined type has no valid declaration name".to_owned());
        return;
    }
    let name = match symbols.resolve(entry.name) {
        Ok(name) if !name.is_empty() => name.to_string(),
        _ => {
            reasons.insert("a target-filtered refined-type name cannot be resolved".to_owned());
            return;
        }
    };
    let Some(descriptor) = owners_by_item.get(&key).copied() else {
        reasons.insert("a target-filtered refined type has no matching Phase 1 owner".to_owned());
        return;
    };
    consumed_owners.insert(key);

    match predicate_owners_by_item.get(&key).map(Vec::as_slice) {
        Some([predicate_owner])
            if predicate_owner.owner == descriptor.owner
                && predicate_owner.file == descriptor.file
                && predicate_owner.item_id == descriptor.item_id
                && predicate_owner.kind == RefinementPredicateOwnerKind::Refined =>
        {
            consumed_predicate_owners.insert(key);
        }
        Some([]) | None => {
            reasons.insert(
                "a refined type has no matching refinement-predicate owner descriptor".to_owned(),
            );
            return;
        }
        Some(_) => {
            reasons.insert(
                "a refined type has duplicate or mismatched refinement-predicate owner descriptors"
                    .to_owned(),
            );
            return;
        }
    }

    let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
        reasons.insert("a target-filtered refined-type AST ID has no source pointer".to_owned());
        return;
    };
    let Some(syntax) = pointer.resolve(file_id, &input.root) else {
        reasons.insert(
            "a target-filtered refined-type source pointer cannot resolve in its AST root"
                .to_owned(),
        );
        return;
    };
    let Some(declaration) = ast::TypeDecl::cast(syntax.clone()) else {
        reasons.insert(
            "a target-filtered refined-type source pointer resolves to a non-type node".to_owned(),
        );
        return;
    };
    let Some(refined_definition) = declaration.alias_def() else {
        reasons.insert("a target-filtered refined type has no alias definition".to_owned());
        return;
    };
    if declaration.struct_def().is_some() || refined_definition.refinement().is_none() {
        reasons.insert(
            "a target-filtered refined type does not have the refined declaration shape".into(),
        );
        return;
    }
    if refined_definition.ty().is_none() {
        reasons.insert("a target-filtered refined type has no base type".to_owned());
        return;
    }

    // Refined types are explicitly non-generic (§5.6.4). Do not emit either
    // the parent or generic-parameter symbols for a declaration that violates
    // this restriction; the inventory must remain incomplete.
    let has_ast_generic_params = declaration
        .generic_params()
        .is_some_and(|params| params.params().next().is_some());
    if has_ast_generic_params || !entry.children.is_empty() {
        reasons.insert("a refined type declares unsupported generic parameters".to_owned());
        return;
    }

    let Some(name_node) = declaration.name() else {
        reasons.insert("a target-filtered refined-type AST has no name node".to_owned());
        return;
    };
    let Some(name_token) = name_node.token() else {
        reasons.insert("a target-filtered refined-type AST has no name token".to_owned());
        return;
    };
    let raw_name = name_token
        .text()
        .strip_prefix("r#")
        .unwrap_or(name_token.text());
    if sai_syntax::normalize_identifier(raw_name) != name {
        reasons.insert(
            "the ItemTree and refined-type AST disagree about the declaration name".to_owned(),
        );
        return;
    }
    if syntax_has_errors(&syntax) || name_token.has_error() {
        reasons.insert(
            "a target-filtered refined-type AST contains recovery or parse errors".to_owned(),
        );
        return;
    }

    let canonical_path = format!(
        "{}{}.{name}",
        package_identity,
        module_path
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect::<String>()
    );
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }

    let def_path = match def_paths.resolve(descriptor.owner.def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        Ok(Some(_)) => {
            reasons.insert(
                "a refined-type owner resolves to a definition path from another package"
                    .to_owned(),
            );
            return;
        }
        Ok(None) => {
            reasons.insert("a refined-type owner has no canonical definition path".to_owned());
            return;
        }
        Err(_) => {
            reasons.insert("a refined-type owner definition path cannot be resolved".to_owned());
            return;
        }
    };
    let path_matches_declaration = def_path.data().len() == module_path.len() + 1
        && def_path
            .data()
            .iter()
            .take(module_path.len())
            .zip(module_path)
            .all(|(component, name)| {
                matches!(&component.data, DefPathData::Module(segment)
                    if symbols.resolve(*segment).is_ok_and(|resolved| resolved.as_ref() == name))
            })
        && matches!(
            def_path.data().last().map(|component| &component.data),
            Some(DefPathData::TypeNs(symbol)) if *symbol == entry.name
        );
    if !path_matches_declaration {
        reasons.insert(
            "the canonical refined-type definition path does not match its module and ItemTree declaration"
                .to_owned(),
        );
        return;
    }
    let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert("a refined-type definition path hash cannot be computed".to_owned());
            return;
        }
    };
    let fingerprint = match declaration_fingerprint(SymbolKind::Refined, &syntax, Some(&name_token))
    {
        Ok(fingerprint) => fingerprint,
        Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
            reasons.insert(
                "a refined-type declaration name token is outside its syntax node".to_owned(),
            );
            return;
        }
    };
    current_symbols.push(CurrentSymbol::new(
        canonical_path,
        def_path_hash,
        SymbolKind::Refined,
        fingerprint,
    ));
}

#[allow(clippy::too_many_arguments)]
fn collect_effect(
    entry: &ItemTreeEntry,
    file_id: FileId,
    module_path: &[String],
    package_identity: &str,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owners_by_item: &HashMap<(FileId, sai_hir::item_tree::ErasedAstId), TopLevelOwnerDescriptor>,
    seen_items: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    let key = (file_id, entry.ast_id);
    if !seen_items.insert(key) {
        reasons.insert("the target-filtered ItemTree contains a duplicate effect AST ID".into());
    }
    if !entry.children.is_empty() {
        reasons.insert("an effect entry has unexpected child declarations".to_owned());
    }
    if entry.name == Symbol::ERROR {
        reasons.insert("a target-filtered effect has no valid declaration name".to_owned());
        return;
    }
    let name = match symbols.resolve(entry.name) {
        Ok(name) if !name.is_empty() => name.to_string(),
        _ => {
            reasons.insert("a target-filtered effect name cannot be resolved".to_owned());
            return;
        }
    };
    let Some(descriptor) = owners_by_item.get(&key).copied() else {
        reasons.insert("a target-filtered effect has no matching Phase 1 owner".to_owned());
        return;
    };
    consumed_owners.insert(key);

    let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
        reasons.insert("a target-filtered effect AST ID has no source pointer".to_owned());
        return;
    };
    let Some(syntax) = pointer.resolve(file_id, &input.root) else {
        reasons.insert(
            "a target-filtered effect source pointer cannot resolve in its AST root".to_owned(),
        );
        return;
    };
    let Some(declaration) = ast::EffectDecl::cast(syntax.clone()) else {
        reasons.insert(
            "a target-filtered effect source pointer resolves to a non-effect node".to_owned(),
        );
        return;
    };
    let Some(name_node) = declaration.name() else {
        reasons.insert("a target-filtered effect AST has no name node".to_owned());
        return;
    };
    let Some(name_token) = name_node.token() else {
        reasons.insert("a target-filtered effect AST has no name token".to_owned());
        return;
    };
    let raw_name = name_token
        .text()
        .strip_prefix("r#")
        .unwrap_or(name_token.text());
    if sai_syntax::normalize_identifier(raw_name) != name {
        reasons
            .insert("the ItemTree and effect AST disagree about the declaration name".to_owned());
        return;
    }
    if syntax_has_errors(&syntax) || name_token.has_error() {
        reasons.insert("a target-filtered effect AST contains recovery or parse errors".to_owned());
        return;
    }

    let canonical_path = format!(
        "{}{}.{name}",
        package_identity,
        module_path
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect::<String>()
    );
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }

    let def_path = match def_paths.resolve(descriptor.owner.def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        Ok(Some(_)) => {
            reasons.insert(
                "an effect owner resolves to a definition path from another package".to_owned(),
            );
            return;
        }
        Ok(None) => {
            reasons.insert("an effect owner has no canonical definition path".to_owned());
            return;
        }
        Err(_) => {
            reasons.insert("an effect owner definition path cannot be resolved".to_owned());
            return;
        }
    };
    let path_matches_declaration = def_path.data().len() == module_path.len() + 1
        && def_path
            .data()
            .iter()
            .take(module_path.len())
            .zip(module_path)
            .all(|(component, name)| {
                matches!(&component.data, DefPathData::Module(segment)
                    if symbols.resolve(*segment).is_ok_and(|resolved| resolved.as_ref() == name))
            })
        && matches!(
            def_path.data().last().map(|component| &component.data),
            Some(DefPathData::Effect(symbol)) if *symbol == entry.name
        );
    if !path_matches_declaration {
        reasons.insert(
            "the canonical effect definition path does not match its module and ItemTree declaration"
                .to_owned(),
        );
        return;
    }
    let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert("an effect definition path hash cannot be computed".to_owned());
            return;
        }
    };
    let fingerprint = match declaration_fingerprint(SymbolKind::Effect, &syntax, Some(&name_token))
    {
        Ok(fingerprint) => fingerprint,
        Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
            reasons
                .insert("an effect declaration name token is outside its syntax node".to_owned());
            return;
        }
    };
    current_symbols.push(CurrentSymbol::new(
        canonical_path,
        def_path_hash,
        SymbolKind::Effect,
        fingerprint,
    ));
}

#[allow(clippy::too_many_arguments)]
fn collect_function(
    entry: &ItemTreeEntry,
    file_id: FileId,
    module_path: &[String],
    package_identity: &str,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owners_by_item: &HashMap<(FileId, sai_hir::item_tree::ErasedAstId), FunctionOwnerDescriptor>,
    seen_items: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    consumed_owners: &mut HashSet<(FileId, sai_hir::item_tree::ErasedAstId)>,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    let key = (file_id, entry.ast_id);
    if !seen_items.insert(key) {
        reasons
            .insert("the target-filtered ItemTree contains a duplicate function AST ID".to_owned());
    }
    if entry.name == Symbol::ERROR {
        reasons.insert("a target-filtered function has no valid declaration name".to_owned());
        return;
    }
    let name = match symbols.resolve(entry.name) {
        Ok(name) if !name.is_empty() => name.to_string(),
        _ => {
            reasons.insert("a target-filtered function name cannot be resolved".to_owned());
            return;
        }
    };
    let Some(descriptor) = owners_by_item.get(&key).copied() else {
        reasons.insert("a target-filtered function has no matching Phase 1 owner".to_owned());
        return;
    };
    consumed_owners.insert(key);

    let Some(pointer) = input.ast_ids.pointer(entry.ast_id) else {
        reasons.insert("a target-filtered function AST ID has no source pointer".to_owned());
        return;
    };
    let Some(syntax) = pointer.resolve(file_id, &input.root) else {
        reasons.insert(
            "a target-filtered function source pointer cannot resolve in its AST root".to_owned(),
        );
        return;
    };
    let Some(function) = ast::FnDecl::cast(syntax.clone()) else {
        reasons.insert(
            "a target-filtered function source pointer resolves to a non-function node".to_owned(),
        );
        return;
    };
    let Some(name_node) = function.name() else {
        reasons.insert("a target-filtered function AST has no name node".to_owned());
        return;
    };
    let Some(name_token) = name_node.token() else {
        reasons.insert("a target-filtered function AST has no name token".to_owned());
        return;
    };
    let raw_name = name_token
        .text()
        .strip_prefix("r#")
        .unwrap_or(name_token.text());
    if sai_syntax::normalize_identifier(raw_name) != name {
        reasons
            .insert("the ItemTree and function AST disagree about the declaration name".to_owned());
        return;
    }
    if syntax_has_errors(&syntax) || name_token.has_error() {
        reasons
            .insert("a target-filtered function AST contains recovery or parse errors".to_owned());
        return;
    }

    let canonical_path = format!(
        "{}{}.{name}",
        package_identity,
        module_path
            .iter()
            .map(|segment| format!("/{segment}"))
            .collect::<String>()
    );
    if !canonical_paths.insert(canonical_path.clone()) {
        reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
    }

    let def_path = match def_paths.resolve(descriptor.owner.def_id) {
        Ok(Some(path)) if path.package() == package_id => path,
        Ok(Some(_)) => {
            reasons.insert(
                "a function owner resolves to a definition path from another package".to_owned(),
            );
            return;
        }
        Ok(None) => {
            reasons.insert("a function owner has no canonical definition path".to_owned());
            return;
        }
        Err(_) => {
            reasons.insert("a function owner definition path cannot be resolved".to_owned());
            return;
        }
    };
    let path_matches_declaration = def_path.data().len() == module_path.len() + 1
        && def_path
            .data()
            .iter()
            .take(module_path.len())
            .zip(module_path)
            .all(|(component, name)| {
                matches!(&component.data, DefPathData::Module(segment)
                    if symbols.resolve(*segment).is_ok_and(|resolved| resolved.as_ref() == name))
            })
        && matches!(
            def_path.data().last().map(|component| &component.data),
            Some(DefPathData::ValueNs(symbol)) if *symbol == entry.name
        );
    if !path_matches_declaration {
        reasons.insert(
            "the canonical definition path does not match its module and ItemTree declaration"
                .to_owned(),
        );
        return;
    }
    let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
        Ok(hash) => Fingerprint::new(hash.raw()),
        Err(_) => {
            reasons.insert("a function definition path hash cannot be computed".to_owned());
            return;
        }
    };
    let fingerprint = match declaration_fingerprint(SymbolKind::Fn, &syntax, Some(&name_token)) {
        Ok(fingerprint) => fingerprint,
        Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
            reasons
                .insert("a function declaration name token is outside its syntax node".to_owned());
            return;
        }
    };
    current_symbols.push(CurrentSymbol::new(
        canonical_path.clone(),
        def_path_hash,
        SymbolKind::Fn,
        fingerprint,
    ));
    collect_item_generic_params(
        &entry.children,
        function.generic_params(),
        &canonical_path,
        descriptor.owner.def_id,
        &def_path,
        input,
        package_id,
        "function",
        canonical_paths,
        current_symbols,
        reasons,
        def_paths,
        packages,
        symbols,
    );
}

#[allow(clippy::too_many_arguments)]
fn collect_item_generic_params(
    generic_children: &[ItemTreeEntry],
    generic_param_list: Option<ast::GenericParamList>,
    function_path: &str,
    function_owner: LocalDefId,
    function_def_path: &sai_hir::DefPath,
    input: &OwnedPhase1ModuleInput,
    package_id: PackageId,
    owner_kind: &str,
    canonical_paths: &mut HashSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
    reasons: &mut HashSet<String>,
    def_paths: &DefPathInterner,
    packages: &PackageInterner,
    symbols: &Interner,
) {
    let ast_params = generic_param_list
        .as_ref()
        .map(|params| params.params().collect::<Vec<_>>())
        .unwrap_or_default();
    if generic_param_list.as_ref().is_some_and(|params| {
        input.parser_diagnostics.iter().any(|diagnostic| {
            parser_diagnostic_overlaps_syntax(
                diagnostic,
                input.filtered.item_tree.file,
                &params.syntax(),
            )
        })
    }) {
        reasons.insert(format!(
            "a {owner_kind} generic-parameter list has a parser diagnostic"
        ));
        return;
    }
    if generic_children.len() != ast_params.len() {
        reasons.insert(format!(
            "a {owner_kind}'s ItemTree generic-parameter count does not match its snapshot AST"
        ));
        return;
    }

    let mut validated = Vec::with_capacity(ast_params.len());
    for (parameter, child) in ast_params.iter().zip(generic_children) {
        let (expected_kind, ast_id, name_node, syntax, is_const) = match parameter {
            ast::GenericParam::Type(parameter) => (
                ItemTreeKind::TypeParam,
                input.ast_ids.ast_id(parameter).map(|id| id.erase()),
                parameter.name(),
                parameter.syntax().clone(),
                false,
            ),
            ast::GenericParam::Const(parameter) => (
                ItemTreeKind::ConstParam,
                input.ast_ids.ast_id(parameter).map(|id| id.erase()),
                parameter.name(),
                parameter.syntax().clone(),
                true,
            ),
        };
        if child.kind != expected_kind {
            reasons.insert(format!(
                "a {owner_kind}'s ItemTree generic-parameter kind does not match its snapshot AST"
            ));
            return;
        }
        if ast_id != Some(child.ast_id) || input.ast_ids.pointer(child.ast_id).is_none() {
            reasons.insert(format!(
                "a {owner_kind}'s ItemTree generic-parameter ID does not match its snapshot AST"
            ));
            return;
        }
        if !child.children.is_empty() {
            reasons.insert("a generic parameter has unexpected child declarations".to_owned());
            return;
        }
        if child.name == Symbol::ERROR {
            reasons.insert(format!(
                "a {owner_kind} generic parameter has no valid ItemTree name"
            ));
            return;
        }
        let name = match symbols.resolve(child.name) {
            Ok(name) if !name.is_empty() => name.to_string(),
            _ => {
                reasons.insert(format!(
                    "a {owner_kind} generic parameter name cannot be resolved"
                ));
                return;
            }
        };
        let Some(name_node) = name_node else {
            reasons.insert(format!(
                "a {owner_kind} generic-parameter AST has no name node"
            ));
            return;
        };
        let Some(name_token) = name_node.token() else {
            reasons.insert(format!(
                "a {owner_kind} generic-parameter AST has no name token"
            ));
            return;
        };
        let raw_name = name_token
            .text()
            .strip_prefix("r#")
            .unwrap_or(name_token.text());
        if sai_syntax::normalize_identifier(raw_name) != name {
            reasons.insert(format!(
                "the ItemTree and {owner_kind} generic-parameter AST disagree about the declaration name"
            ));
            return;
        }
        if syntax_has_errors(&syntax) || name_token.has_error() {
            reasons.insert(format!(
                "a {owner_kind} generic-parameter AST contains recovery or parse errors"
            ));
            return;
        }
        validated.push((child.name, name, name_token, syntax, is_const));
    }

    // Match lower::prepare_generic_params: disambiguators are ordinals for
    // equal kind-and-name components, in source order.
    let mut next_disambiguator = HashMap::<DefPathData, u32>::new();
    let mut path_parameters = Vec::with_capacity(validated.len());
    let mut expected_components = Vec::with_capacity(validated.len());
    for (name, _, _, _, is_const) in &validated {
        let component = if *is_const {
            DefPathData::ConstParam(*name)
        } else {
            DefPathData::TypeParam(*name)
        };
        let disambiguator = next_disambiguator.entry(component.clone()).or_default();
        let ordinal = *disambiguator;
        let Some(next) = ordinal.checked_add(1) else {
            reasons.insert(format!(
                "{owner_kind} generic-parameter disambiguator overflowed"
            ));
            return;
        };
        *disambiguator = next;
        path_parameters.push((*name, *is_const, ordinal));
        expected_components.push((component, ordinal));
    }
    let parameter_def_ids = match def_paths.intern_generic_params(function_owner, &path_parameters)
    {
        Ok(def_ids) if def_ids.len() == validated.len() => def_ids,
        Ok(_) => {
            reasons.insert(format!(
                "HIR returned the wrong number of {owner_kind} generic-parameter definitions"
            ));
            return;
        }
        Err(_) => {
            reasons.insert(format!(
                "{owner_kind} generic-parameter definitions cannot be interned"
            ));
            return;
        }
    };

    let mut generic_symbols = Vec::with_capacity(validated.len());
    for (((_, name, name_token, syntax, _), def_id), (expected_component, disambiguator)) in
        validated
            .iter()
            .zip(parameter_def_ids)
            .zip(expected_components)
    {
        let def_path = match def_paths.resolve(def_id) {
            Ok(Some(path)) if path.package() == package_id => path,
            _ => {
                reasons.insert(format!(
                    "a {owner_kind} generic parameter has no definition path in the selected package"
                ));
                return;
            }
        };
        let parent_len = function_def_path.data().len();
        let exact_path = def_path.data().len() == parent_len + 1
            && &def_path.data()[..parent_len] == function_def_path.data()
            && def_path.data().last().is_some_and(|component| {
                component.data == expected_component && component.disambiguator == disambiguator
            });
        if !exact_path {
            reasons.insert(format!(
                "a {owner_kind} generic-parameter DefPath disagrees with lowering order or kind"
            ));
            return;
        }
        let def_path_hash = match DefPathHash::compute(&def_path, packages, symbols) {
            Ok(hash) => Fingerprint::new(hash.raw()),
            Err(_) => {
                reasons.insert(format!(
                    "a {owner_kind} generic-parameter definition path hash cannot be computed"
                ));
                return;
            }
        };
        let fingerprint = match declaration_fingerprint(
            SymbolKind::GenericParam,
            syntax,
            Some(name_token),
        ) {
            Ok(fingerprint) => fingerprint,
            Err(DeclarationFingerprintError::NameTokenNotInDeclaration) => {
                reasons.insert(format!(
                        "a {owner_kind} generic-parameter name token is outside its declaration syntax node"
                    ));
                return;
            }
        };
        let canonical_path = format!("{function_path}.generic[{name}]");
        if !canonical_paths.insert(canonical_path.clone()) {
            reasons.insert(format!("duplicate canonical symbol path {canonical_path}"));
        }
        generic_symbols.push(CurrentSymbol::new(
            canonical_path,
            def_path_hash,
            SymbolKind::GenericParam,
            fingerprint,
        ));
    }
    current_symbols.extend(generic_symbols);
}

fn syntax_has_errors(root: &SyntaxNode) -> bool {
    let mut stack = vec![SyntaxElement::Node(root.clone())];
    while let Some(element) = stack.pop() {
        match element {
            SyntaxElement::Node(node) => {
                if node.kind() == sai_syntax::SyntaxKind::ERROR {
                    return true;
                }
                stack.extend(node.children_with_tokens());
            }
            SyntaxElement::Token(token) if token.has_error() => return true,
            SyntaxElement::Token(_) => {}
        }
    }
    false
}

fn parser_diagnostic_overlaps_syntax(
    diagnostic: &sai_diagnostics::Diagnostic,
    file_id: FileId,
    syntax: &SyntaxNode,
) -> bool {
    let Some(primary) = diagnostic.primary() else {
        return true;
    };
    if primary.span.file != file_id {
        return false;
    }
    let diagnostic_range = primary.span.range;
    let syntax_range = syntax.text_range();
    diagnostic_range.start() <= syntax_range.end() && syntax_range.start() <= diagnostic_range.end()
}
