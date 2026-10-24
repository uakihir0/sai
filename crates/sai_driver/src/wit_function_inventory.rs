//! Inventory support for authored WIT interface functions.

use super::{Phase1PackageSymbolInventory, RootDatabase};
use sai_hir::{DefPathData, DefPathHash, PackageId};
use sai_package::mvs::MvsPackageVersion;
use sai_semantic::{
    CurrentSymbol, InventoryCoverage, PackageSymbolInventory, SymbolKind,
    declaration_fingerprint_from_token_pairs,
};
use sai_stable_hash::Fingerprint;
use sai_vfs::{FileContents, FileKey, PackageKey, PackageRelativePath, WitFilesSnapshot};
use sai_wit::{
    WitContract, WitIfaceIdx, WitItem, WitItemIdx, WitItemKind, WitMemberKind, WitPackage,
    WitPackageKind, WitType, WitUnit, WitUnitKind, WitView, WitViewDefKind, WitViewEntity,
    authored_interface_alias_tokens, authored_interface_enum_tokens,
    authored_interface_function_tokens, authored_interface_resource_tokens, wit_name_to_sai,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::str;
use std::sync::Arc;

/// Adds direct authored WIT interface functions, resources, and resource
/// members to the module inventory.
///
/// This deliberately does not inspect dependency models in the WIT closure.
/// A WIT view mismatch or any unsupported root declaration leaves the result
/// incomplete, even when supported declarations can still be inventoried.
pub(super) fn add_authored_wit_interface_functions(
    database: &mut RootDatabase,
    package_id: PackageId,
    package_key: &PackageKey,
    selected_version: Option<&MvsPackageVersion>,
    inventory: Phase1PackageSymbolInventory,
) -> Phase1PackageSymbolInventory {
    let Phase1PackageSymbolInventory::Available(inventory) = inventory else {
        return inventory;
    };

    let mut reasons = BTreeSet::new();
    let contract = match database.wit_contract(package_id) {
        Ok(contract) => contract,
        Err(error) => {
            reasons.insert(format!("WIT manifest contract is unavailable: {error:?}"));
            return incomplete_inventory(inventory, reasons, []);
        }
    };
    let Some(wit_directory) = contract.wit_directory() else {
        return Phase1PackageSymbolInventory::Available(inventory);
    };
    let Some(selected_version) = selected_version else {
        reasons.insert("WIT inventory has no exact selected package version".to_owned());
        return incomplete_inventory(inventory, reasons, []);
    };

    let expected_package_key = format!(
        "{}#{}",
        selected_version.package(),
        selected_version.node().line()
    );
    if package_key.as_str() != expected_package_key {
        reasons.insert("WIT inventory package key does not match its selected graph node".into());
        return incomplete_inventory(inventory, reasons, []);
    }

    if !contract_matches_selection(&contract, selected_version) {
        reasons.insert(
            "WIT manifest contract identity or full version does not match the selected package"
                .into(),
        );
        return incomplete_inventory(inventory, reasons, []);
    }
    if contract.kind() == WitPackageKind::Artifact {
        reasons.insert("artifact WIT packages are outside this inventory slice".into());
        return incomplete_inventory(inventory, reasons, []);
    }
    if database.phase1_generated_wit_packages.contains(&package_id) {
        reasons.insert("generated WIT package views are outside this inventory slice".into());
    }

    let unit = match WitUnit::dir(package_id, &database.packages, selected_version) {
        Ok(unit) if unit.canonical_name() == selected_canonical_name(selected_version) => unit,
        Ok(_) => {
            reasons.insert("authored WIT unit does not retain the exact selected version".into());
            return incomplete_inventory(inventory, reasons, []);
        }
        Err(error) => {
            reasons.insert(format!("authored WIT unit identity is invalid: {error:?}"));
            return incomplete_inventory(inventory, reasons, []);
        }
    };
    if !matches!(unit.kind(), WitUnitKind::Dir { package } if package == package_id) {
        reasons.insert("WIT function inventory requires the selected authored Dir unit".into());
        return incomplete_inventory(inventory, reasons, []);
    }

    let file_snapshot = match database
        .vfs
        .wit_files(&mut database.query_database, package_key.clone())
    {
        Ok(Some(snapshot)) => snapshot,
        Ok(None) => {
            reasons.insert("declared WIT directory has no tracked direct-file snapshot".into());
            return incomplete_inventory(inventory, reasons, []);
        }
        Err(error) => {
            reasons.insert(format!(
                "declared WIT file snapshot is unavailable: {error}"
            ));
            return incomplete_inventory(inventory, reasons, []);
        }
    };
    let files = match file_snapshot {
        WitFilesSnapshot::Listed { directory, files } if directory == wit_directory => files,
        WitFilesSnapshot::Listed { directory, .. } => {
            reasons.insert(format!(
                "WIT file snapshot directory `{directory}` differs from manifest directory `{wit_directory}`"
            ));
            return incomplete_inventory(inventory, reasons, []);
        }
        WitFilesSnapshot::NotApplicable => {
            reasons.insert("manifest declares WIT but its file snapshot is not applicable".into());
            return incomplete_inventory(inventory, reasons, []);
        }
    };
    if files.is_empty() {
        reasons.insert("declared WIT directory contains no direct .wit files".into());
        return incomplete_inventory(inventory, reasons, []);
    }

    let source_files = match read_direct_wit_sources(database, package_key, wit_directory, &files) {
        Ok(sources) => sources,
        Err(reason) => {
            reasons.insert(reason);
            return incomplete_inventory(inventory, reasons, []);
        }
    };

    let loaded = match database.wit_package(&unit) {
        Ok(result) => match result.package {
            Some(package) if result.failure.is_none() => package,
            _ => {
                reasons.insert(format!(
                    "authored WIT package model could not be loaded: {:?}",
                    result.failure
                ));
                return incomplete_inventory(inventory, reasons, []);
            }
        },
        Err(error) => {
            reasons.insert(format!("authored WIT package query failed: {error:?}"));
            return incomplete_inventory(inventory, reasons, []);
        }
    };
    let root = loaded.root();
    if root.unit != unit || !root_matches_selection(root, selected_version) {
        reasons.insert("WIT root package model does not match the exact selected unit".into());
        return incomplete_inventory(inventory, reasons, []);
    }
    if loaded.closure().last() != Some(&unit) {
        reasons.insert("WIT package closure does not end with the selected root unit".into());
        return incomplete_inventory(inventory, reasons, []);
    }

    let view = match database.wit_view(&unit) {
        Ok(view) => view,
        Err(error) => {
            reasons.insert(format!("authored WIT view could not be built: {error}"));
            return incomplete_inventory(inventory, reasons, []);
        }
    };
    if !view_matches_root(&view, &unit, root) {
        reasons.insert("authored WIT view does not match the selected root package model".into());
        return incomplete_inventory(inventory, reasons, []);
    }

    let mut symbols = Vec::new();
    collect_root_interface_symbols(
        &view,
        root,
        &source_files,
        &database.packages,
        &database.symbols,
        &mut reasons,
        &mut symbols,
    );
    incomplete_inventory(inventory, reasons, symbols)
}

fn contract_matches_selection(
    contract: &WitContract,
    selected_version: &MvsPackageVersion,
) -> bool {
    contract.identity().to_string() == selected_version.package()
        && contract.version().to_string() == selected_version.version().to_string()
}

fn selected_canonical_name(selected_version: &MvsPackageVersion) -> String {
    format!(
        "{}@{}",
        selected_version.package(),
        selected_version.version()
    )
}

fn root_matches_selection(root: &WitPackage, selected_version: &MvsPackageVersion) -> bool {
    root.name.namespace
        == selected_version
            .package()
            .split(':')
            .next()
            .unwrap_or_default()
        && root.name.name
            == selected_version
                .package()
                .split(':')
                .nth(1)
                .unwrap_or_default()
        && root.name.version.as_ref().map(ToString::to_string)
            == Some(selected_version.version().to_string())
}

fn view_matches_root(view: &WitView, unit: &WitUnit, root: &WitPackage) -> bool {
    view.unit == *unit
        && view.complete
        && view.model == *root
        && view.model.unit == *unit
        && view.interfaces.len() == root.interfaces.len()
        && view.worlds.len() == root.worlds.len()
}

fn read_direct_wit_sources(
    database: &mut RootDatabase,
    package_key: &PackageKey,
    wit_directory: &str,
    files: &[PackageRelativePath],
) -> Result<BTreeMap<String, Arc<str>>, String> {
    let prefix = (wit_directory != ".").then(|| format!("{wit_directory}/"));
    let mut sources = BTreeMap::new();
    for path in files {
        let relative_path = match prefix.as_deref() {
            Some(prefix) => path.as_str().strip_prefix(prefix).ok_or_else(|| {
                format!(
                    "WIT source path `{}` is outside the declared directory `{wit_directory}`",
                    path.as_str()
                )
            })?,
            None => path.as_str(),
        };
        if relative_path.contains('/')
            || !relative_path.ends_with(".wit")
            || relative_path.len() <= ".wit".len()
        {
            return Err(format!(
                "WIT source path `{}` is not a direct .wit file",
                path.as_str()
            ));
        }
        let file_key = FileKey::from_package_path(package_key.as_str().to_owned(), path.clone())
            .map_err(|error| format!("WIT source file key is invalid: {error:?}"))?;
        let contents = database
            .vfs
            .file_contents(&mut database.query_database, file_key)
            .map_err(|error| format!("WIT source bytes are unavailable: {error}"))?
            .ok_or_else(|| format!("WIT source bytes are missing for `{}`", path.as_str()))?;
        let FileContents::Bytes(bytes) = contents else {
            return Err(format!(
                "WIT source bytes could not be read for `{}`",
                path.as_str()
            ));
        };
        let source = str::from_utf8(bytes.as_ref())
            .map_err(|_| format!("WIT source `{}` is not valid UTF-8", path.as_str()))?;
        if sources
            .insert(relative_path.to_owned(), Arc::<str>::from(source))
            .is_some()
        {
            return Err("WIT file snapshot contains duplicate direct source paths".into());
        }
    }
    Ok(sources)
}

fn collect_root_interface_symbols(
    view: &WitView,
    root: &WitPackage,
    source_files: &BTreeMap<String, Arc<str>>,
    packages: &sai_intern::PackageInterner,
    symbols_interner: &sai_intern::Interner,
    reasons: &mut BTreeSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
) {
    if !root.worlds.is_empty() {
        reasons.insert("WIT world declarations are outside this inventory slice".into());
    }

    let selected_identity = format!("{}:{}", root.name.namespace, root.name.name);
    let mut emitted_paths = HashSet::new();
    for (interface_index, interface) in root.interfaces.iter_enumerated() {
        reasons.insert(format!(
            "WIT interface `{}` has no package interface symbol inventory",
            interface.name
        ));
        let Some(view_interface) = view
            .interfaces
            .values()
            .find(|candidate| candidate.wit == interface_index)
        else {
            reasons.insert(format!(
                "WIT interface `{}` is absent from the selected view",
                interface.name
            ));
            continue;
        };
        let expected_interface_name = wit_name_to_view_key(&interface.name).ok();
        if view_interface.wit != interface_index
            || !symbols_interner
                .resolve(view_interface.sai_name)
                .is_ok_and(|name| {
                    expected_interface_name
                        .as_ref()
                        .is_some_and(|expected| name.as_ref() == expected)
                })
        {
            reasons.insert(format!(
                "WIT interface `{}` does not match its selected view identity",
                interface.name
            ));
            continue;
        }
        if !view_interface.shadowed.is_empty() {
            reasons.insert(format!(
                "WIT interface `{}` contains shadowed view members",
                interface.name
            ));
        }

        let mut model_functions_seen = HashSet::new();
        for (member_name, member) in &view_interface.members {
            let Some(item) = interface.items.get(member.item) else {
                reasons.insert(format!(
                    "WIT view member `{}`.{} has an out-of-range model item",
                    interface.name, member_name
                ));
                continue;
            };
            let WitItemKind::Func(function) = &item.kind else {
                reasons.insert(format!(
                    "WIT interface item `{}`.{} is not a standalone function",
                    interface.name, item.name
                ));
                continue;
            };
            if !model_functions_seen.insert(member.item) {
                reasons.insert(format!(
                    "WIT function `{}`.{} appears more than once in the selected view",
                    interface.name, member_name
                ));
                continue;
            }
            if member.kind != WitViewDefKind::Fn
                || member.unsupported.is_some()
                || item.unsupported.is_some()
                || function.is_async
                || function_uses_unsupported_type(function)
            {
                reasons.insert(format!(
                    "WIT function `{}`.{} uses an unsupported declaration form",
                    interface.name, member_name
                ));
                continue;
            }
            let canonical_member_name = match wit_name_to_sai(&item.name, false) {
                Ok(name) => name,
                Err(_) => {
                    reasons.insert(format!(
                        "WIT function `{}`.{} has no supported SAI member spelling",
                        interface.name, item.name
                    ));
                    continue;
                }
            };
            let expected_member_name = canonical_member_name
                .strip_prefix("r#")
                .unwrap_or(&canonical_member_name);
            if member_name.as_str() != expected_member_name {
                reasons.insert(format!(
                    "WIT view member `{}`.{} does not match its model function name",
                    interface.name, member_name
                ));
                continue;
            }
            let Some(definition) = view.definition(member.def) else {
                reasons.insert(format!(
                    "WIT function `{}`.{} has no selected view definition",
                    interface.name, member_name
                ));
                continue;
            };
            if definition.entity
                != (WitViewEntity::Item {
                    interface: interface_index,
                    item: member.item,
                })
                || definition.kind != WitViewDefKind::Fn
            {
                reasons.insert(format!(
                    "WIT function `{}`.{} view definition does not identify its model item",
                    interface.name, member_name
                ));
                continue;
            }
            let Some(source_identity) =
                function_source_identity(item, &interface.name, source_files)
            else {
                reasons.insert(format!(
                    "WIT function `{}`.{} has no exact validated declaration span",
                    interface.name, member_name
                ));
                continue;
            };
            let path = match view.def_path(member.def) {
                Ok(Some(path)) => path,
                _ => {
                    reasons.insert(format!(
                        "WIT function `{}`.{} has no canonical definition path",
                        interface.name, member_name
                    ));
                    continue;
                }
            };
            let expected_interface_name = match wit_name_to_view_key(&interface.name) {
                Ok(name) => name,
                Err(_) => {
                    reasons.insert(format!(
                        "WIT interface `{}` has no supported SAI view name",
                        interface.name
                    ));
                    continue;
                }
            };
            if !view_definition_path_matches_member(
                &path,
                &definition.canonical_path,
                &expected_interface_name,
                member_name,
                symbols_interner,
            ) {
                reasons.insert(format!(
                    "WIT function `{}`.{} does not match its canonical view definition path",
                    interface.name, member_name
                ));
                continue;
            }
            let def_path_hash = match DefPathHash::compute(&path, packages, symbols_interner) {
                Ok(hash) if hash == definition.path_hash => Fingerprint::new(hash.raw()),
                _ => {
                    reasons.insert(format!(
                        "WIT function `{}`.{} definition path hash is inconsistent",
                        interface.name, member_name
                    ));
                    continue;
                }
            };
            let canonical_path = format!(
                "{selected_identity}/{}.{}",
                interface.name, canonical_member_name
            );
            if !emitted_paths.insert(canonical_path.clone()) {
                reasons.insert(format!(
                    "WIT function canonical path `{canonical_path}` is duplicated"
                ));
                continue;
            }
            let fingerprint = declaration_fingerprint_from_token_pairs(
                SymbolKind::AssocFn,
                &source_identity.token_pairs,
            );
            current_symbols.push(CurrentSymbol::new(
                canonical_path,
                def_path_hash,
                SymbolKind::AssocFn,
                fingerprint,
            ));
        }

        let expected_interface_name = match wit_name_to_view_key(&interface.name) {
            Ok(name) => name,
            Err(_) => {
                reasons.insert(format!(
                    "WIT interface `{}` has no supported SAI view name",
                    interface.name
                ));
                continue;
            }
        };
        for (item_index, item) in interface.items.iter_enumerated() {
            match &item.kind {
                WitItemKind::Func(_) => {
                    if !model_functions_seen.contains(&item_index) {
                        reasons.insert(format!(
                            "WIT function `{}`.{} is absent from the selected view",
                            interface.name, item.name
                        ));
                    }
                }
                WitItemKind::Resource(_) => collect_root_interface_resource(
                    view,
                    view_interface,
                    interface_index,
                    item_index,
                    item,
                    &interface.name,
                    &selected_identity,
                    &expected_interface_name,
                    source_files,
                    packages,
                    symbols_interner,
                    &mut emitted_paths,
                    reasons,
                    current_symbols,
                ),
                WitItemKind::Enum(_) => collect_root_interface_enum(
                    view,
                    view_interface,
                    interface_index,
                    item_index,
                    item,
                    &interface.name,
                    &selected_identity,
                    &expected_interface_name,
                    source_files,
                    packages,
                    symbols_interner,
                    &mut emitted_paths,
                    reasons,
                    current_symbols,
                ),
                WitItemKind::Alias(_) => collect_root_interface_alias(
                    view,
                    view_interface,
                    interface_index,
                    item_index,
                    item,
                    &interface.name,
                    &selected_identity,
                    &expected_interface_name,
                    source_files,
                    packages,
                    symbols_interner,
                    &mut emitted_paths,
                    reasons,
                    current_symbols,
                ),
                _ => {
                    reasons.insert(format!(
                        "WIT interface item `{}`.{} is outside this inventory slice",
                        interface.name, item.name
                    ));
                }
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_root_interface_alias(
    view: &WitView,
    view_interface: &sai_wit::ViewInterface,
    interface_index: WitIfaceIdx,
    item_index: WitItemIdx,
    item: &WitItem,
    interface_name: &str,
    selected_identity: &str,
    expected_interface_name: &str,
    source_files: &BTreeMap<String, Arc<str>>,
    packages: &sai_intern::PackageInterner,
    symbols_interner: &sai_intern::Interner,
    emitted_paths: &mut HashSet<String>,
    reasons: &mut BTreeSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
) {
    let WitItemKind::Alias(target) = &item.kind else {
        reasons.insert(format!(
            "WIT item `{interface_name}.{}` is not a direct type alias",
            item.name
        ));
        return;
    };
    if item.unsupported.is_some() || function_uses_unsupported_type_in_alias(target) {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` uses an unsupported declaration or target type",
            item.name
        ));
        return;
    }

    let Some(source_alias) = alias_source_identity(item, interface_name, source_files) else {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` has no exact validated declaration and name span",
            item.name
        ));
        return;
    };
    let alias_sai_name = match wit_name_to_sai(&item.name, true) {
        Ok(name) => name,
        Err(_) => {
            reasons.insert(format!(
                "WIT alias `{interface_name}.{}` has no supported SAI type spelling",
                item.name
            ));
            return;
        }
    };
    let alias_view_name = alias_sai_name.strip_prefix("r#").unwrap_or(&alias_sai_name);
    let Some(view_member) = view_interface.members.get(alias_view_name) else {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` is absent from the selected view",
            item.name
        ));
        return;
    };
    if view_member.item != item_index
        || view_member.kind != WitViewDefKind::Alias
        || view_member.unsupported.is_some()
    {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` does not match its selected view item",
            item.name
        ));
        return;
    }

    let alias_entity = WitViewEntity::Item {
        interface: interface_index,
        item: item_index,
    };
    let Some(alias_def) = view.definition_for_entity(alias_entity) else {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` has no selected view definition",
            item.name
        ));
        return;
    };
    if view_member.def != alias_def {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` view member does not identify its model item",
            item.name
        ));
        return;
    }
    let Some(alias_definition) = view.definition(alias_def) else {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` has no current selected view definition",
            item.name
        ));
        return;
    };
    if !view_definition_matches_entity(alias_definition, alias_entity, WitViewDefKind::Alias) {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` view definition does not identify its model item",
            item.name
        ));
        return;
    }
    let alias_path = match view.def_path(alias_def) {
        Ok(Some(path)) => path,
        _ => {
            reasons.insert(format!(
                "WIT alias `{interface_name}.{}` has no canonical definition path",
                item.name
            ));
            return;
        }
    };
    if !alias_path_matches_selected_package(&alias_path, view.package) {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` definition path belongs to a different package",
            item.name
        ));
        return;
    }
    if !view_definition_path_matches_alias(
        &alias_path,
        &alias_definition.canonical_path,
        expected_interface_name,
        alias_view_name,
        symbols_interner,
    ) {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` does not match its canonical view definition path",
            item.name
        ));
        return;
    }
    let Some(alias_hash) = verified_view_path_hash(
        &alias_path,
        alias_definition.path_hash,
        packages,
        symbols_interner,
    ) else {
        reasons.insert(format!(
            "WIT alias `{interface_name}.{}` definition path hash is inconsistent",
            item.name
        ));
        return;
    };

    let canonical_alias_path = format!("{selected_identity}/{interface_name}.{alias_view_name}");
    if !emitted_paths.insert(canonical_alias_path.clone()) {
        reasons.insert(format!(
            "WIT alias canonical path `{canonical_alias_path}` is duplicated"
        ));
        return;
    }
    current_symbols.push(CurrentSymbol::new(
        canonical_alias_path,
        alias_hash,
        SymbolKind::Alias,
        declaration_fingerprint_from_token_pairs(SymbolKind::Alias, &source_alias.token_pairs),
    ));
}

