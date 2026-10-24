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
    WitContract, WitItem, WitItemKind, WitPackage, WitPackageKind, WitType, WitUnit, WitUnitKind,
    WitView, WitViewDefKind, WitViewEntity, authored_interface_function_tokens, wit_name_to_sai,
};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::str;
use std::sync::Arc;

/// Adds direct authored interface functions to the module inventory.
///
/// This deliberately does not inspect dependency models in the WIT closure.
/// A WIT view mismatch or any unsupported root declaration leaves the result
/// incomplete, even when other declarations can still be inventoried.
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
    collect_root_interface_functions(
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

fn collect_root_interface_functions(
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

        for (item_index, item) in interface.items.iter_enumerated() {
            if matches!(item.kind, WitItemKind::Func(_)) {
                if !model_functions_seen.contains(&item_index) {
                    reasons.insert(format!(
                        "WIT function `{}`.{} is absent from the selected view",
                        interface.name, item.name
                    ));
                }
            } else {
                reasons.insert(format!(
                    "WIT interface item `{}`.{} is outside this inventory slice",
                    interface.name, item.name
                ));
            }
        }
    }
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
    use super::{ValidatedFunctionSource, function_source_identity};
    use sai_wit::{WitFunc, WitItem, WitItemKind, WitSpan, authored_interface_function_tokens};
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

    fn source_map(source: &str) -> BTreeMap<String, Arc<str>> {
        BTreeMap::from([("root.wit".to_owned(), Arc::<str>::from(source))])
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
}
