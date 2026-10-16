//! Qualified lookup through module-like path prefixes.
//!
//! The index is built from active source package scopes and optional WIT views
//! supplied by the caller. Supply every active source package that can own a
//! re-exported target, including transitive packages when a public re-export
//! points to one. All scopes and views must come from the same package graph
//! and its shared `PackageInterner`.
//! Package-local [`super::ModuleId`] values are deliberately not part of this
//! API: module targets are keyed by their canonical [`DefId`] instead.

use crate::package_scopes::{Binding, Phase1PackageScopes};
use crate::package_scopes::{DefKind, Res};
use sai_hir::{DefId, PackageId, Symbol, Visibility};
use sai_intern::Interner;
use sai_wit::{ViewInterface, ViewMember, WitView, WitViewDefKind};
use std::collections::HashMap;

/// A normalized module or interface target from a resolver `Res`.
///
/// `Res::Def(DefKind::Module, id)` and
/// `Res::Def(DefKind::Interface, id)` both name module-like prefixes. Keeping
/// only the canonical definition identity here avoids exposing the
/// package-local module index used to store a module's scope.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ModuleTarget {
    def_id: DefId,
}

impl ModuleTarget {
    /// Normalize a current resolver result into a module-like target.
    ///
    /// The target need not be present in a particular index; `lookup` reports
    /// `NotModule` when its source package or WIT view scope was not supplied.
    pub const fn from_res(res: Res) -> Option<Self> {
        match res {
            Res::Def(DefKind::Module | DefKind::Interface, def_id) => Some(Self { def_id }),
            Res::Def(_, _) | Res::Local(_) | Res::PrimTy(_) | Res::Err | Res::TestOnly(_) => None,
        }
    }

    pub const fn def_id(self) -> DefId {
        self.def_id
    }
}

/// Result of looking up a name through a module-like target.
///
/// `Found` and `Inaccessible` retain the exact scope binding, including its
/// original `Res` and source span, so later resolution and diagnostics do not
/// replace a re-export with an alias identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ModuleMemberLookup<'a> {
    Found(&'a Binding),
    /// A WIT member definition, which has no SAI source span.
    ///
    /// WIT view members are public and intentionally do not use `Binding`.
    WitMember(Res),
    Missing,
    Inaccessible(&'a Binding),
    NotModule,
}

#[derive(Clone, Copy)]
enum MemberScope<'a> {
    Source(&'a HashMap<Symbol, Binding>),
    WitInterface(&'a ViewInterface),
}

enum MemberLookupValue<'a> {
    Source(&'a Binding),
    Wit(Res),
}

impl<'a> MemberScope<'a> {
    fn get(self, name: Symbol, symbols: Option<&Interner>) -> Option<MemberLookupValue<'a>> {
        match self {
            Self::Source(members) => members.get(&name).map(MemberLookupValue::Source),
            Self::WitInterface(interface) => {
                let spelling = symbols?.resolve(name).ok()?;
                interface
                    .members
                    .get(spelling.as_ref())
                    .and_then(wit_member_res)
                    .map(MemberLookupValue::Wit)
            }
        }
    }

    fn iter(self) -> Option<impl Iterator<Item = (Symbol, &'a Binding)> + 'a> {
        match self {
            Self::Source(members) => Some(members.iter().map(|(name, binding)| (*name, binding))),
            // This legacy enumeration API returns source-backed Bindings.
            // WIT lookup has a separate span-free result representation.
            Self::WitInterface(_) => None,
        }
    }
}

#[derive(Clone, Copy)]
struct IndexedTarget<'a> {
    package: PackageId,
    scope: MemberScope<'a>,
}

/// An index from canonical module/interface identities to their member maps.
///
/// Construction hides the `ModuleId`-indexed representation in
/// `Phase1PackageScopes`; source lookup results contain canonical definitions
/// and original bindings, while WIT results are span-free.
pub struct ModuleMemberIndex<'a> {
    targets: HashMap<DefId, IndexedTarget<'a>>,
    wit_symbols: Option<&'a Interner>,
}

impl<'a> ModuleMemberIndex<'a> {
    /// Build an index over the supplied source package scopes.
    ///
    /// Pass the active source package scopes that can own any target reached
    /// while traversing a qualified path. A public re-export can preserve a
    /// definition from a transitive package, so direct dependencies alone may
    /// not be enough.
    pub fn new(package_scopes: impl IntoIterator<Item = &'a Phase1PackageScopes>) -> Self {
        Self::build(package_scopes, std::iter::empty(), None)
    }