fn alias_source_identity(
    item: &WitItem,
    interface_name: &str,
    source_files: &BTreeMap<String, Arc<str>>,
) -> Option<sai_wit::AuthoredInterfaceAliasTokens> {
    let name_span = item.span.as_ref()?;
    let source = source_files.get(&name_span.path)?;
    let name_text = source.get(name_span.range.clone())?;
    if normalized_wit_name(name_text) != Some(item.name.as_str()) {
        return None;
    }
    let scanned =
        authored_interface_alias_tokens(source, interface_name, &item.name, &name_span.range)?;
    if scanned.declaration_range.start > name_span.range.start
        || name_span.range.end > scanned.declaration_range.end
        || scanned.declaration_range.is_empty()
        || source
            .get(scanned.declaration_range.clone())
            .is_none_or(|declaration| !declaration.ends_with(';'))
        || scanned
            .token_pairs
            .last()
            .map(|(kind, text)| (kind.as_str(), text.as_str()))
            != Some(("Semicolon", ";"))
        || scanned
            .token_pairs
            .iter()
            .filter(|(kind, text)| {
                text.is_empty() && matches!(kind.as_str(), "Id" | "ExplicitId" | "StringLiteral")
            })
            .count()
            != 1
    {
        return None;
    }
    Some(scanned)
}

