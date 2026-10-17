//! Stable structural hashing for session-local WIT views.
//!
//! The view stores runtime `PackageId`, `Symbol`, and `DefId` values for
//! compiler lookups. Hashes instead use the unit key and canonical definition
//! paths. The append-only definition index and its tombstones are deliberately
//! excluded so a view's fingerprint does not depend on rebuild history.

use crate::{
    WitFlagsMethod, WitView, WitViewDef, WitViewDefKind, WitViewEntity, WitViewOriginIdentity,
};
use sai_hir::{DefId, DefPathHash};
use sai_stable_hash::{StableHash, StableHasher};

fn hash_path_hash(path_hash: DefPathHash, hasher: &mut StableHasher) {
    path_hash.raw().stable_hash(hasher);
}

fn hash_definition_reference(view: &WitView, id: DefId, hasher: &mut StableHasher) {
    match view.definition(id) {
        Some(definition) => {
            hasher.write_tag(0x01);
            hash_path_hash(definition.path_hash, hasher);
            definition.canonical_path.stable_hash(hasher);
        }
        None => {
            // Invalid or stale runtime references have no stable path. Their
            // owning view still compares them by value after fingerprinting.
            hasher.write_tag(0xFF);
        }
    }
}

impl StableHash for WitView {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE0);
        self.unit.stable_hash(hasher);
        self.complete.stable_hash(hasher);
        self.model.stable_hash(hasher);

        let mut definitions = self
            .defs
            .iter_enumerated()
            .filter_map(|(_, definition)| definition.as_ref())
            .collect::<Vec<_>>();
        definitions.sort_by(|left, right| {
            left.path_hash
                .cmp(&right.path_hash)
                .then_with(|| left.canonical_path.cmp(&right.canonical_path))
        });
        hasher.write_tag(0x01);
        hasher.write_len(u64::try_from(definitions.len()).unwrap_or(u64::MAX));
        for definition in definitions {
            definition.stable_hash(hasher);
        }

        hasher.write_tag(0x02);
        hasher.write_len(u64::try_from(self.interfaces.len()).unwrap_or(u64::MAX));
        for (name, interface) in &self.interfaces {
            name.stable_hash(hasher);
            hash_definition_reference(self, interface.def, hasher);
            interface.wit.stable_hash(hasher);

            // `sai_name` is a session-local Symbol. Its canonical spelling is
            // already represented by the interface DefPath and the WIT name.
            hasher.write_len(u64::try_from(interface.members.len()).unwrap_or(u64::MAX));
            for (member_name, member) in &interface.members {
                member_name.stable_hash(hasher);
                hash_definition_reference(self, member.def, hasher);
                member.kind.stable_hash(hasher);
                member.item.stable_hash(hasher);
                member.unsupported.stable_hash(hasher);
            }

            hasher.write_len(u64::try_from(interface.shadowed.len()).unwrap_or(u64::MAX));
            for shadowed in &interface.shadowed {
                shadowed.wit_name.stable_hash(hasher);
                shadowed.resource.stable_hash(hasher);
                hash_definition_reference(self, shadowed.winner, hasher);
            }
            interface.origin_identity.stable_hash(hasher);
        }

        hasher.write_tag(0x03);
        hasher.write_len(u64::try_from(self.worlds.len()).unwrap_or(u64::MAX));
        for (name, world) in &self.worlds {
            name.stable_hash(hasher);
            hash_definition_reference(self, world.def, hasher);
            world.wit.stable_hash(hasher);

            // As with interfaces, the SAI symbol is recoverable from the
            // world path and canonical key and must not hash by runtime ID.
            hasher.write_len(u64::try_from(world.import_fns.len()).unwrap_or(u64::MAX));
            for (function_name, definition) in &world.import_fns {
                function_name.stable_hash(hasher);
                hash_definition_reference(self, *definition, hasher);
            }

            hasher.write_len(u64::try_from(world.export_fns.len()).unwrap_or(u64::MAX));
            for function_name in &world.export_fns {
                function_name.stable_hash(hasher);
            }
        }
    }
}

impl StableHash for WitViewDef {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE1);
        self.entity.stable_hash(hasher);
        self.kind.stable_hash(hasher);
        hash_path_hash(self.path_hash, hasher);
        self.canonical_path.stable_hash(hasher);
    }
}

impl StableHash for WitViewDefKind {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE2);
        hasher.write_tag(match self {
            Self::Interface => 0x01,
            Self::World => 0x02,
            Self::Struct => 0x03,
            Self::Enum => 0x04,
            Self::Resource => 0x05,
            Self::Alias => 0x06,
            Self::Fn => 0x07,
            Self::Field => 0x08,
            Self::Variant => 0x09,
            Self::AssocFn => 0x0A,
            Self::WorldFn => 0x0B,
        });
    }
}

