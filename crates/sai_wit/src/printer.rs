//! Canonical text printer for generated WIT packages.

use crate::{
    WitFunc, WitIfaceIdx, WitIfaceRef, WitItem, WitItemKind, WitMember, WitMemberKind, WitPackage,
    WitStabilityVersion, WitType, WitTypeRef, WitWorld, WitWorldItem, WitWorldItemDirection,
    WitWorldItemOrder, WitWorldRef,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;

/// Print a generated WIT package in the canonical format of spec/13 §13.11.
///
/// The printer is intended for generated units. Their producer omits items
/// whose boundary types cannot be represented by the generated-WIT contract.
///
/// # Panics
///
/// Panics if a type is marked unsupported. Such a type has lost source details
/// needed to reconstruct its WIT spelling and cannot occur in a generated
/// package accepted by the generation rules.
pub fn print_wit(package: &WitPackage) -> String {
    let version = package
        .name
        .version
        .as_ref()
        .expect("generated WIT packages always have a selected version");
    let mut output = format!(
        "package {}:{}@{};\n",
        wit_identifier(&package.name.namespace),
        wit_identifier(&package.name.name),
        version,
    );

    let mut top_level = Vec::with_capacity(package.interfaces.len() + package.worlds.len());
    for (index, interface) in package.interfaces.iter_enumerated() {
        top_level.push(print_interface(package, index, interface));
    }
    for world in package.worlds.iter() {
        top_level.push(print_world(package, world));
    }

    if !top_level.is_empty() {
        output.push('\n');
        output.push_str(&top_level.join("\n\n"));
        output.push('\n');
    }
    output
}

fn print_interface(
    package: &WitPackage,
    interface_index: WitIfaceIdx,
    interface: &crate::WitInterface,
) -> String {
    let mut output = String::new();
    push_docs(&mut output, interface.docs.as_deref(), 0);
    push_stability(
        &mut output,
        interface.since.as_ref(),
        interface.deprecated.as_ref(),
        0,
    );

    let mut uses = BTreeMap::<String, BTreeSet<String>>::new();
    for item in interface.items.iter() {
        collect_item_uses(package, interface_index, item, &mut uses);
    }

    let printed_items = interface
        .items
        .iter()
        .filter_map(print_item)
        .collect::<Vec<_>>();
    if uses.is_empty() && printed_items.is_empty() {
        let _ = write!(output, "interface {} {{}}", wit_identifier(&interface.name));
        return output;
    }

    let _ = writeln!(output, "interface {} {{", wit_identifier(&interface.name));
    if !uses.is_empty() {
        for (path, members) in &uses {
            let _ = writeln!(
                output,
                "  use {}.{{{}}};",
                path,
                members
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
    }
    if !uses.is_empty() && !printed_items.is_empty() {
        output.push('\n');
    }
    for (index, item) in printed_items.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        push_indented(&mut output, item, 1);
        output.push('\n');
    }
    output.push('}');
    output
}

fn print_item(item: &WitItem) -> Option<String> {
    if matches!(item.kind, WitItemKind::Used(_)) {
        return None;
    }

    let mut output = String::new();
    push_docs(&mut output, item.docs.as_deref(), 0);
    push_stability(
        &mut output,
        item.since.as_ref(),
        item.deprecated.as_ref(),
        0,
    );
    let name = wit_identifier(&item.name);
    match &item.kind {
        WitItemKind::Record(fields) => {
            let _ = writeln!(output, "record {name} {{");
            for field in fields {
                push_docs(&mut output, field.docs.as_deref(), 1);
                let _ = writeln!(
                    output,
                    "  {}: {},",
                    wit_identifier(&field.name),
                    print_type(&field.ty)
                );
            }
            output.push('}');
        }
        WitItemKind::Variant(cases) => {
            let _ = writeln!(output, "variant {name} {{");
            for case in cases {
                push_docs(&mut output, case.docs.as_deref(), 1);
                let _ = write!(output, "  {}", wit_identifier(&case.name));
                if let Some(payload) = &case.payload {
                    let _ = write!(output, "({})", print_type(payload));
                }
                output.push_str(",\n");
            }
            output.push('}');
        }
        WitItemKind::Enum(cases) => {
            let _ = writeln!(output, "enum {name} {{");
            for case in cases {
                let _ = writeln!(output, "  {},", wit_identifier(case));
            }
            output.push('}');
        }
        WitItemKind::Flags(flags) => {
            let _ = writeln!(output, "flags {name} {{");
            for flag in flags {
                let _ = writeln!(output, "  {},", wit_identifier(flag));
            }
            output.push('}');
        }
        WitItemKind::Resource(members) if members.is_empty() => {
            let _ = write!(output, "resource {name};");
        }
        WitItemKind::Resource(members) => {
            let _ = writeln!(output, "resource {name} {{");
            for member in members {
                push_member(&mut output, member);
            }
            output.push('}');
        }
        WitItemKind::Alias(ty) => {
            let _ = write!(output, "type {name} = {};", print_type(ty));
        }
        WitItemKind::Used(_) => unreachable!("used items were omitted above"),
        WitItemKind::Func(function) => {
            let _ = write!(output, "{name}: {};", print_function(function, true));
        }
    }
    Some(output)
}

fn push_member(output: &mut String, member: &WitMember) {
    push_docs(output, member.docs.as_deref(), 1);
    push_stability(output, member.since.as_ref(), member.deprecated.as_ref(), 1);

    match member.kind {
        WitMemberKind::Constructor => {
            let params = print_params(&member.func, false);
            let _ = writeln!(output, "  constructor({params});");
        }
        WitMemberKind::Method => {
            let _ = writeln!(
                output,
                "  {}: {};",
                wit_identifier(&member.name),
                print_function(&member.func, true)
            );
        }
        WitMemberKind::Static => {
            let _ = writeln!(
                output,
                "  {}: static {};",
                wit_identifier(&member.name),
                print_function(&member.func, true)
            );
        }
    }
}

fn print_world(package: &WitPackage, world: &WitWorld) -> String {
    let mut output = String::new();
    push_docs(&mut output, world.docs.as_deref(), 0);
    push_stability(
        &mut output,
        world.since.as_ref(),
        world.deprecated.as_ref(),
        0,
    );
    let entries = world_entries(world);
    if entries.is_empty() {
        let _ = write!(output, "world {} {{}}", wit_identifier(&world.name));
        return output;
    }
    let _ = writeln!(output, "world {} {{", wit_identifier(&world.name));
    for entry in entries {
        match entry {
            WorldEntry::Import(item) => print_world_item(&mut output, package, "import", item),
            WorldEntry::Export(item) => print_world_item(&mut output, package, "export", item),
            WorldEntry::Include(reference) => {
                push_stability(
                    &mut output,
                    reference.since.as_ref(),
                    reference.deprecated.as_ref(),
                    1,
                );
                let path = print_world_path(package, reference);
                let _ = write!(output, "  include {path}");
                if !reference.names.is_empty() {
                    let names = reference
                        .names
                        .iter()
                        .map(|(name, alias)| {
                            format!("{} as {}", wit_identifier(name), wit_identifier(alias))
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    let _ = write!(output, " with {{{names}}}");
                    output.push('\n');
                } else {
                    output.push_str(";\n");
                }
            }
        }
    }
    output.push('}');
    output
}

enum WorldEntry<'a> {
    Import(&'a WitWorldItem),
    Export(&'a WitWorldItem),
    Include(&'a WitWorldRef),
}

fn world_entries(world: &WitWorld) -> Vec<WorldEntry<'_>> {
    let order = if world.source_order.is_empty() {
        let mut fallback = (0..world.imports.len())
            .filter_map(|index| u32::try_from(index).ok())
            .map(WitWorldItemOrder::import)
            .collect::<Vec<_>>();
        fallback.extend(
            (0..world.exports.len())
                .filter_map(|index| u32::try_from(index).ok())
                .map(WitWorldItemOrder::export),
        );
        fallback.extend(
            (0..world.includes.len())
                .filter_map(|index| u32::try_from(index).ok())
                .map(WitWorldItemOrder::include),
        );
        fallback
    } else {
        world.source_order.clone()
    };

    order
        .iter()
        .filter_map(|entry| {
            let index = usize::try_from(entry.index).ok()?;
            match entry.direction {
                WitWorldItemDirection::Import => {
                    Some(WorldEntry::Import(world.imports.get(index)?))
                }
                WitWorldItemDirection::Export => {
                    Some(WorldEntry::Export(world.exports.get(index)?))
                }
                WitWorldItemDirection::Include => {
                    Some(WorldEntry::Include(world.includes.get(index)?))
                }
            }
        })
        .collect()
}

fn print_world_item(
    output: &mut String,
    package: &WitPackage,
    direction: &str,
    item: &WitWorldItem,
) {
    match item {
        WitWorldItem::Interface {
            reference,
            since,
            deprecated,
            ..
        } => {
            push_stability(output, since.as_ref(), deprecated.as_ref(), 1);
            let path = print_interface_path(package, reference);
            let _ = writeln!(output, "  {direction} {path};");
        }
        WitWorldItem::Func {
            name,
            func,
            since,
            deprecated,
            ..
        } => {
            push_stability(output, since.as_ref(), deprecated.as_ref(), 1);
            let _ = writeln!(
                output,
                "  {direction} {}: {};",
                wit_identifier(name),
                print_function(func, true)
            );
        }
    }
}

fn print_function(function: &WitFunc, omit_self: bool) -> String {
    let mut output = String::new();
    if function.is_async {
        output.push_str("async ");
    }
    let _ = write!(output, "func({})", print_params(function, omit_self));
    if let Some(result) = &function.result {
        let _ = write!(output, " -> {}", print_type(result));
    }
    output
}

fn print_params(function: &WitFunc, omit_self: bool) -> String {
    function
        .params
        .iter()
        .filter(|(name, _)| !(omit_self && name.trim_start_matches("r#") == "self"))
        .map(|(name, ty)| format!("{}: {}", wit_identifier(name), print_type(ty)))
        .collect::<Vec<_>>()
        .join(", ")
}

fn print_type(ty: &WitType) -> String {
    match ty {
        WitType::Bool => "bool".to_owned(),
        WitType::S8 => "s8".to_owned(),
        WitType::S16 => "s16".to_owned(),
        WitType::S32 => "s32".to_owned(),
        WitType::S64 => "s64".to_owned(),
        WitType::U8 => "u8".to_owned(),
        WitType::U16 => "u16".to_owned(),
        WitType::U32 => "u32".to_owned(),
        WitType::U64 => "u64".to_owned(),
        WitType::F32 => "f32".to_owned(),
        WitType::F64 => "f64".to_owned(),
        WitType::Char => "char".to_owned(),
        WitType::String => "string".to_owned(),
        WitType::List(inner) => format!("list<{}>", print_type(inner)),
        WitType::Option(inner) => format!("option<{}>", print_type(inner)),
        WitType::Result { ok, err } => match (ok, err) {
            (Some(ok), Some(err)) => {
                format!("result<{}, {}>", print_type(ok), print_type(err))
            }
            (None, Some(err)) => format!("result<_, {}>", print_type(err)),
            (Some(ok), None) => format!("result<{}>", print_type(ok)),
            (None, None) => "result".to_owned(),
        },
        WitType::Tuple(types) => format!(
            "tuple<{}>",
            types.iter().map(print_type).collect::<Vec<_>>().join(", ")
        ),
        WitType::Named(reference) => wit_identifier(&reference.item_name),
        WitType::Own(reference) => wit_identifier(&reference.item_name),
        WitType::Borrow(reference) => {
            format!("borrow<{}>", wit_identifier(&reference.item_name))
        }
        WitType::Future(inner) => format!(
            "future{}",
            inner
                .as_deref()
                .map(|inner| format!("<{}>", print_type(inner)))
                .unwrap_or_default()
        ),
        WitType::Stream(inner) => format!(
            "stream{}",
            inner
                .as_deref()
                .map(|inner| format!("<{}>", print_type(inner)))
                .unwrap_or_default()
        ),
        WitType::Unsupported(unsupported) => {
            panic!("cannot canonically print unsupported WIT type {unsupported:?}")
        }
    }
}

fn collect_item_uses(
    package: &WitPackage,
    current_interface: WitIfaceIdx,
    item: &WitItem,
    uses: &mut BTreeMap<String, BTreeSet<String>>,
) {
    match &item.kind {
        WitItemKind::Record(fields) => {
            for field in fields {
                collect_type_uses(package, current_interface, &field.ty, uses);
            }
        }
        WitItemKind::Variant(cases) => {
            for case in cases {
                if let Some(ty) = &case.payload {
                    collect_type_uses(package, current_interface, ty, uses);
                }
            }
        }
        WitItemKind::Resource(members) => {
            for member in members {
                collect_func_uses(package, current_interface, &member.func, uses);
            }
        }
        WitItemKind::Alias(ty) => collect_type_uses(package, current_interface, ty, uses),
        WitItemKind::Used(reference) => {
            add_use(
                package,
                current_interface,
                reference,
                Some(&item.name),
                uses,
            );
        }
        WitItemKind::Func(function) => {
            collect_func_uses(package, current_interface, function, uses)
        }
        WitItemKind::Enum(_) | WitItemKind::Flags(_) => {}
    }
}

fn collect_func_uses(
    package: &WitPackage,
    current_interface: WitIfaceIdx,
    function: &WitFunc,
    uses: &mut BTreeMap<String, BTreeSet<String>>,
) {
    for (_, ty) in &function.params {
        collect_type_uses(package, current_interface, ty, uses);
    }
    if let Some(result) = &function.result {
        collect_type_uses(package, current_interface, result, uses);
    }
}

fn collect_type_uses(
    package: &WitPackage,
    current_interface: WitIfaceIdx,
    ty: &WitType,
    uses: &mut BTreeMap<String, BTreeSet<String>>,
) {
    match ty {
        WitType::List(inner)
        | WitType::Option(inner)
        | WitType::Future(Some(inner))
        | WitType::Stream(Some(inner)) => {
            collect_type_uses(package, current_interface, inner, uses)
        }
        WitType::Result { ok, err } => {
            if let Some(ok) = ok {
                collect_type_uses(package, current_interface, ok, uses);
            }
            if let Some(err) = err {
                collect_type_uses(package, current_interface, err, uses);
            }
        }
        WitType::Tuple(types) => {
            for ty in types {
                collect_type_uses(package, current_interface, ty, uses);
            }
        }
        WitType::Named(reference) | WitType::Own(reference) | WitType::Borrow(reference) => {
            add_use(package, current_interface, reference, None, uses);
        }
        WitType::Future(None)
        | WitType::Stream(None)
        | WitType::Bool
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
        | WitType::Unsupported(_) => {}
    }
}

fn add_use(
    package: &WitPackage,
    current_interface: WitIfaceIdx,
    reference: &WitTypeRef,
    alias: Option<&str>,
    uses: &mut BTreeMap<String, BTreeSet<String>>,
) {
    if reference.interface.unit == package.unit
        && reference.interface.interface == current_interface
    {
        return;
    }
    let path = print_interface_path(package, &reference.interface);
    let item_name = wit_identifier(&reference.item_name);
    let rendered = match alias {
        Some(alias) if alias != reference.item_name => {
            format!("{item_name} as {}", wit_identifier(alias))
        }
        _ => item_name,
    };
    // Sort the spelling that appears in the output. Escaping a keyword adds
    // `%`, which can change its bytewise position relative to other names.
    uses.entry(path).or_default().insert(rendered);
}

fn print_interface_path(package: &WitPackage, reference: &WitIfaceRef) -> String {
    print_unit_path(package, &reference.unit, &reference.name)
}

fn print_world_path(package: &WitPackage, reference: &WitWorldRef) -> String {
    print_unit_path(package, &reference.unit, &reference.name)
}

fn print_unit_path(package: &WitPackage, unit: &crate::WitUnit, name: &str) -> String {
    let name = wit_identifier(name);
    if unit == &package.unit {
        return name;
    }

    let canonical_name = unit.canonical_name();
    match canonical_name.rsplit_once('@') {
        Some((identity, version)) => {
            format!("{}/{name}@{version}", wit_package_identity(identity))
        }
        None => format!("{canonical_name}/{name}"),
    }
}

fn wit_package_identity(identity: &str) -> String {
    let (namespace, name) = identity
        .split_once(':')
        .expect("WIT package identities contain a namespace and name");
    format!("{}:{}", wit_identifier(namespace), wit_identifier(name))
}

fn push_docs(output: &mut String, docs: Option<&str>, level: usize) {
    let Some(docs) = docs else {
        return;
    };
    let normalized = docs.replace("\r\n", "\n").replace('\r', "\n");
    for line in normalized.split('\n') {
        let line = line.trim_end_matches([' ', '\t', '\r']);
        push_indent(output, level);
        if line.is_empty() {
            output.push_str("///\n");
        } else {
            let _ = writeln!(output, "/// {}", line.replace('\t', "  "));
        }
    }
}

fn push_stability(
    output: &mut String,
    since: Option<&WitStabilityVersion>,
    deprecated: Option<&WitStabilityVersion>,
    level: usize,
) {
    if let Some(version) = since {
        push_indent(output, level);
        let _ = writeln!(output, "@since(version = {version})");
    }
    if let Some(version) = deprecated {
        let since = since.expect("a deprecated WIT item must retain its source @since version");
        assert!(
            since.cmp_precedence(version) != std::cmp::Ordering::Greater,
            "a WIT item cannot be deprecated before its @since version"
        );
        push_indent(output, level);
        let _ = writeln!(output, "@deprecated(version = {version})");
    }
}

fn push_indented(output: &mut String, text: &str, level: usize) {
    for (index, line) in text.lines().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        push_indent(output, level);
        output.push_str(line);
    }
}

fn push_indent(output: &mut String, level: usize) {
    for _ in 0..level * 2 {
        output.push(' ');
    }
}

/// Render one WIT identifier, including the `%` escape for reserved words.
///
/// This is shared with diagnostics that must use the same bytewise ordering
/// as the canonical printer.
pub fn wit_identifier(name: &str) -> String {
    if crate::source_scan::is_wit_keyword(name) {
        format!("%{name}")
    } else {
        name.to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::print_wit;
    use crate::{
        WitCase, WitField, WitFunc, WitIfaceIdx, WitIfaceRef, WitIndexVec, WitInterface, WitItem,
        WitItemIdx, WitItemKind, WitMember, WitMemberKind, WitPackage, WitPackageName,
        WitStabilityVersion, WitType, WitTypeRef, WitUnit, WitWorld, WitWorldItem,
        WitWorldItemOrder, WitWorldRef,
    };
    use sai_intern::{Interner, PackageInterner};
    use sai_package::{mvs::MvsPackageVersion, version::Version};
    use std::sync::Arc;

    fn wasi_unit(package: &str) -> WitUnit {
        let symbols = Interner::new();
        let package = symbols.intern(package).expect("valid WIT package symbol");
        WitUnit::wasi(
            package,
            &symbols,
            &Version::parse("0.2.3").expect("valid version"),
        )
        .expect("embedded WASI package")
    }

    fn dir_unit(identity: &str, version: &str) -> WitUnit {
        let selected_version = Version::parse(version).expect("valid version");
        let selected =
            MvsPackageVersion::new(identity, selected_version).expect("valid selected package");
        let packages = PackageInterner::new();
        let key = format!("{}#{}", identity, selected.version().compatibility_line());
        let package = packages.intern(&key).expect("valid package key");
        WitUnit::dir(package, &packages, &selected).expect("matching unit")
    }

    fn package(unit: WitUnit, interfaces: Vec<WitInterface>, worlds: Vec<WitWorld>) -> WitPackage {
        let canonical_name = unit.canonical_name().to_owned();
        let identity = canonical_name
            .rsplit_once('@')
            .map(|(identity, _)| identity.to_owned())
            .expect("unit has a selected version");
        let (namespace, name) = identity
            .split_once(':')
            .map(|(namespace, name)| (namespace.to_owned(), name.to_owned()))
            .expect("package identity");
        let version = canonical_name
            .rsplit_once('@')
            .map(|(_, version)| Version::parse(version).expect("unit version parses"));
        WitPackage {
            unit,
            name: WitPackageName {
                namespace,
                name,
                version,
            },
            interfaces: WitIndexVec::try_from_vec(interfaces).expect("small interface list"),
            worlds: WitIndexVec::try_from_vec(worlds).expect("small world list"),
            declaration_order: Vec::new(),
        }
    }

    fn interface(name: &str, items: Vec<WitItem>) -> WitInterface {
        WitInterface {
            name: name.to_owned(),
            docs: None,
            since: None,
            deprecated: None,
            items: WitIndexVec::try_from_vec(items).expect("small item list"),
            span: None,
        }
    }

    fn item(name: &str, kind: WitItemKind) -> WitItem {
        WitItem {
            name: name.to_owned(),
            docs: None,
            since: None,
            deprecated: None,
            kind,
            unsupported: None,
            span: None,
        }
    }

    fn function(params: Vec<(String, WitType)>, result: Option<WitType>) -> WitFunc {
        WitFunc {
            params,
            result,
            is_async: false,
        }
    }

    #[test]
    fn prints_types_comments_deprecation_handles_and_world_source_order() {
        let unit = wasi_unit("io");
        let included_unit = dir_unit("acme:base", "1.0.0");
        let world = WitWorld {
            name: "api".to_owned(),
            docs: Some(Arc::from("The package world.")),
            imports: vec![
                WitWorldItem::Func {
                    name: "first".to_owned(),
                    func: function(Vec::new(), None),
                    unsupported: None,
                    since: None,
                    deprecated: None,
                    span: None,
                },
                WitWorldItem::Func {
                    name: "second".to_owned(),
                    func: function(Vec::new(), None),
                    unsupported: None,
                    since: None,
                    deprecated: None,
                    span: None,
                },
            ],
            exports: vec![WitWorldItem::Func {
                name: "middle".to_owned(),
                func: function(Vec::new(), None),
                unsupported: None,
                since: None,
                deprecated: None,
                span: None,
            }],
            includes: vec![WitWorldRef {
                unit: included_unit,
                world: crate::WitWorldIdx::from_raw(0),
                name: "base".to_owned(),
                names: Vec::new(),
                since: None,
                deprecated: None,
                span: None,
            }],
            source_order: vec![
                WitWorldItemOrder::import(0),
                WitWorldItemOrder::export(0),
                WitWorldItemOrder::include(0),
                WitWorldItemOrder::import(1),
            ],
            since: None,
            deprecated: None,
            unsupported: None,
            span: None,
        };
        let resource = WitItem {
            name: "file".to_owned(),
            docs: None,
            since: None,
            deprecated: None,
            kind: WitItemKind::Resource(vec![
                WitMember {
                    name: "constructor".to_owned(),
                    kind: WitMemberKind::Constructor,
                    func: function(
                        vec![("contents".to_owned(), WitType::List(Box::new(WitType::U8)))],
                        None,
                    ),
                    docs: None,
                    since: None,
                    deprecated: None,
                    unsupported: None,
                },
                WitMember {
                    name: "close".to_owned(),
                    kind: WitMemberKind::Method,
                    func: function(
                        vec![
                            (
                                "self".to_owned(),
                                WitType::Borrow(WitTypeRef {
                                    interface: WitIfaceRef {
                                        unit: unit.clone(),
                                        interface: WitIfaceIdx::from_raw(0),
                                        name: "files".to_owned(),
                                    },
                                    item: WitItemIdx::from_raw(0),
                                    item_name: "file".to_owned(),
                                }),
                            ),
                            ("force".to_owned(), WitType::Bool),
                        ],
                        Some(WitType::Result {
                            ok: None,
                            err: Some(Box::new(WitType::Named(WitTypeRef {
                                interface: WitIfaceRef {
                                    unit: unit.clone(),
                                    interface: WitIfaceIdx::from_raw(0),
                                    name: "files".to_owned(),
                                },
                                item: WitItemIdx::from_raw(1),
                                item_name: "image-error".to_owned(),
                            }))),
                        }),
                    ),
                    docs: None,
                    since: None,
                    deprecated: None,
                    unsupported: None,
                },
                WitMember {
                    name: "new-file".to_owned(),
                    kind: WitMemberKind::Static,
                    func: function(Vec::new(), None),
                    docs: None,
                    since: None,
                    deprecated: None,
                    unsupported: None,
                },
            ]),
            unsupported: None,
            span: None,
        };
        let items = vec![
            WitItem {
                docs: Some(Arc::from("A file.\nWith two lines.")),
                ..item(
                    "file",
                    WitItemKind::Resource(match resource.kind {
                        WitItemKind::Resource(members) => members,
                        _ => unreachable!(),
                    }),
                )
            },
            WitItem {
                name: "image-error".to_owned(),
                kind: WitItemKind::Enum(vec!["not-found".to_owned(), "too-large".to_owned()]),
                ..item("placeholder", WitItemKind::Enum(Vec::new()))
            },
            WitItem {
                name: "type".to_owned(),
                docs: None,
                since: Some(WitStabilityVersion::parse("0.1.0").expect("valid version")),
                deprecated: Some(WitStabilityVersion::parse("1.4.0").expect("valid version")),
                kind: WitItemKind::Alias(WitType::Result {
                    ok: Some(Box::new(WitType::Tuple(vec![
                        WitType::U32,
                        WitType::String,
                    ]))),
                    err: None,
                }),
                unsupported: None,
                span: None,
            },
            item("empty-resource", WitItemKind::Resource(Vec::new())),
            item(
                "point",
                WitItemKind::Record(vec![
                    WitField {
                        name: "x".to_owned(),
                        docs: None,
                        ty: WitType::S32,
                    },
                    WitField {
                        name: "y".to_owned(),
                        docs: None,
                        ty: WitType::S32,
                    },
                ]),
            ),
            item(
                "state",
                WitItemKind::Variant(vec![
                    WitCase {
                        name: "ready".to_owned(),
                        docs: None,
                        payload: None,
                    },
                    WitCase {
                        name: "failed".to_owned(),
                        docs: None,
                        payload: Some(WitType::String),
                    },
                ]),
            ),
            item("permissions", WitItemKind::Flags(vec!["read".to_owned()])),
            item(
                "run",
                WitItemKind::Func(function(
                    vec![("as".to_owned(), WitType::Option(Box::new(WitType::Bool)))],
                    Some(WitType::Result {
                        ok: Some(Box::new(WitType::U32)),
                        err: Some(Box::new(WitType::String)),
                    }),
                )),
            ),
        ];
        // Ensure the writer follows the model order rather than sorting items.
        let mut iface = interface("files", items);
        iface.docs = Some(Arc::from("Files API.\r\nWith\ttabs.  "));
        iface.since = Some(WitStabilityVersion::parse("0.1.0+api.7").expect("valid version"));
        iface.deprecated = Some(WitStabilityVersion::parse("1.3.0+old.2").expect("valid version"));
        let package = package(unit, vec![iface], vec![world]);

        let text = print_wit(&package);
        assert_eq!(
            text,
            concat!(
                "package wasi:io@0.2.3;\n\n",
                "/// Files API.\n",
                "/// With  tabs.\n",
                "@since(version = 0.1.0+api.7)\n",
                "@deprecated(version = 1.3.0+old.2)\n",
                "interface files {\n",
                "  /// A file.\n",
                "  /// With two lines.\n",
                "  resource file {\n",
                "    constructor(contents: list<u8>);\n",
                "    close: func(force: bool) -> result<_, image-error>;\n",
                "    new-file: static func();\n",
                "  }\n\n",
                "  enum image-error {\n",
                "    not-found,\n",
                "    too-large,\n",
                "  }\n\n",
                "  @since(version = 0.1.0)\n",
                "  @deprecated(version = 1.4.0)\n",
                "  type %type = result<tuple<u32, string>>;\n\n",
                "  resource empty-resource;\n\n",
                "  record point {\n",
                "    x: s32,\n",
                "    y: s32,\n",
                "  }\n\n",
                "  variant state {\n",
                "    ready,\n",
                "    failed(string),\n",
                "  }\n\n",
                "  flags permissions {\n",
                "    read,\n",
                "  }\n\n",
                "  run: func(%as: option<bool>) -> result<u32, string>;\n",
                "}\n\n",
                "/// The package world.\n",
                "world api {\n",
                "  import first: func();\n",
                "  export middle: func();\n",
                "  include acme:base/base@1.0.0;\n",
                "  import second: func();\n",
                "}\n"
            )
        );
        crate::parse_source_files(
            package.unit.clone(),
            [crate::WitSourceFile::new("printed.wit", text)],
        )
        .expect("canonical printer output is syntactically valid WIT");
    }

    #[test]
    fn groups_cross_interface_uses_by_path_and_member_name() {
        let local = dir_unit("acme:root", "1.0.0");
        let first = dir_unit("acme:zdep", "2.3.4");
        let second = wasi_unit("io");
        let external_ref =
            |unit: WitUnit, interface: u32, interface_name: &str, item: u32, item_name: &str| {
                WitType::Named(WitTypeRef {
                    interface: WitIfaceRef {
                        unit,
                        interface: WitIfaceIdx::from_raw(interface),
                        name: interface_name.to_owned(),
                    },
                    item: WitItemIdx::from_raw(item),
                    item_name: item_name.to_owned(),
                })
            };
        let root = package(
            local,
            vec![interface(
                "client",
                vec![item(
                    "consume",
                    WitItemKind::Func(function(
                        vec![
                            ("z".to_owned(), external_ref(first, 1, "types", 0, "record")),
                            (
                                "a".to_owned(),
                                external_ref(second.clone(), 0, "error", 2, "stream-error"),
                            ),
                            (
                                "b".to_owned(),
                                external_ref(second.clone(), 0, "error", 1, "closed"),
                            ),
                            (
                                "c".to_owned(),
                                external_ref(second.clone(), 0, "error", 3, "record"),
                            ),
                            ("d".to_owned(), external_ref(second, 0, "error", 4, "with")),
                        ],
                        None,
                    )),
                )],
            )],
            vec![],
        );

        assert_eq!(
            print_wit(&root),
            concat!(
                "package acme:root@1.0.0;\n\n",
                "interface client {\n",
                "  use acme:zdep/types@2.3.4.{%record};\n",
                "  use wasi:io/error@0.2.3.{%record, %with, closed, stream-error};\n\n",
                "  consume: func(z: %record, a: stream-error, b: closed, c: %record, d: %with);\n",
                "}\n"
            )
        );
    }

    #[test]
    fn escapes_reserved_package_identity_segments() {
        let root = dir_unit("acme:root", "1.0.0");
        let dependency = dir_unit("acme:interface", "2.0.0");
        let model = package(
            root.clone(),
            Vec::new(),
            vec![WitWorld {
                name: "api".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                imports: Vec::new(),
                exports: Vec::new(),
                includes: vec![WitWorldRef {
                    unit: dependency,
                    world: crate::WitWorldIdx::from_raw(0),
                    name: "base".to_owned(),
                    names: Vec::new(),
                    since: None,
                    deprecated: None,
                    span: None,
                }],
                source_order: vec![WitWorldItemOrder::include(0)],
                unsupported: None,
                span: None,
            }],
        );
        let text = print_wit(&model);
        assert!(text.starts_with("package acme:root@1.0.0;\n"));
        assert!(text.contains("include acme:%interface/base@2.0.0;"));
        crate::parse_source_files(root, [crate::WitSourceFile::new("printed.wit", text)])
            .expect("escaped external identity parses");

        let reserved_root = dir_unit("acme:interface", "1.0.0");
        let text = print_wit(&package(reserved_root.clone(), Vec::new(), Vec::new()));
        assert!(text.starts_with("package acme:%interface@1.0.0;\n"));
        crate::parse_source_files(
            reserved_root,
            [crate::WitSourceFile::new("printed.wit", text)],
        )
        .expect("escaped package declaration parses");
    }
}