fn function_uses_unsupported_type_in_alias(ty: &WitType) -> bool {
    match ty {
        WitType::Unsupported(_) | WitType::Future(_) | WitType::Stream(_) => true,
        WitType::List(inner) | WitType::Option(inner) => {
            function_uses_unsupported_type_in_alias(inner)
        }
        WitType::Result { ok, err } => {
            ok.as_ref()
                .is_some_and(|ty| function_uses_unsupported_type_in_alias(ty))
                || err
                    .as_ref()
                    .is_some_and(|ty| function_uses_unsupported_type_in_alias(ty))
        }
        WitType::Tuple(elements) => elements.iter().any(function_uses_unsupported_type_in_alias),
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_root_interface_resource(
    view: &WitView,
    view_interface: &sai_wit::ViewInterface,
    interface_index: WitIfaceIdx,
    item_index: WitItemIdx,
    item: &WitItem,
    interface_name: &str,
    selected_identity: &str,
    expected_interface_name: &str,
    source_files: &BTreeMap<String, Arc<str>>,
    packages: &sai_intern::PackageInterner,
    symbols_interner: &sai_intern::Interner,
    emitted_paths: &mut HashSet<String>,
    reasons: &mut BTreeSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
) {
    let WitItemKind::Resource(members) = &item.kind else {
        reasons.insert(format!(
            "WIT item `{interface_name}.{}` is not a resource",
            item.name
        ));
        return;
    };

    let Some(source_resource) = resource_source_identity(item, interface_name, source_files) else {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` has no exact validated declaration span",
            item.name
        ));
        return;
    };

    let resource_sai_name = match wit_name_to_sai(&item.name, true) {
        Ok(name) => name,
        Err(_) => {
            reasons.insert(format!(
                "WIT resource `{interface_name}.{}` has no supported SAI type spelling",
                item.name
            ));
            return;
        }
    };
    let resource_view_name = resource_sai_name
        .strip_prefix("r#")
        .unwrap_or(&resource_sai_name);
    let Some(view_member) = view_interface.members.get(resource_view_name) else {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` is absent from the selected view",
            item.name
        ));
        return;
    };
    if view_member.item != item_index || view_member.kind != WitViewDefKind::Resource {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` does not match its selected view item",
            item.name
        ));
        return;
    }
    if item.unsupported.is_some() {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` uses an unsupported declaration form",
            item.name
        ));
    }
    let resource_entity = WitViewEntity::Item {
        interface: interface_index,
        item: item_index,
    };
    let Some(resource_def) = view.definition_for_entity(resource_entity) else {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` has no selected view definition",
            item.name
        ));
        return;
    };
    if view_member.def != resource_def {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` view member does not identify its model item",
            item.name
        ));
        return;
    }
    let Some(resource_definition) = view.definition(resource_def) else {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` has no current selected view definition",
            item.name
        ));
        return;
    };
    if resource_definition.entity != resource_entity
        || resource_definition.kind != WitViewDefKind::Resource
    {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` view definition does not identify its model item",
            item.name
        ));
        return;
    }
    let resource_path = match view.def_path(resource_def) {
        Ok(Some(path)) => path,
        _ => {
            reasons.insert(format!(
                "WIT resource `{interface_name}.{}` has no canonical definition path",
                item.name
            ));
            return;
        }
    };
    if !view_definition_path_matches_resource(
        &resource_path,
        &resource_definition.canonical_path,
        expected_interface_name,
        resource_view_name,
        symbols_interner,
    ) {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` does not match its canonical view definition path",
            item.name
        ));
        return;
    }
    let resource_hash = match DefPathHash::compute(&resource_path, packages, symbols_interner) {
        Ok(hash) if hash == resource_definition.path_hash => Fingerprint::new(hash.raw()),
        _ => {
            reasons.insert(format!(
                "WIT resource `{interface_name}.{}` definition path hash is inconsistent",
                item.name
            ));
            return;
        }
    };

    let canonical_resource_path =
        format!("{selected_identity}/{interface_name}.{resource_view_name}");
    if emitted_paths.insert(canonical_resource_path.clone()) {
        current_symbols.push(CurrentSymbol::new(
            canonical_resource_path,
            resource_hash,
            SymbolKind::Resource,
            declaration_fingerprint_from_token_pairs(
                SymbolKind::Resource,
                &source_resource.token_pairs,
            ),
        ));
    } else {
        reasons.insert(format!(
            "WIT resource canonical path `{canonical_resource_path}` is duplicated"
        ));
        return;
    }

    if !resource_members_match_source(item, members, &source_resource, source_files) {
        reasons.insert(format!(
            "WIT resource `{interface_name}.{}` scanner members do not match parsed member names, kinds, order, or spans",
            item.name
        ));
        return;
    }

    for (member_index, (member, source_member)) in members
        .iter()
        .zip(source_resource.members.iter())
        .enumerate()
    {
        let member_view_name = match member.kind {
            WitMemberKind::Constructor => "new".to_owned(),
            WitMemberKind::Method | WitMemberKind::Static => {
                match wit_name_to_view_key(&member.name) {
                    Ok(name) => name,
                    Err(_) => {
                        reasons.insert(format!(
                            "WIT resource member `{interface_name}.{}`.{} has no supported SAI member spelling",
                            item.name, member.name
                        ));
                        continue;
                    }
                }
            }
        };
        if member.unsupported.is_some()
            || member.func.is_async
            || function_uses_unsupported_type(&member.func)
        {
            reasons.insert(format!(
                "WIT resource member `{interface_name}.{}`.{} uses an unsupported signature",
                item.name, member.name
            ));
            continue;
        }
        let Ok(member_ordinal) = u32::try_from(member_index) else {
            reasons.insert(format!(
                "WIT resource `{interface_name}.{}` has too many members",
                item.name
            ));
            continue;
        };
        let entity = WitViewEntity::ResourceMember {
            interface: interface_index,
            item: item_index,
            member: member_ordinal,
        };
        let Some(member_def) = view.definition_for_entity(entity) else {
            reasons.insert(format!(
                "WIT resource member `{interface_name}.{}`.{} is missing or shadowed in the selected view",
                item.name, member.name
            ));
            continue;
        };
        let Some(member_definition) = view.definition(member_def) else {
            reasons.insert(format!(
                "WIT resource member `{interface_name}.{}`.{} has no current selected view definition",
                item.name, member.name
            ));
            continue;
        };
        if member_definition.entity != entity || member_definition.kind != WitViewDefKind::AssocFn {
            reasons.insert(format!(
                "WIT resource member `{interface_name}.{}`.{} view definition does not identify its model member",
                item.name, member.name
            ));
            continue;
        }
        let member_path = match view.def_path(member_def) {
            Ok(Some(path)) => path,
            _ => {
                reasons.insert(format!(
                    "WIT resource member `{interface_name}.{}`.{} has no canonical definition path",
                    item.name, member.name
                ));
                continue;
            }
        };
        if !view_definition_path_matches_resource_member(
            &member_path,
            &member_definition.canonical_path,
            expected_interface_name,
            resource_view_name,
            &member_view_name,
            symbols_interner,
        ) {
            reasons.insert(format!(
                "WIT resource member `{interface_name}.{}`.{} does not match its canonical view definition path",
                item.name, member.name
            ));
            continue;
        }
        let member_hash = match DefPathHash::compute(&member_path, packages, symbols_interner) {
            Ok(hash) if hash == member_definition.path_hash => Fingerprint::new(hash.raw()),
            _ => {
                reasons.insert(format!(
                        "WIT resource member `{interface_name}.{}`.{} definition path hash is inconsistent",
                        item.name, member.name
                    ));
                continue;
            }
        };
        let canonical_member_path =
            format!("{selected_identity}/{interface_name}.{resource_view_name}.{member_view_name}");
        if !emitted_paths.insert(canonical_member_path.clone()) {
            reasons.insert(format!(
                "WIT resource member canonical path `{canonical_member_path}` is duplicated"
            ));
            continue;
        }
        current_symbols.push(CurrentSymbol::new(
            canonical_member_path,
            member_hash,
            SymbolKind::AssocFn,
            declaration_fingerprint_from_token_pairs(
                SymbolKind::AssocFn,
                &source_member.token_pairs,
            ),
        ));
    }
}

#[allow(clippy::too_many_arguments)]
fn collect_root_interface_enum(
    view: &WitView,
    view_interface: &sai_wit::ViewInterface,
    interface_index: WitIfaceIdx,
    item_index: WitItemIdx,
    item: &WitItem,
    interface_name: &str,
    selected_identity: &str,
    expected_interface_name: &str,
    source_files: &BTreeMap<String, Arc<str>>,
    packages: &sai_intern::PackageInterner,
    symbols_interner: &sai_intern::Interner,
    emitted_paths: &mut HashSet<String>,
    reasons: &mut BTreeSet<String>,
    current_symbols: &mut Vec<CurrentSymbol>,
) {
    let WitItemKind::Enum(cases) = &item.kind else {
        reasons.insert(format!(
            "WIT item `{interface_name}.{}` is not a unit enum",
            item.name
        ));
        return;
    };
    if item.unsupported.is_some() {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` uses an unsupported declaration form",
            item.name
        ));
        return;
    }
    let Some(source_enum) = enum_source_identity(item, interface_name, cases, source_files) else {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` has no exact validated declaration and case spans",
            item.name
        ));
        return;
    };

    let enum_sai_name = match wit_name_to_sai(&item.name, true) {
        Ok(name) => name,
        Err(_) => {
            reasons.insert(format!(
                "WIT enum `{interface_name}.{}` has no supported SAI type spelling",
                item.name
            ));
            return;
        }
    };
    let enum_view_name = enum_sai_name.strip_prefix("r#").unwrap_or(&enum_sai_name);
    let Some(view_member) = view_interface.members.get(enum_view_name) else {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` is absent from the selected view",
            item.name
        ));
        return;
    };
    if view_member.item != item_index
        || view_member.kind != WitViewDefKind::Enum
        || view_member.unsupported.is_some()
    {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` does not match its selected view item",
            item.name
        ));
        return;
    }
    let enum_entity = WitViewEntity::Item {
        interface: interface_index,
        item: item_index,
    };
    let Some(enum_def) = view.definition_for_entity(enum_entity) else {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` has no selected view definition",
            item.name
        ));
        return;
    };
    if view_member.def != enum_def {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` view member does not identify its model item",
            item.name
        ));
        return;
    }
    let Some(enum_definition) = view.definition(enum_def) else {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` has no current selected view definition",
            item.name
        ));
        return;
    };
    if !view_definition_matches_entity(enum_definition, enum_entity, WitViewDefKind::Enum) {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` view definition does not identify its model item",
            item.name
        ));
        return;
    }
    let enum_path = match view.def_path(enum_def) {
        Ok(Some(path)) => path,
        _ => {
            reasons.insert(format!(
                "WIT enum `{interface_name}.{}` has no canonical definition path",
                item.name
            ));
            return;
        }
    };
    if !view_definition_path_matches_enum(
        &enum_path,
        &enum_definition.canonical_path,
        expected_interface_name,
        enum_view_name,
        symbols_interner,
    ) {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` does not match its canonical view definition path",
            item.name
        ));
        return;
    }
    let Some(enum_hash) = verified_view_path_hash(
        &enum_path,
        enum_definition.path_hash,
        packages,
        symbols_interner,
    ) else {
        reasons.insert(format!(
            "WIT enum `{interface_name}.{}` definition path hash is inconsistent",
            item.name
        ));
        return;
    };

    let canonical_enum_path = format!("{selected_identity}/{interface_name}.{enum_view_name}");
    if !emitted_paths.insert(canonical_enum_path.clone()) {
        reasons.insert(format!(
            "WIT enum canonical path `{canonical_enum_path}` is duplicated"
        ));
        return;
    }
    current_symbols.push(CurrentSymbol::new(
        canonical_enum_path,
        enum_hash,
        SymbolKind::Enum,
        declaration_fingerprint_from_token_pairs(SymbolKind::Enum, &source_enum.token_pairs),
    ));

    for (case_index, (case_name, source_case)) in
        cases.iter().zip(source_enum.cases.iter()).enumerate()
    {
        let case_sai_name = match wit_name_to_sai(case_name, true) {
            Ok(name) => name,
            Err(_) => {
                reasons.insert(format!(
                    "WIT enum case `{interface_name}.{}`.{case_name} has no supported SAI spelling",
                    item.name
                ));
                continue;
            }
        };
        let case_view_name = case_sai_name.strip_prefix("r#").unwrap_or(&case_sai_name);
        let Ok(case_ordinal) = u32::try_from(case_index) else {
            reasons.insert(format!(
                "WIT enum `{interface_name}.{}` has too many cases",
                item.name
            ));
            continue;
        };
        let case_entity = WitViewEntity::Variant {
            interface: interface_index,
            item: item_index,
            case: case_ordinal,
        };
        let Some(case_def) = view.definition_for_entity(case_entity) else {
            reasons.insert(format!(
                "WIT enum case `{interface_name}.{}`.{case_name} is missing from the selected view",
                item.name
            ));
            continue;
        };
        let Some(case_definition) = view.definition(case_def) else {
            reasons.insert(format!(
                "WIT enum case `{interface_name}.{}`.{case_name} has no current selected view definition",
                item.name
            ));
            continue;
        };
        if !view_definition_matches_entity(case_definition, case_entity, WitViewDefKind::Variant) {
            reasons.insert(format!(
                "WIT enum case `{interface_name}.{}`.{case_name} view definition does not identify its model case",
                item.name
            ));
            continue;
        }
        let case_path = match view.def_path(case_def) {
            Ok(Some(path)) => path,
            _ => {
                reasons.insert(format!(
                    "WIT enum case `{interface_name}.{}`.{case_name} has no canonical definition path",
                    item.name
                ));
                continue;
            }
        };
        if !view_definition_path_matches_enum_case(
            &case_path,
            &case_definition.canonical_path,
            expected_interface_name,
            enum_view_name,
            case_view_name,
            symbols_interner,
        ) {
            reasons.insert(format!(
                "WIT enum case `{interface_name}.{}`.{case_name} does not match its canonical view definition path",
                item.name
            ));
            continue;
        }
        let Some(case_hash) = verified_view_path_hash(
            &case_path,
            case_definition.path_hash,
            packages,
            symbols_interner,
        ) else {
            reasons.insert(format!(
                "WIT enum case `{interface_name}.{}`.{case_name} definition path hash is inconsistent",
                item.name
            ));
            continue;
        };
        let canonical_case_path =
            format!("{selected_identity}/{interface_name}.{enum_view_name}.{case_view_name}");
        if !emitted_paths.insert(canonical_case_path.clone()) {
            reasons.insert(format!(
                "WIT enum case canonical path `{canonical_case_path}` is duplicated"
            ));
            continue;
        }
        current_symbols.push(CurrentSymbol::new(
            canonical_case_path,
            case_hash,
            SymbolKind::Variant,
            declaration_fingerprint_from_token_pairs(SymbolKind::Variant, &source_case.token_pairs),
        ));
    }
}