impl StableHash for WitViewEntity {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE3);
        match self {
            Self::Interface { interface } => {
                hasher.write_tag(0x01);
                interface.stable_hash(hasher);
            }
            Self::World { world } => {
                hasher.write_tag(0x02);
                world.stable_hash(hasher);
            }
            Self::Item { interface, item } => {
                hasher.write_tag(0x03);
                interface.stable_hash(hasher);
                item.stable_hash(hasher);
            }
            Self::Field {
                interface,
                item,
                field,
            } => {
                hasher.write_tag(0x04);
                interface.stable_hash(hasher);
                item.stable_hash(hasher);
                field.stable_hash(hasher);
            }
            Self::Variant {
                interface,
                item,
                case,
            } => {
                hasher.write_tag(0x05);
                interface.stable_hash(hasher);
                item.stable_hash(hasher);
                case.stable_hash(hasher);
            }
            Self::ResourceMember {
                interface,
                item,
                member,
            } => {
                hasher.write_tag(0x06);
                interface.stable_hash(hasher);
                item.stable_hash(hasher);
                member.stable_hash(hasher);
            }
            Self::FlagsMethod {
                interface,
                item,
                method,
            } => {
                hasher.write_tag(0x07);
                interface.stable_hash(hasher);
                item.stable_hash(hasher);
                method.stable_hash(hasher);
            }
            Self::WorldFunction {
                world,
                import,
                item,
            } => {
                hasher.write_tag(0x08);
                world.stable_hash(hasher);
                import.stable_hash(hasher);
                item.stable_hash(hasher);
            }
        }
    }
}

impl StableHash for WitFlagsMethod {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE4);
        hasher.write_tag(match self {
            Self::Empty => 0x01,
            Self::All => 0x02,
            Self::Contains => 0x03,
            Self::IsEmpty => 0x04,
        });
    }
}

impl StableHash for WitViewOriginIdentity {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xE5);
        hash_path_hash(self.path_hash, hasher);
        self.canonical_path.stable_hash(hasher);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{WitSourceFile, WitUnit, parse_source_files};
    use sai_hir::DefPathInterner;
    use sai_intern::{Interner, PackageInterner};
    use sai_package::mvs::MvsPackageVersion;
    use sai_package::version::Version;
    use sai_stable_hash::fingerprint;
    use std::sync::Arc;
    use wit_parser::Resolve;

    fn parsed_model(
        packages: &PackageInterner,
        symbols: &Interner,
        source: &str,
    ) -> (WitUnit, crate::WitPackage) {
        let package = packages.intern("acme:api#1").expect("package key interns");
        let version = MvsPackageVersion::new("acme:api", Version::parse("1.0.0").unwrap()).unwrap();
        let unit = WitUnit::dir(package, packages, &version).unwrap();
        let parsed = parse_source_files(
            unit.clone(),
            [WitSourceFile::new("api.wit", source.as_bytes().to_vec())],
        )
        .expect("test WIT parses");
        let mut resolve = Resolve::default();
        let model = crate::WitResolveContext::default()
            .push_package(&mut resolve, parsed)
            .expect("test WIT converts to the owned model");
        let _ = symbols;
        (unit, model)
    }

    fn make_view(
        packages: &PackageInterner,
        symbols: &Interner,
        unit: WitUnit,
        model: crate::WitPackage,
        paths: Option<Arc<DefPathInterner>>,
    ) -> WitView {
        let view_package = packages.intern(unit.stable_key()).unwrap();
        let paths = paths.unwrap_or_else(|| {
            Arc::new(DefPathInterner::new(view_package, packages, symbols).unwrap())
        });
        WitView::build(model, packages, symbols, paths).unwrap()
    }

    const API_SOURCE: &str = "\
package acme:api@1.0.0;
interface api {
    record point { x: u32, y: u32 }
    draw: func(value: point) -> string;
}
world app {
    export start: func();
}
";

    #[test]
    fn stable_hash_ignores_session_local_package_symbol_and_definition_ids() {
        let first_packages = PackageInterner::new();
        let first_symbols = Interner::new();
        first_packages.intern("padding#1").unwrap();
        first_symbols.intern("padding").unwrap();
        let (first_unit, first_model) = parsed_model(&first_packages, &first_symbols, API_SOURCE);
        let first = make_view(
            &first_packages,
            &first_symbols,
            first_unit,
            first_model,
            None,
        );

        let second_packages = PackageInterner::new();
        let second_symbols = Interner::new();
        let (second_unit, second_model) =
            parsed_model(&second_packages, &second_symbols, API_SOURCE);
        let second = make_view(
            &second_packages,
            &second_symbols,
            second_unit,
            second_model,
            None,
        );

        assert_ne!(first.package, second.package);
        assert_ne!(
            first.interfaces["api"].sai_name,
            second.interfaces["api"].sai_name
        );
        assert_ne!(first.interfaces["api"].def, second.interfaces["api"].def);
        assert_eq!(fingerprint(&first), fingerprint(&second));
    }

    #[test]
    fn stable_hash_ignores_definition_tombstones_and_assignment_history() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let (unit, full_model) = parsed_model(
            &packages,
            &symbols,
            "\
package acme:api@1.0.0;
interface zeta {}
interface api { record point { x: u32 } }
world app {}
",
        );
        let package = packages.intern(unit.stable_key()).unwrap();
        let persistent_paths =
            Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        let _first = make_view(
            &packages,
            &symbols,
            unit.clone(),
            full_model,
            Some(Arc::clone(&persistent_paths)),
        );

        let (reduced_unit, reduced_model) = parsed_model(&packages, &symbols, API_SOURCE);
        let after_removal = make_view(
            &packages,
            &symbols,
            reduced_unit.clone(),
            reduced_model.clone(),
            Some(persistent_paths),
        );
        assert!(
            after_removal
                .defs
                .iter_enumerated()
                .any(|(_, definition)| definition.is_none())
        );

        let clean = make_view(&packages, &symbols, reduced_unit, reduced_model, None);
        assert_eq!(fingerprint(&after_removal), fingerprint(&clean));
    }
}
