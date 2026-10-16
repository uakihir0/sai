use crate::body::{Body, FnParam, FnSig};
use crate::item_tree::RawAttr;
use crate::types::{Bound, EffectRef, Generics, LangItem, Name, Path, Ty};
use crate::{HirId, LocalDefId, OwnerId, Safety, Span, Symbol, Visibility};
use sai_diagnostics::Diagnostic;
use std::hash::{Hash, Hasher};

/// A top-level HIR value for one definition, backed by the query-session arena.
///
/// The arena outlives memoized owners. This value borrows node data and raw
/// attributes from that arena and the attribute query, respectively.
pub struct HirOwner<'hir> {
    pub id: OwnerId,
    pub node: OwnerNode<'hir>,
    pub body: Option<&'hir Body<'hir>>,
    pub raw_attrs: &'hir [RawAttr],
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum OwnerNode<'hir> {
    Item(&'hir Item<'hir>),
    TraitItem(&'hir TraitItem<'hir>),
    ImplItem(&'hir ImplItem<'hir>),
    ExternItem(&'hir ExternItem<'hir>),
    InterfaceItem(&'hir InterfaceItem<'hir>),
    WorldItem(&'hir WorldItem<'hir>),
    DerivedImpl(&'hir DerivedImpl),
}

#[derive(Clone, Copy, Debug)]
pub struct Item<'hir> {
    pub owner_id: OwnerId,
    pub name: Name,
    pub vis: Visibility,
    pub kind: ItemKind<'hir>,
    pub span: Span,
}

impl PartialEq for Item<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.owner_id == other.owner_id
            && self.name == other.name
            && self.vis == other.vis
            && self.kind == other.kind
    }
}

impl Eq for Item<'_> {}

impl Hash for Item<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner_id.hash(state);
        self.name.hash(state);
        self.vis.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ItemKind<'hir> {
    Fn {
        sig: FnSig<'hir>,
        generics: Generics<'hir>,
        has_body: bool,
    },
    Struct {
        generics: Generics<'hir>,
        fields: &'hir [FieldDef<'hir>],
    },
    Alias {
        generics: Generics<'hir>,
        ty: &'hir Ty<'hir>,
    },
    Refined {
        base: &'hir Ty<'hir>,
    },
    Enum {
        generics: Generics<'hir>,
        variants: &'hir [Variant<'hir>],
    },
    Distinct {
        generics: Generics<'hir>,
        pub_ctor: bool,
        base: &'hir Ty<'hir>,
        has_predicate: bool,
    },
    Resource {
        fields: &'hir [FieldDef<'hir>],
    },
    Trait {
        safety: Safety,
        generics: Generics<'hir>,
        supertraits: &'hir [Bound<'hir>],
        items: &'hir [OwnerId],
    },
    Impl {
        safety: Safety,
        generics: Generics<'hir>,
        kind: ImplKind<'hir>,
        items: &'hir [OwnerId],
    },
    Const {
        ty: &'hir Ty<'hir>,
    },
    Invariant {
        target: Path<'hir>,
    },
    Effect {
        covers: &'hir [EffectRef<'hir>],
    },
    Interface {
        items: &'hir [OwnerId],
    },
    World {
        items: &'hir [OwnerId],
    },
    ExternBlock {
        safety: Safety,
        abi: ExternAbi,
        module: Option<Symbol>,
        items: &'hir [OwnerId],
    },
    Test {
        name: &'hir str,
    },
    Property {
        name: &'hir str,
        params: &'hir [FnParam<'hir>],
    },
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ExternAbi {
    Wasm,
    C,
    Err,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ImplKind<'hir> {
    Inherent {
        self_ty: &'hir Ty<'hir>,
    },
    Trait {
        trait_ref: Path<'hir>,
        self_ty: &'hir Ty<'hir>,
    },
    Interface {
        path: Path<'hir>,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct TraitItem<'hir> {
    pub owner_id: OwnerId,
    pub trait_def: LocalDefId,
    pub name: Name,
    pub kind: TraitItemKind<'hir>,
    pub span: Span,
}

impl PartialEq for TraitItem<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.owner_id == other.owner_id
            && self.trait_def == other.trait_def
            && self.name == other.name
            && self.kind == other.kind
    }
}