fn resource_source_identity(
    item: &WitItem,
    interface_name: &str,
    source_files: &BTreeMap<String, Arc<str>>,
) -> Option<sai_wit::AuthoredInterfaceResourceTokens> {
    let name_span = item.span.as_ref()?;
    let declaration_span = item.declaration_span.as_ref()?;
    if name_span.path != declaration_span.path
        || declaration_span.range.start > name_span.range.start
        || name_span.range.end > declaration_span.range.end
        || declaration_span.range.is_empty()
    {
        return None;
    }
    let source = source_files.get(&name_span.path)?;
    let name_text = source.get(name_span.range.clone())?;
    if normalized_wit_name(name_text) != Some(item.name.as_str()) {
        return None;
    }
    let declaration_text = source.get(declaration_span.range.clone())?;
    if !(declaration_text.ends_with(';') || declaration_text.ends_with('}')) {
        return None;
    }
    let scanned =
        authored_interface_resource_tokens(source, interface_name, &item.name, &name_span.range)?;
    if scanned.declaration_range != declaration_span.range
        || scanned.token_pairs != *item.declaration_token_pairs.as_ref()?
    {
        return None;
    }
    let name_token_count = scanned
        .token_pairs
        .iter()
        .filter(|(kind, text)| {
            text.is_empty() && matches!(kind.as_str(), "Id" | "ExplicitId" | "StringLiteral")
        })
        .count();
    (name_token_count == 1).then_some(scanned)
}