    /// Build an index over source scopes and foreign WIT views.
    ///
    /// WIT interface members are keyed by canonical SAI spelling in the view
    /// and by session-local `Symbol` in resolver paths, so `symbols` must be
    /// the interner used to construct those paths and views. WIT members are
    /// returned as [`ModuleMemberLookup::WitMember`], without manufacturing
    /// source spans or `Binding`s.
    pub fn new_with_wit_views(
        package_scopes: impl IntoIterator<Item = &'a Phase1PackageScopes>,
        wit_views: impl IntoIterator<Item = &'a WitView>,
        symbols: &'a Interner,
    ) -> Self {
        Self::build(package_scopes, wit_views, Some(symbols))
    }

    fn build(
        package_scopes: impl IntoIterator<Item = &'a Phase1PackageScopes>,
        wit_views: impl IntoIterator<Item = &'a WitView>,
        wit_symbols: Option<&'a Interner>,
    ) -> Self {
        let mut targets = HashMap::new();

        for package in package_scopes {
            for module in &package.modules {
                let def_id = module.module_def_id.to_def_id();
                targets.entry(def_id).or_insert(IndexedTarget {
                    package: module.module_def_id.package(),
                    scope: MemberScope::Source(&module.members),
                });
            }

            for (def_id, interface) in &package.interfaces {
                targets.entry(*def_id).or_insert(IndexedTarget {
                    package: def_id.package,
                    scope: MemberScope::Source(&interface.members),
                });
            }
        }

        for view in wit_views {
            for interface in view.interfaces.values() {
                targets.entry(interface.def).or_insert(IndexedTarget {
                    package: interface.def.package,
                    scope: MemberScope::WitInterface(interface),
                });
            }
        }

        Self {
            targets,
            wit_symbols,
        }
    }

    /// Look up a member of a module or interface `Res`.
    ///
    /// A missing source/WIT scope and a non-module `Res` both yield `NotModule`.
    /// A member from another package is available only when its effective
    /// visibility is `Visibility::Pub` or `Visibility::Export`; private members
    /// remain available within their declaring package.
    pub fn lookup(
        &self,
        target: Res,
        name: Symbol,
        requesting_package: PackageId,
    ) -> ModuleMemberLookup<'a> {
        let Some(target) = ModuleTarget::from_res(target) else {
            return ModuleMemberLookup::NotModule;
        };
        self.lookup_target(target, name, requesting_package)
    }

    /// Look up a member using a previously normalized module target.
    pub fn lookup_target(
        &self,
        target: ModuleTarget,
        name: Symbol,
        requesting_package: PackageId,
    ) -> ModuleMemberLookup<'a> {
        let Some(indexed) = self.targets.get(&target.def_id) else {
            return ModuleMemberLookup::NotModule;
        };
        let Some(member) = indexed.scope.get(name, self.wit_symbols) else {
            return ModuleMemberLookup::Missing;
        };
        match member {
            MemberLookupValue::Wit(res) => ModuleMemberLookup::WitMember(res),
            MemberLookupValue::Source(binding) => {
                if indexed.package != requesting_package
                    && !matches!(binding.visibility, Visibility::Pub | Visibility::Export)
                {
                    ModuleMemberLookup::Inaccessible(binding)
                } else {
                    ModuleMemberLookup::Found(binding)
                }
            }
        }
    }

    /// Enumerate source-backed bindings visible through a module or interface.
    ///
    /// A missing target or WIT view scope returns `None`, because WIT members
    /// have no source `Binding`. Package-private members are included when the
    /// requesting package owns the target; for foreign source targets, only
    /// `Pub` and `Export` bindings are included. The iteration order is
    /// unspecified because source member scopes use hash maps.
    pub fn visible_members(
        &self,
        target: ModuleTarget,
        requesting_package: PackageId,
    ) -> Option<impl Iterator<Item = (Symbol, &'a Binding)> + 'a> {
        let indexed = *self.targets.get(&target.def_id)?;
        Some(indexed.scope.iter()?.filter_map(move |(name, binding)| {
            (indexed.package == requesting_package
                || matches!(binding.visibility, Visibility::Pub | Visibility::Export))
            .then_some((name, binding))
        }))
    }
}