impl Eq for TraitItem<'_> {}

impl Hash for TraitItem<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner_id.hash(state);
        self.trait_def.hash(state);
        self.name.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum TraitItemKind<'hir> {
    AssocType {
        bounds: &'hir [Bound<'hir>],
    },
    Fn {
        sig: FnSig<'hir>,
        generics: Generics<'hir>,
        has_body: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ImplItem<'hir> {
    pub owner_id: OwnerId,
    pub impl_def: LocalDefId,
    pub name: Name,
    pub vis: Visibility,
    pub kind: ImplItemKind<'hir>,
    pub span: Span,
}

impl PartialEq for ImplItem<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.owner_id == other.owner_id
            && self.impl_def == other.impl_def
            && self.name == other.name
            && self.vis == other.vis
            && self.kind == other.kind
    }
}

impl Eq for ImplItem<'_> {}

impl Hash for ImplItem<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner_id.hash(state);
        self.impl_def.hash(state);
        self.name.hash(state);
        self.vis.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ImplItemKind<'hir> {
    AssocType {
        ty: &'hir Ty<'hir>,
    },
    Fn {
        sig: FnSig<'hir>,
        generics: Generics<'hir>,
        has_body: bool,
    },
}

#[derive(Clone, Copy, Debug)]
pub struct ExternItem<'hir> {
    pub owner_id: OwnerId,
    pub block: LocalDefId,
    pub name: Name,
    pub sig: FnSig<'hir>,
    pub generics: Generics<'hir>,
    pub span: Span,
}

impl PartialEq for ExternItem<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.owner_id == other.owner_id
            && self.block == other.block
            && self.name == other.name
            && self.sig == other.sig
            && self.generics == other.generics
    }
}

impl Eq for ExternItem<'_> {}

impl Hash for ExternItem<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner_id.hash(state);
        self.block.hash(state);
        self.name.hash(state);
        self.sig.hash(state);
        self.generics.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct FieldDef<'hir> {
    pub hir_id: HirId,
    pub def_id: LocalDefId,
    pub vis: Visibility,
    pub name: Name,
    pub ty: &'hir Ty<'hir>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Variant<'hir> {
    pub hir_id: HirId,
    pub def_id: LocalDefId,
    pub name: Name,
    pub shape: VariantShape<'hir>,
    pub discriminant: Option<i128>,
    /// Whether an `= INT` discriminant was written, including values too large
    /// to fit in the semantic `i128` field above.
    pub has_explicit_discriminant: bool,
    /// Complete source range of the variant declaration.
    pub span: Span,
    /// Complete source range of a tuple or record payload, when present.
    pub payload_span: Option<Span>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum VariantShape<'hir> {
    Unit,
    Tuple(&'hir [Ty<'hir>]),
    Record(&'hir [FieldDef<'hir>]),
}

#[derive(Clone, Copy, Debug)]
pub struct DerivedImpl {
    pub owner_id: OwnerId,
    pub trait_: LangItem,
    pub self_def: LocalDefId,
    pub span: Span,
}

impl PartialEq for DerivedImpl {
    fn eq(&self, other: &Self) -> bool {
        self.owner_id == other.owner_id
            && self.trait_ == other.trait_
            && self.self_def == other.self_def
    }
}

impl Eq for DerivedImpl {}

impl Hash for DerivedImpl {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner_id.hash(state);
        self.trait_.hash(state);
        self.self_def.hash(state);
    }
}

#[derive(Clone, Copy, Debug)]
pub struct InterfaceItem<'hir> {
    pub owner_id: OwnerId,
    pub interface: LocalDefId,
    pub name: Name,
    pub kind: InterfaceItemKind<'hir>,
    pub span: Span,
    /// Exact syntax range of this item's generic parameter list, when present.
    pub generic_params_span: Option<Span>,
    /// Exact syntax range of this item's `where` clause, when present.
    pub where_clause_span: Option<Span>,
}