fn enum_source_identity(
    item: &WitItem,
    interface_name: &str,
    cases: &[String],
    source_files: &BTreeMap<String, Arc<str>>,
) -> Option<sai_wit::AuthoredInterfaceEnumTokens> {
    let name_span = item.span.as_ref()?;
    let source = source_files.get(&name_span.path)?;
    let name_text = source.get(name_span.range.clone())?;
    if normalized_wit_name(name_text) != Some(item.name.as_str()) {
        return None;
    }
    let scanned =
        authored_interface_enum_tokens(source, interface_name, &item.name, &name_span.range)?;
    if scanned.cases.len() != cases.len()
        || scanned.declaration_range.start > name_span.range.start
        || name_span.range.end > scanned.declaration_range.end
        || scanned.declaration_range.is_empty()
        || source
            .get(scanned.declaration_range.clone())
            .is_none_or(|declaration| !declaration.ends_with('}'))
        || scanned
            .token_pairs
            .last()
            .map(|(kind, text)| (kind.as_str(), text.as_str()))
            != Some(("RightBrace", "}"))
        || scanned
            .token_pairs
            .iter()
            .filter(|(kind, text)| {
                text.is_empty() && matches!(kind.as_str(), "Id" | "ExplicitId" | "StringLiteral")
            })
            .count()
            != 1
    {
        return None;
    }
    let mut previous_end = scanned.declaration_range.start;
    for (model_case, source_case) in cases.iter().zip(&scanned.cases) {
        let name_text = source.get(source_case.name_range.clone())?;
        if source_case.name.as_str() != model_case.as_str()
            || normalized_wit_name(name_text) != Some(model_case.as_str())
            || source_case.declaration_range.start < scanned.declaration_range.start
            || source_case.declaration_range.end > scanned.declaration_range.end
            || source_case.declaration_range.start < previous_end
            || source_case.declaration_range.is_empty()
            || source_case.name_range.start < source_case.declaration_range.start
            || source_case.name_range.end > source_case.declaration_range.end
            || source_case
                .token_pairs
                .iter()
                .filter(|(kind, text)| {
                    text.is_empty()
                        && matches!(kind.as_str(), "Id" | "ExplicitId" | "StringLiteral")
                })
                .count()
                != 1
        {
            return None;
        }
        previous_end = source_case.declaration_range.end;
    }
    Some(scanned)
}

fn resource_members_match_source(
    item: &WitItem,
    members: &[sai_wit::WitMember],
    source_resource: &sai_wit::AuthoredInterfaceResourceTokens,
    source_files: &BTreeMap<String, Arc<str>>,
) -> bool {
    if source_resource.members.len() != members.len() {
        return false;
    }
    let Some(resource_span) = item.declaration_span.as_ref() else {
        return false;
    };
    let Some(source) = source_files.get(&resource_span.path) else {
        return false;
    };
    let mut previous_end = resource_span.range.start;
    for (member, source_member) in members.iter().zip(&source_resource.members) {
        if source_member.name != member.name || source_member.kind != member.kind {
            return false;
        }
        let Some(name_text) = source.get(source_member.name_range.clone()) else {
            return false;
        };
        if normalized_wit_name(name_text) != Some(member.name.as_str())
            || source_member.declaration_range.start < resource_span.range.start
            || source_member.declaration_range.end > resource_span.range.end
            || source_member.declaration_range.start < previous_end
            || source_member.declaration_range.is_empty()
            || source_member.name_range.start < source_member.declaration_range.start
            || source_member.name_range.end > source_member.declaration_range.end
            || source
                .get(source_member.declaration_range.clone())
                .is_none_or(|declaration| !declaration.ends_with(';'))
        {
            return false;
        }
        if member_name_token_count(&source_member.token_pairs, member.kind) != 1
            || source_member
                .token_pairs
                .last()
                .map(|(kind, text)| (kind.as_str(), text.as_str()))
                != Some(("Semicolon", ";"))
        {
            return false;
        }
        previous_end = source_member.declaration_range.end;
    }
    true
}

fn member_name_token_count(token_pairs: &[(String, String)], kind: WitMemberKind) -> usize {
    token_pairs
        .iter()
        .filter(|(token_kind, text)| {
            text.is_empty()
                && (matches!(token_kind.as_str(), "Id" | "ExplicitId" | "StringLiteral")
                    || (kind == WitMemberKind::Constructor && token_kind == "Constructor"))
        })
        .count()
}

fn wit_name_to_view_key(name: &str) -> Result<String, sai_wit::WitViewError> {
    wit_name_to_sai(name, false).map(|name| name.strip_prefix("r#").unwrap_or(&name).to_owned())
}

fn view_definition_path_matches_member(
    path: &sai_hir::DefPath,
    definition_canonical_path: &str,
    expected_interface: &str,
    expected_member: &str,
    symbols_interner: &sai_intern::Interner,
) -> bool {
    let [interface_component, member_component] = path.data() else {
        return false;
    };
    let (DefPathData::Interface(interface_symbol), DefPathData::ValueNs(member_symbol)) =
        (&interface_component.data, &member_component.data)
    else {
        return false;
    };
    if !symbols_interner
        .resolve(*interface_symbol)
        .is_ok_and(|name| name.as_ref() == expected_interface)
        || !symbols_interner
            .resolve(*member_symbol)
            .is_ok_and(|name| name.as_ref() == expected_member)
    {
        return false;
    }
    let expected_canonical_path = format!(
        "interface[{}:{};{}]/value[{}:{};{}]/",
        expected_interface.len(),
        expected_interface,
        interface_component.disambiguator,
        expected_member.len(),
        expected_member,
        member_component.disambiguator,
    );
    definition_canonical_path == expected_canonical_path
}

fn view_definition_path_matches_resource(
    path: &sai_hir::DefPath,
    definition_canonical_path: &str,
    expected_interface: &str,
    expected_resource: &str,
    symbols_interner: &sai_intern::Interner,
) -> bool {
    let [interface_component, resource_component] = path.data() else {
        return false;
    };
    let (DefPathData::Interface(interface_symbol), DefPathData::TypeNs(resource_symbol)) =
        (&interface_component.data, &resource_component.data)
    else {
        return false;
    };
    if interface_component.disambiguator != 0
        || resource_component.disambiguator != 0
        || !symbols_interner
            .resolve(*interface_symbol)
            .is_ok_and(|name| name.as_ref() == expected_interface)
        || !symbols_interner
            .resolve(*resource_symbol)
            .is_ok_and(|name| name.as_ref() == expected_resource)
    {
        return false;
    }
    let expected_canonical_path = format!(
        "interface[{}:{};{}]/type[{}:{};{}]/",
        expected_interface.len(),
        expected_interface,
        interface_component.disambiguator,
        expected_resource.len(),
        expected_resource,
        resource_component.disambiguator,
    );
    definition_canonical_path == expected_canonical_path
}

fn view_definition_path_matches_enum(
    path: &sai_hir::DefPath,
    definition_canonical_path: &str,
    expected_interface: &str,
    expected_enum: &str,
    symbols_interner: &sai_intern::Interner,
) -> bool {
    let [interface_component, enum_component] = path.data() else {
        return false;
    };
    let (DefPathData::Interface(interface_symbol), DefPathData::TypeNs(enum_symbol)) =
        (&interface_component.data, &enum_component.data)
    else {
        return false;
    };
    if interface_component.disambiguator != 0
        || enum_component.disambiguator != 0
        || !symbols_interner
            .resolve(*interface_symbol)
            .is_ok_and(|name| name.as_ref() == expected_interface)
        || !symbols_interner
            .resolve(*enum_symbol)
            .is_ok_and(|name| name.as_ref() == expected_enum)
    {
        return false;
    }
    let expected_canonical_path = format!(
        "interface[{}:{};{}]/type[{}:{};{}]/",
        expected_interface.len(),
        expected_interface,
        interface_component.disambiguator,
        expected_enum.len(),
        expected_enum,
        enum_component.disambiguator,
    );
    definition_canonical_path == expected_canonical_path
}