fn wit_member_res(member: &ViewMember) -> Option<Res> {
    let kind = match member.kind {
        WitViewDefKind::Struct => DefKind::Struct,
        WitViewDefKind::Enum => DefKind::Enum,
        WitViewDefKind::Resource => DefKind::Resource,
        WitViewDefKind::Alias => DefKind::Alias,
        WitViewDefKind::Fn => DefKind::Fn,
        WitViewDefKind::Interface
        | WitViewDefKind::World
        | WitViewDefKind::Field
        | WitViewDefKind::Variant
        | WitViewDefKind::AssocFn
        | WitViewDefKind::WorldFn => return None,
    };
    Some(Res::Def(kind, member.def))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::package_scopes::{
        BindingOrigin, InterfaceScope, Phase1ModuleScopes, Phase1PackageScopes,
    };
    use sai_hir::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, LocalDefId};
    use sai_intern::{Interner, PackageInterner};
    use sai_vfs::{FileId, Span, TextRange, TextSize};
    use std::collections::{BTreeMap, HashMap};

    fn package_id(packages: &PackageInterner, name: &str) -> PackageId {
        packages.intern(name).expect("package interns")
    }

    fn local_def_id(
        package: PackageId,
        packages: &PackageInterner,
        symbols: &Interner,
        path: Vec<DefPathData>,
    ) -> LocalDefId {
        let data = path
            .into_iter()
            .map(|data| DisambiguatedDefPathData {
                data,
                disambiguator: 0,
            })
            .collect();
        let path = DefPath::new(package, data, packages, symbols).expect("path is valid");
        DefPathInterner::new(package, packages, symbols)
            .expect("path interner is valid")
            .intern(path)
            .expect("definition interns")
    }

    fn symbol(symbols: &Interner, name: &str) -> Symbol {
        symbols.intern(name).expect("symbol interns")
    }

    fn binding(res: Res, visibility: Visibility) -> Binding {
        Binding {
            res,
            name_span: Span {
                file: FileId::new(1),
                range: TextRange::empty(TextSize::new(0)),
            },
            origin: BindingOrigin::Decl,
            visibility,
        }
    }

    fn module_scope(module_def_id: LocalDefId) -> Phase1ModuleScopes {
        Phase1ModuleScopes {
            module_def_id,
            declared: HashMap::new(),
            members: HashMap::new(),
            scope: HashMap::new(),
            imports: Vec::new(),
        }
    }

    fn package_scopes(
        modules: Vec<Phase1ModuleScopes>,
        interfaces: HashMap<DefId, InterfaceScope>,
    ) -> Phase1PackageScopes {
        Phase1PackageScopes {
            modules,
            interfaces,
            exports: BTreeMap::new(),
            top_level_owners: Vec::new(),
            function_owners: Vec::new(),
            invariant_owners: Vec::new(),
            refinement_predicate_owners: Vec::new(),
            extern_function_owners: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn follows_nested_module_members_without_exposing_module_ids() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let nested_name = symbol(&symbols, "nested");
        let nested = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::Module(nested_name)],
        );
        let leaf_name = symbol(&symbols, "leaf");
        let leaf = local_def_id(
            package,
            &packages,
            &symbols,
            vec![
                DefPathData::Module(nested_name),
                DefPathData::ValueNs(leaf_name),
            ],
        );

        let mut root_scope = module_scope(root);
        root_scope.members.insert(
            nested_name,
            binding(
                Res::Def(DefKind::Module, nested.to_def_id()),
                Visibility::Private,
            ),
        );
        let mut nested_scope = module_scope(nested);
        nested_scope.members.insert(
            leaf_name,
            binding(Res::Def(DefKind::Fn, leaf.to_def_id()), Visibility::Private),
        );
        let scopes = package_scopes(vec![root_scope, nested_scope], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);

        let nested_binding = match index.lookup(
            Res::Def(DefKind::Module, root.to_def_id()),
            nested_name,
            package,
        ) {
            ModuleMemberLookup::Found(binding) => binding,
            result => panic!("expected nested module, got {result:?}"),
        };
        assert_eq!(
            nested_binding.res,
            Res::Def(DefKind::Module, nested.to_def_id())
        );
        let leaf_binding = match index.lookup(nested_binding.res, leaf_name, package) {
            ModuleMemberLookup::Found(binding) => binding,
            result => panic!("expected nested leaf, got {result:?}"),
        };
        assert_eq!(leaf_binding.res, Res::Def(DefKind::Fn, leaf.to_def_id()));
    }

    #[test]
    fn reexport_lookup_preserves_original_resolution_identity() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let facade_name = symbol(&symbols, "facade");
        let facade = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::Module(facade_name)],
        );
        let source_item = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(symbol(&symbols, "original"))],
        );
        let reexport_name = symbol(&symbols, "renamed");

        let mut root_scope = module_scope(root);
        root_scope.members.insert(
            facade_name,
            binding(
                Res::Def(DefKind::Module, facade.to_def_id()),
                Visibility::Pub,
            ),
        );
        let mut facade_scope = module_scope(facade);
        facade_scope.members.insert(
            reexport_name,
            binding(
                Res::Def(DefKind::Fn, source_item.to_def_id()),
                Visibility::Pub,
            ),
        );
        let scopes = package_scopes(vec![root_scope, facade_scope], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);

        match index.lookup(
            Res::Def(DefKind::Module, facade.to_def_id()),
            reexport_name,
            package,
        ) {
            ModuleMemberLookup::Found(binding) => {
                assert_eq!(binding.res, Res::Def(DefKind::Fn, source_item.to_def_id()));
            }
            result => panic!("expected re-export, got {result:?}"),
        }
    }

    #[test]
    fn foreign_members_follow_effective_visibility() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let requester = package_id(&packages, "acme:app#1.0");
        let dependency = package_id(&packages, "acme:dependency#1.0");
        let root = local_def_id(dependency, &packages, &symbols, Vec::new());
        let private_name = symbol(&symbols, "private_fn");
        let item = local_def_id(
            dependency,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(private_name)],
        );
        let public_name = symbol(&symbols, "public_fn");
        let public_item = local_def_id(
            dependency,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(public_name)],
        );
        let exported_name = symbol(&symbols, "exported_fn");
        let exported_item = local_def_id(
            dependency,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(exported_name)],
        );
        let mut root_scope = module_scope(root);
        root_scope.members.insert(
            private_name,
            binding(Res::Def(DefKind::Fn, item.to_def_id()), Visibility::Private),
        );
        root_scope.members.insert(
            public_name,
            binding(
                Res::Def(DefKind::Fn, public_item.to_def_id()),
                Visibility::Pub,
            ),
        );
        root_scope.members.insert(
            exported_name,
            binding(
                Res::Def(DefKind::Fn, exported_item.to_def_id()),
                Visibility::Export,
            ),
        );
        let scopes = package_scopes(vec![root_scope], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);

        assert!(matches!(
            index.lookup(Res::Def(DefKind::Module, root.to_def_id()), private_name, requester),
            ModuleMemberLookup::Inaccessible(binding)
                if binding.res == Res::Def(DefKind::Fn, item.to_def_id())
        ));
        assert!(matches!(
            index.lookup(Res::Def(DefKind::Module, root.to_def_id()), public_name, requester),
            ModuleMemberLookup::Found(binding)
                if binding.res == Res::Def(DefKind::Fn, public_item.to_def_id())
        ));
        assert!(matches!(
            index.lookup(
                Res::Def(DefKind::Module, root.to_def_id()),
                exported_name,
                requester
            ),
            ModuleMemberLookup::Found(binding)
                if binding.res == Res::Def(DefKind::Fn, exported_item.to_def_id())
        ));
    }

    #[test]
    fn looks_up_interface_members_and_normalizes_interface_targets() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let interface = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::Interface(symbol(&symbols, "view"))],
        );
        let item = local_def_id(
            package,
            &packages,
            &symbols,
            vec![
                DefPathData::Interface(symbol(&symbols, "view")),
                DefPathData::ValueNs(symbol(&symbols, "read")),
            ],
        );
        let name = symbol(&symbols, "read");
        let mut members = HashMap::new();
        members.insert(
            name,
            binding(Res::Def(DefKind::Fn, item.to_def_id()), Visibility::Private),
        );
        let scopes = package_scopes(
            Vec::new(),
            HashMap::from([(interface.to_def_id(), InterfaceScope { members })]),
        );
        let index = ModuleMemberIndex::new([&scopes]);
        let res = Res::Def(DefKind::Interface, interface.to_def_id());
        let target = ModuleTarget::from_res(res).expect("interface normalizes");
        assert_eq!(target.def_id(), interface.to_def_id());

        match index.lookup_target(target, name, package) {
            ModuleMemberLookup::Found(binding) => {
                assert_eq!(binding.res, Res::Def(DefKind::Fn, item.to_def_id()));
            }
            result => panic!("expected interface member, got {result:?}"),
        }
    }

    #[test]
    fn distinguishes_missing_members_and_non_module_resolutions() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let function = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(symbol(&symbols, "f"))],
        );
        let absent = symbol(&symbols, "absent");
        let scopes = package_scopes(vec![module_scope(root)], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);

        assert_eq!(
            index.lookup(Res::Def(DefKind::Module, root.to_def_id()), absent, package),
            ModuleMemberLookup::Missing
        );
        assert_eq!(
            index.lookup(Res::Def(DefKind::Fn, function.to_def_id()), absent, package),
            ModuleMemberLookup::NotModule
        );
        assert_eq!(
            index.lookup(
                Res::Def(DefKind::Module, function.to_def_id()),
                absent,
                package
            ),
            ModuleMemberLookup::NotModule
        );
    }

    #[test]
    fn private_member_is_available_within_its_declaring_package() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let item = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(symbol(&symbols, "private_fn"))],
        );
        let name = symbol(&symbols, "private_fn");
        let mut root_scope = module_scope(root);
        root_scope.members.insert(
            name,
            binding(Res::Def(DefKind::Fn, item.to_def_id()), Visibility::Private),
        );
        let scopes = package_scopes(vec![root_scope], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);

        match index.lookup(Res::Def(DefKind::Module, root.to_def_id()), name, package) {
            ModuleMemberLookup::Found(binding) => {
                assert_eq!(binding.res, Res::Def(DefKind::Fn, item.to_def_id()));
            }
            result => panic!("expected package-local private member, got {result:?}"),
        }
    }

    #[test]
    fn visible_member_enumeration_includes_same_package_private_bindings() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let package = package_id(&packages, "acme:app#1.0");
        let root = local_def_id(package, &packages, &symbols, Vec::new());
        let private_name = symbol(&symbols, "private_fn");
        let private_item = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(private_name)],
        );
        let public_name = symbol(&symbols, "public_fn");
        let public_item = local_def_id(
            package,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(public_name)],
        );
        let mut root_scope = module_scope(root);
        root_scope.members.insert(
            private_name,
            binding(
                Res::Def(DefKind::Fn, private_item.to_def_id()),
                Visibility::Private,
            ),
        );
        root_scope.members.insert(
            public_name,
            binding(
                Res::Def(DefKind::Fn, public_item.to_def_id()),
                Visibility::Pub,
            ),
        );
        let scopes = package_scopes(vec![root_scope], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);

        let members = index
            .visible_members(
                ModuleTarget::from_res(Res::Def(DefKind::Module, root.to_def_id()))
                    .expect("module normalizes"),
                package,
            )
            .expect("module scope is indexed")
            .collect::<HashMap<_, _>>();

        assert_eq!(
            members.get(&private_name).map(|binding| binding.res),
            Some(Res::Def(DefKind::Fn, private_item.to_def_id()))
        );
        assert_eq!(
            members.get(&public_name).map(|binding| binding.res),
            Some(Res::Def(DefKind::Fn, public_item.to_def_id()))
        );
    }

    #[test]
    fn visible_member_enumeration_excludes_foreign_private_bindings() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let requester = package_id(&packages, "acme:app#1.0");
        let dependency = package_id(&packages, "acme:dependency#1.0");
        let root = local_def_id(dependency, &packages, &symbols, Vec::new());
        let private_name = symbol(&symbols, "private_fn");
        let private_item = local_def_id(
            dependency,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(private_name)],
        );
        let public_name = symbol(&symbols, "public_fn");
        let public_item = local_def_id(
            dependency,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(public_name)],
        );
        let exported_name = symbol(&symbols, "exported_fn");
        let exported_item = local_def_id(
            dependency,
            &packages,
            &symbols,
            vec![DefPathData::ValueNs(exported_name)],
        );
        let mut root_scope = module_scope(root);
        root_scope.members.insert(
            private_name,
            binding(
                Res::Def(DefKind::Fn, private_item.to_def_id()),
                Visibility::Private,
            ),
        );
        root_scope.members.insert(
            public_name,
            binding(
                Res::Def(DefKind::Fn, public_item.to_def_id()),
                Visibility::Pub,
            ),
        );
        root_scope.members.insert(
            exported_name,
            binding(
                Res::Def(DefKind::Fn, exported_item.to_def_id()),
                Visibility::Export,
            ),
        );
        let scopes = package_scopes(vec![root_scope], HashMap::new());
        let index = ModuleMemberIndex::new([&scopes]);

        let members = index
            .visible_members(
                ModuleTarget::from_res(Res::Def(DefKind::Module, root.to_def_id()))
                    .expect("module normalizes"),
                requester,
            )
            .expect("dependency scope is indexed")
            .collect::<HashMap<_, _>>();

        assert!(!members.contains_key(&private_name));
        assert_eq!(
            members.get(&public_name).map(|binding| binding.res),
            Some(Res::Def(DefKind::Fn, public_item.to_def_id()))
        );
        assert_eq!(
            members.get(&exported_name).map(|binding| binding.res),
            Some(Res::Def(DefKind::Fn, exported_item.to_def_id()))
        );
    }
}