impl PartialEq for InterfaceItem<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.owner_id == other.owner_id
            && self.interface == other.interface
            && self.name == other.name
            && self.kind == other.kind
    }
}

impl Eq for InterfaceItem<'_> {}

impl Hash for InterfaceItem<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner_id.hash(state);
        self.interface.hash(state);
        self.name.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Copy, Debug)]
pub enum InterfaceItemKind<'hir> {
    Struct {
        generics: Generics<'hir>,
        fields: &'hir [FieldDef<'hir>],
    },
    Alias {
        generics: Generics<'hir>,
        ty: &'hir Ty<'hir>,
    },
    Refined {
        base: &'hir Ty<'hir>,
        clause: Span,
    },
    Enum {
        generics: Generics<'hir>,
        variants: &'hir [Variant<'hir>],
    },
    Resource {
        members: &'hir [OwnerId],
    },
    Fn {
        sig: FnSig<'hir>,
        generics: Generics<'hir>,
    },
    ResourceMember {
        resource: LocalDefId,
        role: MemberRole,
        sig: FnSig<'hir>,
        generics: Generics<'hir>,
    },
}

impl PartialEq for InterfaceItemKind<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Struct {
                    generics: left_generics,
                    fields: left_fields,
                },
                Self::Struct {
                    generics: right_generics,
                    fields: right_fields,
                },
            ) => left_generics == right_generics && left_fields == right_fields,
            (
                Self::Alias {
                    generics: left_generics,
                    ty: left_ty,
                },
                Self::Alias {
                    generics: right_generics,
                    ty: right_ty,
                },
            ) => left_generics == right_generics && left_ty == right_ty,
            (Self::Refined { base: left, .. }, Self::Refined { base: right, .. }) => left == right,
            (
                Self::Enum {
                    generics: left_generics,
                    variants: left_variants,
                },
                Self::Enum {
                    generics: right_generics,
                    variants: right_variants,
                },
            ) => left_generics == right_generics && left_variants == right_variants,
            (Self::Resource { members: left }, Self::Resource { members: right }) => left == right,
            (
                Self::Fn {
                    sig: left_sig,
                    generics: left_generics,
                },
                Self::Fn {
                    sig: right_sig,
                    generics: right_generics,
                },
            ) => left_sig == right_sig && left_generics == right_generics,
            (
                Self::ResourceMember {
                    resource: left_resource,
                    role: left_role,
                    sig: left_sig,
                    generics: left_generics,
                },
                Self::ResourceMember {
                    resource: right_resource,
                    role: right_role,
                    sig: right_sig,
                    generics: right_generics,
                },
            ) => {
                left_resource == right_resource
                    && left_role == right_role
                    && left_sig == right_sig
                    && left_generics == right_generics
            }
            _ => false,
        }
    }
}

impl Eq for InterfaceItemKind<'_> {}

impl Hash for InterfaceItemKind<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Struct { generics, fields } => {
                generics.hash(state);
                fields.hash(state);
            }
            Self::Alias { generics, ty } => {
                generics.hash(state);
                ty.hash(state);
            }
            Self::Refined { base, .. } => base.hash(state),
            Self::Enum { generics, variants } => {
                generics.hash(state);
                variants.hash(state);
            }
            Self::Resource { members } => members.hash(state),
            Self::Fn { sig, generics } => {
                sig.hash(state);
                generics.hash(state);
            }
            Self::ResourceMember {
                resource,
                role,
                sig,
                generics,
            } => {
                resource.hash(state);
                role.hash(state);
                sig.hash(state);
                generics.hash(state);
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MemberRole {
    Constructor,
    Method,
    Static,
}

#[derive(Clone, Copy, Debug)]
pub struct WorldItem<'hir> {
    pub owner_id: OwnerId,
    pub world: LocalDefId,
    pub kind: WorldItemKind<'hir>,
    pub span: Span,
}