fn view_definition_path_matches_alias(
    path: &sai_hir::DefPath,
    definition_canonical_path: &str,
    expected_interface: &str,
    expected_alias: &str,
    symbols_interner: &sai_intern::Interner,
) -> bool {
    let [interface_component, alias_component] = path.data() else {
        return false;
    };
    let (DefPathData::Interface(interface_symbol), DefPathData::TypeNs(alias_symbol)) =
        (&interface_component.data, &alias_component.data)
    else {
        return false;
    };
    if interface_component.disambiguator != 0
        || alias_component.disambiguator != 0
        || !symbols_interner
            .resolve(*interface_symbol)
            .is_ok_and(|name| name.as_ref() == expected_interface)
        || !symbols_interner
            .resolve(*alias_symbol)
            .is_ok_and(|name| name.as_ref() == expected_alias)
    {
        return false;
    }
    let expected_canonical_path = format!(
        "interface[{}:{};{}]/type[{}:{};{}]/",
        expected_interface.len(),
        expected_interface,
        interface_component.disambiguator,
        expected_alias.len(),
        expected_alias,
        alias_component.disambiguator,
    );
    definition_canonical_path == expected_canonical_path
}

fn alias_path_matches_selected_package(
    path: &sai_hir::DefPath,
    selected_package: sai_intern::PackageId,
) -> bool {
    path.package() == selected_package
}

fn view_definition_matches_entity(
    definition: &sai_wit::WitViewDef,
    expected_entity: WitViewEntity,
    expected_kind: WitViewDefKind,
) -> bool {
    definition.entity == expected_entity && definition.kind == expected_kind
}

fn verified_view_path_hash(
    path: &sai_hir::DefPath,
    expected_hash: DefPathHash,
    packages: &sai_intern::PackageInterner,
    symbols_interner: &sai_intern::Interner,
) -> Option<Fingerprint> {
    let hash = DefPathHash::compute(path, packages, symbols_interner).ok()?;
    (hash == expected_hash).then(|| Fingerprint::new(hash.raw()))
}

fn view_definition_path_matches_enum_case(
    path: &sai_hir::DefPath,
    definition_canonical_path: &str,
    expected_interface: &str,
    expected_enum: &str,
    expected_case: &str,
    symbols_interner: &sai_intern::Interner,
) -> bool {
    let [interface_component, enum_component, case_component] = path.data() else {
        return false;
    };
    let (
        DefPathData::Interface(interface_symbol),
        DefPathData::TypeNs(enum_symbol),
        DefPathData::Variant(case_symbol),
    ) = (
        &interface_component.data,
        &enum_component.data,
        &case_component.data,
    )
    else {
        return false;
    };
    if interface_component.disambiguator != 0
        || enum_component.disambiguator != 0
        || case_component.disambiguator != 0
        || !symbols_interner
            .resolve(*interface_symbol)
            .is_ok_and(|name| name.as_ref() == expected_interface)
        || !symbols_interner
            .resolve(*enum_symbol)
            .is_ok_and(|name| name.as_ref() == expected_enum)
        || !symbols_interner
            .resolve(*case_symbol)
            .is_ok_and(|name| name.as_ref() == expected_case)
    {
        return false;
    }
    let expected_canonical_path = format!(
        "interface[{}:{};{}]/type[{}:{};{}]/variant[{}:{};{}]/",
        expected_interface.len(),
        expected_interface,
        interface_component.disambiguator,
        expected_enum.len(),
        expected_enum,
        enum_component.disambiguator,
        expected_case.len(),
        expected_case,
        case_component.disambiguator,
    );
    definition_canonical_path == expected_canonical_path
}

fn view_definition_path_matches_resource_member(
    path: &sai_hir::DefPath,
    definition_canonical_path: &str,
    expected_interface: &str,
    expected_resource: &str,
    expected_member: &str,
    symbols_interner: &sai_intern::Interner,
) -> bool {
    let [interface_component, resource_component, member_component] = path.data() else {
        return false;
    };
    let (
        DefPathData::Interface(interface_symbol),
        DefPathData::TypeNs(resource_symbol),
        DefPathData::AssocFn(member_symbol),
    ) = (
        &interface_component.data,
        &resource_component.data,
        &member_component.data,
    )
    else {
        return false;
    };
    if interface_component.disambiguator != 0
        || resource_component.disambiguator != 0
        || member_component.disambiguator != 0
        || !symbols_interner
            .resolve(*interface_symbol)
            .is_ok_and(|name| name.as_ref() == expected_interface)
        || !symbols_interner
            .resolve(*resource_symbol)
            .is_ok_and(|name| name.as_ref() == expected_resource)
        || !symbols_interner
            .resolve(*member_symbol)
            .is_ok_and(|name| name.as_ref() == expected_member)
    {
        return false;
    }
    let expected_canonical_path = format!(
        "interface[{}:{};{}]/type[{}:{};{}]/assoc-fn[{}:{};{}]/",
        expected_interface.len(),
        expected_interface,
        interface_component.disambiguator,
        expected_resource.len(),
        expected_resource,
        resource_component.disambiguator,
        expected_member.len(),
        expected_member,
        member_component.disambiguator,
    );
    definition_canonical_path == expected_canonical_path
}

struct ValidatedFunctionSource {
    token_pairs: Vec<(String, String)>,
}

fn function_source_identity(
    item: &WitItem,
    interface_name: &str,
    source_files: &BTreeMap<String, Arc<str>>,
) -> Option<ValidatedFunctionSource> {
    let name_span = item.span.as_ref()?;
    let declaration_span = item.declaration_span.as_ref()?;
    if name_span.path != declaration_span.path
        || declaration_span.range.start > name_span.range.start
        || name_span.range.end > declaration_span.range.end
        || declaration_span.range.is_empty()
    {
        return None;
    }
    let source = source_files.get(&name_span.path)?;
    let name_text = source.get(name_span.range.clone())?;
    if normalized_wit_name(name_text) != Some(item.name.as_str()) {
        return None;
    }
    let declaration_text = source.get(declaration_span.range.clone())?;
    if !declaration_text.ends_with(';') {
        return None;
    }
    let scanned =
        authored_interface_function_tokens(source, interface_name, &item.name, &name_span.range)?;
    if scanned.declaration_range != declaration_span.range
        || scanned.token_pairs != *item.declaration_token_pairs.as_ref()?
    {
        return None;
    }
    let name_token_count = scanned
        .token_pairs
        .iter()
        .filter(|(kind, text)| {
            text.is_empty() && matches!(kind.as_str(), "Id" | "ExplicitId" | "StringLiteral")
        })
        .count();
    if name_token_count != 1
        || scanned
            .token_pairs
            .last()
            .map(|(kind, text)| (kind.as_str(), text.as_str()))
            != Some(("Semicolon", ";"))
    {
        return None;
    }
    Some(ValidatedFunctionSource {
        token_pairs: scanned.token_pairs,
    })
}

fn normalized_wit_name(text: &str) -> Option<&str> {
    if let Some(identifier) = text.strip_prefix('%') {
        return Some(identifier);
    }
    if let Some(identifier) = text
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return (!identifier.contains('\\')).then_some(identifier);
    }
    Some(text)
}

fn function_uses_unsupported_type(function: &sai_wit::WitFunc) -> bool {
    fn unsupported(ty: &WitType) -> bool {
        match ty {
            WitType::Unsupported(_) | WitType::Future(_) | WitType::Stream(_) => true,
            WitType::List(inner) | WitType::Option(inner) => unsupported(inner),
            WitType::Result { ok, err } => {
                ok.as_deref().is_some_and(unsupported) || err.as_deref().is_some_and(unsupported)
            }
            WitType::Tuple(items) => items.iter().any(unsupported),
            WitType::Bool
            | WitType::S8
            | WitType::S16
            | WitType::S32
            | WitType::S64
            | WitType::U8
            | WitType::U16
            | WitType::U32
            | WitType::U64
            | WitType::F32
            | WitType::F64
            | WitType::Char
            | WitType::String
            | WitType::Named(_)
            | WitType::Own(_)
            | WitType::Borrow(_) => false,
        }
    }

    function.params.iter().any(|(_, ty)| unsupported(ty))
        || function.result.as_ref().is_some_and(unsupported)
}

fn incomplete_inventory(
    inventory: PackageSymbolInventory,
    additional_reasons: BTreeSet<String>,
    additions: impl IntoIterator<Item = CurrentSymbol>,
) -> Phase1PackageSymbolInventory {
    let mut reasons = additional_reasons;
    if let InventoryCoverage::Incomplete { reason } = inventory.coverage() {
        reasons.insert(reason.clone());
    }
    let mut symbols = inventory.symbols().to_vec();
    let mut paths = symbols
        .iter()
        .map(|symbol| symbol.path().to_owned())
        .collect::<HashSet<_>>();
    for symbol in additions {
        if paths.insert(symbol.path().to_owned()) {
            symbols.push(symbol);
        } else {
            reasons.insert(format!(
                "multiple declarations share canonical symbol path `{}`",
                symbol.path()
            ));
        }
    }
    if reasons.is_empty() {
        Phase1PackageSymbolInventory::Available(PackageSymbolInventory::complete(
            inventory.file().clone(),
            inventory.package_node_key().to_owned(),
            symbols,
        ))
    } else {
        Phase1PackageSymbolInventory::Available(PackageSymbolInventory::incomplete(
            inventory.file().clone(),
            inventory.package_node_key().to_owned(),
            reasons.into_iter().collect::<Vec<_>>().join("; "),
            symbols,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ValidatedFunctionSource, alias_path_matches_selected_package, alias_source_identity,
        function_source_identity, resource_members_match_source, resource_source_identity,
        verified_view_path_hash, view_definition_matches_entity,
        view_definition_path_matches_alias, view_definition_path_matches_enum,
        view_definition_path_matches_enum_case,
    };
    use sai_hir::{DefPath, DefPathData, DefPathHash, DisambiguatedDefPathData};
    use sai_intern::{Interner, PackageInterner};
    use sai_semantic::{SymbolKind, declaration_fingerprint_from_token_pairs};
    use sai_stable_hash::Fingerprint;
    use sai_wit::{
        WitFunc, WitIfaceIdx, WitIndexVec, WitItem, WitItemIdx, WitItemKind, WitMember,
        WitMemberKind, WitSpan, WitType, WitViewDef, WitViewDefKind, WitViewEntity,
        authored_interface_alias_tokens, authored_interface_enum_tokens,
        authored_interface_function_tokens, authored_interface_resource_tokens,
    };
    use std::collections::BTreeMap;
    use std::sync::Arc;

    const SOURCE: &str =
        "package acme:sample@1.0.0;\ninterface api { greet: func(name: string) -> u32; }\n";

    fn function_item(source: &str) -> WitItem {
        let name_start = source.find("greet").expect("function name occurs");
        let name_range = name_start..name_start + "greet".len();
        let tokens = authored_interface_function_tokens(source, "api", "greet", &name_range)
            .expect("direct function tokens are found");
        WitItem {
            name: "greet".to_owned(),
            docs: None,
            since: None,
            deprecated: None,
            kind: WitItemKind::Func(WitFunc {
                params: Vec::new(),
                result: None,
                is_async: false,
            }),
            unsupported: None,
            span: Some(WitSpan {
                path: "root.wit".to_owned(),
                range: name_range,
            }),
            declaration_span: Some(WitSpan {
                path: "root.wit".to_owned(),
                range: tokens.declaration_range,
            }),
            declaration_token_pairs: Some(tokens.token_pairs),
        }
    }

    fn alias_item(source: &str, name: &str) -> WitItem {
        let name_start = source.find(name).expect("alias name occurs");
        let name_range = name_start..name_start + name.len();
        let _tokens = authored_interface_alias_tokens(source, "api", name, &name_range)
            .expect("direct alias tokens are found");
        WitItem {
            name: name.to_owned(),
            docs: None,
            since: None,
            deprecated: None,
            kind: WitItemKind::Alias(WitType::U32),
            unsupported: None,
            span: Some(WitSpan {
                path: "root.wit".to_owned(),
                range: name_range,
            }),
            declaration_span: None,
            declaration_token_pairs: None,
        }
    }

    fn source_map(source: &str) -> BTreeMap<String, Arc<str>> {
        BTreeMap::from([("root.wit".to_owned(), Arc::<str>::from(source))])
    }

    fn def_path(
        packages: &PackageInterner,
        package: sai_intern::PackageId,
        symbols: &Interner,
        data: Vec<DisambiguatedDefPathData>,
    ) -> DefPath {
        DefPath::new(package, data, packages, symbols).expect("view path uses current interners")
    }

    fn path_symbol(symbols: &Interner, name: &str) -> sai_intern::Symbol {
        symbols.intern(name).expect("path symbol interns")
    }

    fn enum_path(
        packages: &PackageInterner,
        package: sai_intern::PackageId,
        symbols: &Interner,
    ) -> DefPath {
        def_path(
            packages,
            package,
            symbols,
            vec![
                DisambiguatedDefPathData {
                    data: DefPathData::Interface(path_symbol(symbols, "api")),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(path_symbol(symbols, "Status")),
                    disambiguator: 0,
                },
            ],
        )
    }

    fn alias_path(
        packages: &PackageInterner,
        package: sai_intern::PackageId,
        symbols: &Interner,
    ) -> DefPath {
        def_path(
            packages,
            package,
            symbols,
            vec![
                DisambiguatedDefPathData {
                    data: DefPathData::Interface(path_symbol(symbols, "api")),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(path_symbol(symbols, "Count")),
                    disambiguator: 0,
                },
            ],
        )
    }

    fn enum_case_path(
        packages: &PackageInterner,
        package: sai_intern::PackageId,
        symbols: &Interner,
    ) -> DefPath {
        def_path(
            packages,
            package,
            symbols,
            vec![
                DisambiguatedDefPathData {
                    data: DefPathData::Interface(path_symbol(symbols, "api")),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(path_symbol(symbols, "Status")),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::Variant(path_symbol(symbols, "Ready")),
                    disambiguator: 0,
                },
            ],
        )
    }

    fn resource_item(source: &str) -> WitItem {
        let name_start = source.find("store").expect("resource name occurs");
        let name_range = name_start..name_start + "store".len();
        let tokens = authored_interface_resource_tokens(source, "api", "store", &name_range)
            .expect("resource declaration tokens are found");
        WitItem {
            name: "store".to_owned(),
            docs: None,
            since: None,
            deprecated: None,
            kind: WitItemKind::Resource(vec![WitMember {
                name: "read".to_owned(),
                kind: WitMemberKind::Method,
                func: WitFunc {
                    params: Vec::new(),
                    result: None,
                    is_async: false,
                },
                docs: None,
                since: None,
                deprecated: None,
                unsupported: None,
            }]),
            unsupported: None,
            span: Some(WitSpan {
                path: "root.wit".to_owned(),
                range: name_range,
            }),
            declaration_span: Some(WitSpan {
                path: "root.wit".to_owned(),
                range: tokens.declaration_range,
            }),
            declaration_token_pairs: Some(tokens.token_pairs),
        }
    }

    #[test]
    fn function_source_identity_requires_exact_same_file_spans_and_bytes() {
        let item = function_item(SOURCE);
        assert!(function_source_identity(&item, "api", &source_map(SOURCE)).is_some());

        let mut invalid_name = item.clone();
        invalid_name.span.as_mut().expect("name span").range.end += 1;
        assert!(function_source_identity(&invalid_name, "api", &source_map(SOURCE)).is_none());

        let mut invalid_path = item.clone();
        invalid_path
            .declaration_span
            .as_mut()
            .expect("declaration span")
            .path = "other.wit".to_owned();
        assert!(function_source_identity(&invalid_path, "api", &source_map(SOURCE)).is_none());

        let mut invalid_declaration = item.clone();
        invalid_declaration
            .declaration_span
            .as_mut()
            .expect("declaration span")
            .range
            .end = SOURCE.len() + 1;
        assert!(
            function_source_identity(&invalid_declaration, "api", &source_map(SOURCE)).is_none()
        );

        let edited_source = SOURCE.replace("greet", "notify");
        assert!(
            function_source_identity(&item, "api", &source_map(&edited_source)).is_none(),
            "a model span cannot validate against changed snapshot bytes"
        );
    }

    #[test]
    fn function_source_identity_recomputes_token_pairs_from_snapshot_bytes() {
        let mut item = function_item(SOURCE);
        item.declaration_token_pairs
            .as_mut()
            .expect("declaration tokens")[0]
            .1 = "altered".to_owned();
        assert!(function_source_identity(&item, "api", &source_map(SOURCE)).is_none());

        let original = function_item(SOURCE);
        let validated = function_source_identity(&original, "api", &source_map(SOURCE))
            .expect("source tokens are validated from the snapshot");
        assert!(matches!(validated, ValidatedFunctionSource { .. }));
    }

    #[test]
    fn resource_source_identity_requires_exact_resource_spans_and_snapshot_bytes() {
        let source = "interface api { resource store { read: func(); } }\n";
        let item = resource_item(source);
        let sources = source_map(source);
        assert!(resource_source_identity(&item, "api", &sources).is_some());

        let mut invalid_name = item.clone();
        invalid_name.span.as_mut().expect("resource span").range.end += 1;
        assert!(resource_source_identity(&invalid_name, "api", &sources).is_none());

        let mut invalid_path = item.clone();
        invalid_path
            .declaration_span
            .as_mut()
            .expect("resource declaration span")
            .path = "other.wit".to_owned();
        assert!(resource_source_identity(&invalid_path, "api", &sources).is_none());

        let mut invalid_tokens = item.clone();
        invalid_tokens
            .declaration_token_pairs
            .as_mut()
            .expect("resource declaration tokens")[0]
            .1 = "tampered".to_owned();
        assert!(resource_source_identity(&invalid_tokens, "api", &sources).is_none());

        let edited_source = source.replace("store", "vault");
        assert!(
            resource_source_identity(&item, "api", &source_map(&edited_source)).is_none(),
            "resource identity cannot validate against edited snapshot bytes"
        );
    }

    #[test]
    fn alias_source_identity_requires_exact_name_span_and_snapshot_bytes() {
        let source = "interface api { type count = u32; }\n";
        let item = alias_item(source, "count");
        let sources = source_map(source);
        assert!(alias_source_identity(&item, "api", &sources).is_some());

        let mut invalid_name = item.clone();
        invalid_name.span.as_mut().expect("alias span").range.end += 1;
        assert!(alias_source_identity(&invalid_name, "api", &sources).is_none());

        let mut invalid_interface = item.clone();
        assert!(
            alias_source_identity(&invalid_interface, "other", &sources).is_none(),
            "the scanner must identify the exact source interface"
        );
        invalid_interface.name = "changed".to_owned();
        assert!(alias_source_identity(&invalid_interface, "api", &sources).is_none());

        let edited_source = source.replace("count", "amount");
        assert!(
            alias_source_identity(&item, "api", &source_map(&edited_source)).is_none(),
            "the name span cannot validate against changed snapshot bytes"
        );

        let mut missing_span = item;
        missing_span.span = None;
        assert!(alias_source_identity(&missing_span, "api", &sources).is_none());
    }

    #[test]
    fn resource_member_sidecars_must_match_parsed_names_kinds_order_and_spans() {
        let source = "interface api { resource store { read: func(); } }\n";
        let item = resource_item(source);
        let sources = source_map(source);
        let source_resource =
            resource_source_identity(&item, "api", &sources).expect("resource validates");
        let WitItemKind::Resource(members) = &item.kind else {
            panic!("resource model expected");
        };
        assert!(resource_members_match_source(
            &item,
            members,
            &source_resource,
            &sources
        ));

        let mut wrong_name = members.clone();
        wrong_name[0].name = "write".to_owned();
        assert!(!resource_members_match_source(
            &item,
            &wrong_name,
            &source_resource,
            &sources
        ));

        let mut wrong_kind = members.clone();
        wrong_kind[0].kind = WitMemberKind::Static;
        assert!(!resource_members_match_source(
            &item,
            &wrong_kind,
            &source_resource,
            &sources
        ));

        let mut wrong_span = source_resource.clone();
        wrong_span.members[0].name_range.end += 1;
        assert!(!resource_members_match_source(
            &item,
            members,
            &wrong_span,
            &sources
        ));

        let mut extra_member = members.clone();
        extra_member.push(members[0].clone());
        assert!(!resource_members_match_source(
            &item,
            &extra_member,
            &source_resource,
            &sources
        ));
    }

    #[test]
    fn enum_and_case_fingerprints_survive_renaming_their_own_names() {
        fn fingerprints(source: &str, enum_name: &str) -> (Fingerprint, Fingerprint) {
            let name_start = source.find(enum_name).expect("enum name occurs");
            let enum_tokens = authored_interface_enum_tokens(
                source,
                "api",
                enum_name,
                &(name_start..name_start + enum_name.len()),
            )
            .expect("scanner binds to the exact enum name range");
            let enum_fingerprint = declaration_fingerprint_from_token_pairs(
                SymbolKind::Enum,
                &enum_tokens.token_pairs,
            );
            let case_tokens = &enum_tokens.cases[0];
            let case_fingerprint = declaration_fingerprint_from_token_pairs(
                SymbolKind::Variant,
                &case_tokens.token_pairs,
            );
            (enum_fingerprint, case_fingerprint)
        }

        let original = fingerprints(
            "package acme:sample@1.0.0; interface api { enum status { ready, stopped } }",
            "status",
        );
        let enum_renamed = fingerprints(
            "package acme:sample@1.0.0; interface api { enum lifecycle { ready, stopped } }",
            "lifecycle",
        );
        let case_renamed = fingerprints(
            "package acme:sample@1.0.0; interface api { enum status { prepared, stopped } }",
            "status",
        );

        assert_eq!(original.0, enum_renamed.0);
        assert_eq!(original.1, enum_renamed.1);
        assert_eq!(original.1, case_renamed.1);
        assert_ne!(
            original.0, case_renamed.0,
            "the enum fingerprint tracks changes to its ordered cases"
        );
    }

    #[test]
    fn alias_def_paths_and_hashes_reject_mismatched_view_identity() {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:sample#1.0.0")
            .expect("package interns");
        let symbols = Interner::new();
        let path = alias_path(&packages, package, &symbols);
        let canonical_path = "interface[3:api;0]/type[5:Count;0]/";
        assert!(view_definition_path_matches_alias(
            &path,
            canonical_path,
            "api",
            "Count",
            &symbols
        ));
        assert!(!view_definition_path_matches_alias(
            &path,
            "interface[3:api;0]/type[6:Wrong;0]/",
            "api",
            "Count",
            &symbols
        ));
        assert!(!view_definition_path_matches_alias(
            &path,
            canonical_path,
            "other",
            "Count",
            &symbols
        ));
        let wrong_kind = def_path(
            &packages,
            package,
            &symbols,
            vec![
                DisambiguatedDefPathData {
                    data: DefPathData::Interface(path_symbol(&symbols, "api")),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::ValueNs(path_symbol(&symbols, "Count")),
                    disambiguator: 0,
                },
            ],
        );
        assert!(!view_definition_path_matches_alias(
            &wrong_kind,
            canonical_path,
            "api",
            "Count",
            &symbols
        ));
        let wrong_disambiguator = def_path(
            &packages,
            package,
            &symbols,
            vec![
                DisambiguatedDefPathData {
                    data: DefPathData::Interface(path_symbol(&symbols, "api")),
                    disambiguator: 0,
                },
                DisambiguatedDefPathData {
                    data: DefPathData::TypeNs(path_symbol(&symbols, "Count")),
                    disambiguator: 1,
                },
            ],
        );
        assert!(!view_definition_path_matches_alias(
            &wrong_disambiguator,
            canonical_path,
            "api",
            "Count",
            &symbols
        ));

        let expected_hash =
            DefPathHash::compute(&path, &packages, &symbols).expect("path hash computes");
        assert_eq!(
            verified_view_path_hash(&path, expected_hash, &packages, &symbols),
            Some(Fingerprint::new(expected_hash.raw()))
        );
        let other_package = packages
            .intern("acme:other#1.0.0")
            .expect("other package interns");
        let foreign_path = alias_path(&packages, other_package, &symbols);
        let foreign_hash = DefPathHash::compute(&foreign_path, &packages, &symbols)
            .expect("foreign path hash computes");
        assert!(
            verified_view_path_hash(&foreign_path, foreign_hash, &packages, &symbols).is_some(),
            "a correctly recomputed foreign package hash is internally valid"
        );
        assert!(!alias_path_matches_selected_package(&foreign_path, package));
        assert!(alias_path_matches_selected_package(&path, package));
        assert_eq!(
            verified_view_path_hash(
                &path,
                DefPathHash::compute(&wrong_disambiguator, &packages, &symbols)
                    .expect("mismatched path hash computes"),
                &packages,
                &symbols
            ),
            None
        );
    }

    #[test]
    fn enum_def_paths_and_hashes_reject_mismatched_view_identity() {
        let packages = PackageInterner::new();
        let package = packages
            .intern("acme:sample#1.0.0")
            .expect("package interns");
        let symbols = Interner::new();
        let enum_path = enum_path(&packages, package, &symbols);
        let enum_canonical_path = "interface[3:api;0]/type[6:Status;0]/";
        assert!(view_definition_path_matches_enum(
            &enum_path,
            enum_canonical_path,
            "api",
            "Status",
            &symbols
        ));
        assert!(!view_definition_path_matches_enum(
            &enum_path,
            "interface[3:api;0]/type[5:Wrong;0]/",
            "api",
            "Status",
            &symbols
        ));
        assert!(!view_definition_path_matches_enum(
            &enum_path,
            enum_canonical_path,
            "other",
            "Status",
            &symbols
        ));

        let expected_hash =
            DefPathHash::compute(&enum_path, &packages, &symbols).expect("path hash computes");
        let interface = WitIndexVec::<WitIfaceIdx, ()>::new()
            .next_index()
            .expect("interface index fits");
        let item = WitIndexVec::<WitItemIdx, ()>::new()
            .next_index()
            .expect("item index fits");
        let enum_definition = WitViewDef {
            entity: WitViewEntity::Item { interface, item },
            kind: WitViewDefKind::Enum,
            path_hash: expected_hash,
            canonical_path: enum_canonical_path.to_owned(),
        };
        assert!(view_definition_matches_entity(
            &enum_definition,
            WitViewEntity::Item { interface, item },
            WitViewDefKind::Enum
        ));
        let mut wrong_entity = enum_definition.clone();
        wrong_entity.entity = WitViewEntity::Interface { interface };
        assert!(!view_definition_matches_entity(
            &wrong_entity,
            WitViewEntity::Item { interface, item },
            WitViewDefKind::Enum
        ));
        let mut wrong_kind = enum_definition.clone();
        wrong_kind.kind = WitViewDefKind::Variant;
        assert!(!view_definition_matches_entity(
            &wrong_kind,
            WitViewEntity::Item { interface, item },
            WitViewDefKind::Enum
        ));
        assert_eq!(
            verified_view_path_hash(&enum_path, expected_hash, &packages, &symbols),
            Some(Fingerprint::new(expected_hash.raw()))
        );
        let wrong_hash = DefPathHash::compute(
            &enum_case_path(&packages, package, &symbols),
            &packages,
            &symbols,
        )
        .expect("distinct path hash computes");
        assert_ne!(expected_hash, wrong_hash);
        assert_eq!(
            verified_view_path_hash(&enum_path, wrong_hash, &packages, &symbols),
            None,
            "a view hash for a different entity is rejected"
        );

        let case_path = enum_case_path(&packages, package, &symbols);
        let case_canonical_path = "interface[3:api;0]/type[6:Status;0]/variant[5:Ready;0]/";
        assert!(view_definition_path_matches_enum_case(
            &case_path,
            case_canonical_path,
            "api",
            "Status",
            "Ready",
            &symbols
        ));
        assert!(!view_definition_path_matches_enum_case(
            &case_path,
            case_canonical_path,
            "api",
            "Status",
            "NotReady",
            &symbols
        ));
    }
}