impl PartialEq for WorldItem<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.owner_id == other.owner_id && self.world == other.world && self.kind == other.kind
    }
}

impl Eq for WorldItem<'_> {}

impl Hash for WorldItem<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.owner_id.hash(state);
        self.world.hash(state);
        self.kind.hash(state);
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WorldItemKind<'hir> {
    Import(WorldRef<'hir>),
    Export(WorldRef<'hir>),
    ImportFn { name: Name, sig: FnSig<'hir> },
    ExportFn { name: Name, sig: FnSig<'hir> },
    Include(WorldRef<'hir>),
}

#[derive(Clone, Copy, Debug)]
pub enum WorldRef<'hir> {
    Pkg {
        hir_id: HirId,
        path: &'hir str,
        span: Span,
    },
    Mod(Path<'hir>),
}

impl PartialEq for WorldRef<'_> {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (
                Self::Pkg {
                    hir_id: left_id,
                    path: left_path,
                    ..
                },
                Self::Pkg {
                    hir_id: right_id,
                    path: right_path,
                    ..
                },
            ) => left_id == right_id && left_path == right_path,
            (Self::Mod(left), Self::Mod(right)) => left == right,
            _ => false,
        }
    }
}

impl Eq for WorldRef<'_> {}

impl Hash for WorldRef<'_> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        std::mem::discriminant(self).hash(state);
        match self {
            Self::Pkg { hir_id, path, .. } => {
                hir_id.hash(state);
                path.hash(state);
            }
            Self::Mod(path) => path.hash(state),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::body::FnSig;
    use crate::types::{EffectRef, Generics, PathRoot, TyKind};
    use crate::{
        DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, HirId, PackageInterner,
        SpanCtxt,
    };
    use sai_intern::{Interner, ItemLocalId};
    use sai_vfs::{FileId, TextRange, TextSize};
    use std::collections::hash_map::DefaultHasher;

    struct TestIds {
        owner: OwnerId,
        other_owner: OwnerId,
        local: LocalDefId,
        other_local: LocalDefId,
        hir: HirId,
        symbol: Symbol,
        interner: Interner,
    }

    fn test_ids() -> TestIds {
        let packages = PackageInterner::new();
        let package = packages.intern("acme:items#1.0").expect("package interns");
        let interner = Interner::new();
        let symbol = interner.intern("sample").expect("symbol interns");
        let paths = DefPathInterner::new(package, &packages, &interner).expect("path interner");
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(symbol),
                disambiguator: 0,
            }],
            &packages,
            &interner,
        )
        .expect("definition path");
        let local = paths.intern(path).expect("definition interns");
        let other_symbol = interner.intern("other").expect("symbol interns");
        let other_path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data: DefPathData::TypeNs(other_symbol),
                disambiguator: 0,
            }],
            &packages,
            &interner,
        )
        .expect("other definition path");
        let other_local = paths.intern(other_path).expect("other definition interns");
        let owner = OwnerId { def_id: local };
        TestIds {
            owner,
            other_owner: OwnerId {
                def_id: other_local,
            },
            local,
            other_local,
            hir: HirId {
                owner,
                local_id: ItemLocalId::default(),
            },
            symbol,
            interner,
        }
    }

    fn span(offset: u32) -> Span {
        Span {
            file: FileId::new(1),
            range: TextRange::empty(TextSize::new(offset)),
            ctxt: SpanCtxt::Root,
        }
    }

    fn name(ids: &TestIds, offset: u32) -> Name {
        Name {
            sym: ids.symbol,
            span: span(offset),
        }
    }

    fn ty(ids: &TestIds, offset: u32) -> Ty<'static> {
        Ty {
            hir_id: ids.hir,
            kind: TyKind::Infer,
            span: span(offset),
        }
    }

    fn empty_generics() -> Generics<'static> {
        Generics {
            params: &[],
            where_preds: &[],
        }
    }

    fn hash<T: Hash>(value: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn item_and_synthetic_owner_equality_ignores_source_locations() {
        let ids = test_ids();
        let ty_a = ty(&ids, 1);
        let ty_b = ty(&ids, 20);
        let left = Item {
            owner_id: ids.owner,
            name: name(&ids, 2),
            vis: Visibility::Pub,
            kind: ItemKind::Alias {
                generics: empty_generics(),
                ty: &ty_a,
            },
            span: span(0),
        };
        let right = Item {
            owner_id: ids.owner,
            name: name(&ids, 21),
            vis: Visibility::Pub,
            kind: ItemKind::Alias {
                generics: empty_generics(),
                ty: &ty_b,
            },
            span: span(19),
        };
        assert_eq!(left, right);
        assert_eq!(hash(&left), hash(&right));
        assert_ne!(
            left,
            Item {
                owner_id: ids.other_owner,
                ..right
            }
        );

        let trait_item_left = TraitItem {
            owner_id: ids.owner,
            trait_def: ids.local,
            name: name(&ids, 3),
            kind: TraitItemKind::AssocType { bounds: &[] },
            span: span(2),
        };
        let trait_item_right = TraitItem {
            name: name(&ids, 30),
            span: span(20),
            ..trait_item_left
        };
        assert_eq!(trait_item_left, trait_item_right);
        assert_eq!(hash(&trait_item_left), hash(&trait_item_right));
        assert_ne!(
            trait_item_left,
            TraitItem {
                trait_def: ids.other_local,
                ..trait_item_right
            }
        );
        assert_ne!(
            trait_item_left,
            TraitItem {
                owner_id: ids.other_owner,
                ..trait_item_right
            }
        );

        let impl_item_left = ImplItem {
            owner_id: ids.owner,
            impl_def: ids.local,
            name: name(&ids, 4),
            vis: Visibility::Pub,
            kind: ImplItemKind::AssocType { ty: &ty_a },
            span: span(3),
        };
        let impl_item_right = ImplItem {
            name: name(&ids, 40),
            kind: ImplItemKind::AssocType { ty: &ty_b },
            span: span(30),
            ..impl_item_left
        };
        assert_eq!(impl_item_left, impl_item_right);
        assert_eq!(hash(&impl_item_left), hash(&impl_item_right));
        assert_ne!(
            impl_item_left,
            ImplItem {
                impl_def: ids.other_local,
                ..impl_item_right
            }
        );
        assert_ne!(
            impl_item_left,
            ImplItem {
                owner_id: ids.other_owner,
                ..impl_item_right
            }
        );

        let signature_left = FnSig {
            safety: Safety::Safe,
            is_async: false,
            params: &[],
            ret: Some(&ty_a),
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let signature_right = FnSig {
            ret: Some(&ty_b),
            ..signature_left
        };
        let extern_item_left = ExternItem {
            owner_id: ids.owner,
            block: ids.local,
            name: name(&ids, 5),
            sig: signature_left,
            generics: empty_generics(),
            span: span(4),
        };
        let extern_item_right = ExternItem {
            name: name(&ids, 50),
            sig: signature_right,
            span: span(40),
            ..extern_item_left
        };
        assert_eq!(extern_item_left, extern_item_right);
        assert_eq!(hash(&extern_item_left), hash(&extern_item_right));
        assert_ne!(
            extern_item_left,
            ExternItem {
                block: ids.other_local,
                ..extern_item_right
            }
        );
        assert_ne!(
            extern_item_left,
            ExternItem {
                owner_id: ids.other_owner,
                ..extern_item_right
            }
        );

        let interface_item_left = InterfaceItem {
            owner_id: ids.owner,
            interface: ids.local,
            name: name(&ids, 6),
            kind: InterfaceItemKind::Struct {
                generics: empty_generics(),
                fields: &[],
            },
            span: span(5),
            generic_params_span: None,
            where_clause_span: None,
        };
        let interface_item_right = InterfaceItem {
            name: name(&ids, 60),
            span: span(50),
            ..interface_item_left
        };
        assert_eq!(interface_item_left, interface_item_right);
        assert_eq!(hash(&interface_item_left), hash(&interface_item_right));
        assert_ne!(
            interface_item_left,
            InterfaceItem {
                interface: ids.other_local,
                ..interface_item_right
            }
        );
        assert_ne!(
            interface_item_left,
            InterfaceItem {
                owner_id: ids.other_owner,
                ..interface_item_right
            }
        );

        let world_item_left = WorldItem {
            owner_id: ids.owner,
            world: ids.local,
            kind: WorldItemKind::Import(WorldRef::Pkg {
                hir_id: ids.hir,
                path: "acme:dep/core",
                span: span(7),
            }),
            span: span(6),
        };
        let world_item_right = WorldItem {
            kind: WorldItemKind::Import(WorldRef::Pkg {
                hir_id: ids.hir,
                path: "acme:dep/core",
                span: span(70),
            }),
            span: span(60),
            ..world_item_left
        };
        assert_eq!(world_item_left, world_item_right);
        assert_eq!(hash(&world_item_left), hash(&world_item_right));
        assert_ne!(
            world_item_left,
            WorldItem {
                world: ids.other_local,
                ..world_item_right
            }
        );
        assert_ne!(
            world_item_left,
            WorldItem {
                owner_id: ids.other_owner,
                ..world_item_right
            }
        );

        let derived_left = DerivedImpl {
            owner_id: ids.owner,
            trait_: LangItem::Clone,
            self_def: ids.local,
            span: span(4),
        };
        let derived_right = DerivedImpl {
            span: span(40),
            ..derived_left
        };
        assert_eq!(derived_left, derived_right);
        assert_eq!(hash(&derived_left), hash(&derived_right));
        assert_ne!(
            derived_left,
            DerivedImpl {
                self_def: ids.other_local,
                ..derived_right
            }
        );
        assert_ne!(
            derived_left,
            DerivedImpl {
                owner_id: ids.other_owner,
                ..derived_right
            }
        );

        let refined_left = InterfaceItemKind::Refined {
            base: &ty_a,
            clause: span(5),
        };
        let refined_right = InterfaceItemKind::Refined {
            base: &ty_b,
            clause: span(50),
        };
        assert_eq!(refined_left, refined_right);
        assert_eq!(hash(&refined_left), hash(&refined_right));

        let pkg_left = WorldRef::Pkg {
            hir_id: ids.hir,
            path: "acme:dep/core",
            span: span(6),
        };
        let pkg_right = WorldRef::Pkg {
            hir_id: ids.hir,
            path: "acme:dep/core",
            span: span(60),
        };
        assert_eq!(pkg_left, pkg_right);
        assert_eq!(hash(&pkg_left), hash(&pkg_right));
    }

    #[test]
    fn arena_stores_item_families_and_world_package_paths() {
        let ids = test_ids();
        let arena = crate::arena::HirArena::new();
        let type_node = arena.alloc_ty(ty(&ids, 3));
        let test_name = arena.alloc_str("smoke");
        let item = arena.alloc_item(Item {
            owner_id: ids.owner,
            name: name(&ids, 2),
            vis: Visibility::Private,
            kind: ItemKind::Test { name: test_name },
            span: span(1),
        });
        let test_name_address = test_name.as_ptr();
        let field = FieldDef {
            hir_id: ids.hir,
            def_id: ids.local,
            vis: Visibility::Pub,
            name: name(&ids, 4),
            ty: type_node,
        };
        let fields = arena.alloc_field_defs(&[field]);
        let variant = Variant {
            hir_id: ids.hir,
            def_id: ids.local,
            name: name(&ids, 5),
            shape: VariantShape::Record(fields),
            discriminant: Some(7),
            has_explicit_discriminant: true,
            span: span(7),
            payload_span: Some(span(8)),
        };
        let variants = arena.alloc_variants(&[variant]);
        let world_ref = WorldRef::Pkg {
            hir_id: ids.hir,
            path: arena.alloc_str("acme:dep/core"),
            span: span(6),
        };
        let world_item = arena.alloc_world_item(WorldItem {
            owner_id: ids.owner,
            world: ids.local,
            kind: WorldItemKind::Import(world_ref),
            span: span(7),
        });

        for offset in 0..128 {
            arena.alloc_str(if offset % 2 == 0 {
                "growth"
            } else {
                "allocation"
            });
        }

        assert_eq!(item.kind, ItemKind::Test { name: "smoke" });
        assert_eq!(variants[0].shape, VariantShape::Record(fields));
        assert_eq!(fields[0].ty, type_node);
        assert_eq!(
            world_item.kind,
            WorldItemKind::Import(WorldRef::Pkg {
                hir_id: ids.hir,
                path: "acme:dep/core",
                span: span(900),
            })
        );
        assert_eq!(test_name_address, test_name.as_ptr());
        assert_eq!(arena.alloc_str("UTF-8"), "UTF-8");
    }

    #[test]
    fn declaration_variants_preserve_interface_and_foreign_item_metadata() {
        let ids = test_ids();
        let arena = crate::arena::HirArena::new();
        let ty = arena.alloc_ty(ty(&ids, 3));
        let signature = FnSig {
            safety: Safety::Safe,
            is_async: false,
            params: &[],
            ret: Some(ty),
            borrows: None,
            effects: None,
            requires: &[],
            ensures: &[],
        };
        let path = crate::types::Path {
            hir_id: ids.hir,
            root: PathRoot::Plain,
            segments: &[],
            span: span(0),
        };
        let owner_ids = arena.alloc_owner_ids(&[ids.owner, ids.other_owner]);
        let item_kinds = [
            ItemKind::Fn {
                sig: signature,
                generics: empty_generics(),
                has_body: true,
            },
            ItemKind::Struct {
                generics: empty_generics(),
                fields: &[],
            },
            ItemKind::Alias {
                generics: empty_generics(),
                ty,
            },
            ItemKind::Refined { base: ty },
            ItemKind::Enum {
                generics: empty_generics(),
                variants: &[],
            },
            ItemKind::Distinct {
                generics: empty_generics(),
                pub_ctor: true,
                base: ty,
                has_predicate: false,
            },
            ItemKind::Resource { fields: &[] },
            ItemKind::Trait {
                safety: Safety::Safe,
                generics: empty_generics(),
                supertraits: &[],
                items: owner_ids,
            },
            ItemKind::Impl {
                safety: Safety::Unsafe,
                generics: empty_generics(),
                kind: ImplKind::Inherent { self_ty: ty },
                items: owner_ids,
            },
            ItemKind::Const { ty },
            ItemKind::Invariant { target: path },
            ItemKind::Effect {
                covers: &[EffectRef {
                    hir_id: ids.hir,
                    segments: &[],
                }],
            },
            ItemKind::Interface { items: owner_ids },
            ItemKind::World { items: owner_ids },
            ItemKind::ExternBlock {
                safety: Safety::Safe,
                abi: ExternAbi::Wasm,
                module: Some(ids.symbol),
                items: owner_ids,
            },
            ItemKind::Test { name: "test" },
            ItemKind::Property {
                name: "property",
                params: &[],
            },
        ];
        let items = item_kinds.map(|kind| Item {
            owner_id: ids.owner,
            name: name(&ids, 0),
            vis: Visibility::Private,
            kind,
            span: span(0),
        });
        let stored_items = arena.alloc_items(&items);

        let impl_kinds = [
            ImplKind::Inherent { self_ty: ty },
            ImplKind::Trait {
                trait_ref: path,
                self_ty: ty,
            },
            ImplKind::Interface { path },
        ];
        let interface_kinds = [
            InterfaceItemKind::Struct {
                generics: empty_generics(),
                fields: &[],
            },
            InterfaceItemKind::Alias {
                generics: empty_generics(),
                ty,
            },
            InterfaceItemKind::Refined {
                base: ty,
                clause: span(4),
            },
            InterfaceItemKind::Enum {
                generics: empty_generics(),
                variants: &[],
            },
            InterfaceItemKind::Resource { members: owner_ids },
            InterfaceItemKind::Fn {
                sig: signature,
                generics: empty_generics(),
            },
            InterfaceItemKind::ResourceMember {
                resource: ids.local,
                role: MemberRole::Method,
                sig: signature,
                generics: empty_generics(),
            },
        ];
        let interface = arena.alloc_interface_item(InterfaceItem {
            owner_id: ids.owner,
            interface: ids.local,
            name: name(&ids, 1),
            kind: interface_kinds[6],
            span: span(0),
            generic_params_span: None,
            where_clause_span: None,
        });
        let extern_item = arena.alloc_extern_item(ExternItem {
            owner_id: ids.owner,
            block: ids.local,
            name: name(&ids, 2),
            sig: signature,
            generics: empty_generics(),
            span: span(1),
        });
        let trait_item = arena.alloc_trait_item(TraitItem {
            owner_id: ids.owner,
            trait_def: ids.local,
            name: name(&ids, 3),
            kind: TraitItemKind::Fn {
                sig: signature,
                generics: empty_generics(),
                has_body: false,
            },
            span: span(2),
        });
        let impl_item = arena.alloc_impl_item(ImplItem {
            owner_id: ids.owner,
            impl_def: ids.local,
            name: name(&ids, 4),
            vis: Visibility::Pub,
            kind: ImplItemKind::AssocType { ty },
            span: span(3),
        });
        let trait_assoc_type = arena.alloc_trait_item(TraitItem {
            owner_id: ids.owner,
            trait_def: ids.local,
            name: name(&ids, 5),
            kind: TraitItemKind::AssocType { bounds: &[] },
            span: span(4),
        });
        let impl_fn = arena.alloc_impl_item(ImplItem {
            owner_id: ids.owner,
            impl_def: ids.local,
            name: name(&ids, 6),
            vis: Visibility::Private,
            kind: ImplItemKind::Fn {
                sig: signature,
                generics: empty_generics(),
                has_body: true,
            },
            span: span(5),
        });
        let world_kinds = [
            WorldItemKind::Import(WorldRef::Pkg {
                hir_id: ids.hir,
                path: "acme:dep/core",
                span: span(6),
            }),
            WorldItemKind::Export(WorldRef::Mod(path)),
            WorldItemKind::ImportFn {
                name: name(&ids, 7),
                sig: signature,
            },
            WorldItemKind::ExportFn {
                name: name(&ids, 8),
                sig: signature,
            },
            WorldItemKind::Include(WorldRef::Mod(path)),
        ];

        assert_eq!(interface.kind, interface_kinds[6]);
        assert_eq!(
            trait_item.kind,
            TraitItemKind::Fn {
                sig: signature,
                generics: empty_generics(),
                has_body: false,
            }
        );
        assert_eq!(impl_item.kind, ImplItemKind::AssocType { ty });
        assert_eq!(extern_item.sig, signature);
        assert_eq!(
            trait_assoc_type.kind,
            TraitItemKind::AssocType { bounds: &[] }
        );
        assert!(matches!(
            impl_fn.kind,
            ImplItemKind::Fn { has_body: true, .. }
        ));
        assert_eq!(
            ids.interner
                .resolve(ids.symbol)
                .expect("symbol resolves")
                .as_ref(),
            "sample"
        );
        assert_eq!(stored_items.len(), 17);
        assert_eq!(
            match stored_items[7] {
                Item {
                    kind: ItemKind::Trait { items, .. },
                    ..
                } => items,
                _ => unreachable!("item kind at this position is a trait"),
            },
            owner_ids
        );
        assert_eq!(impl_kinds.len(), 3);
        assert_eq!(world_kinds.len(), 5);
        assert_eq!([ExternAbi::Wasm, ExternAbi::C, ExternAbi::Err].len(), 3);
    }
}
