//! Stable structural hashes for the parser-independent WIT query model.

use crate::{
    Unsupported, WitCase, WitField, WitFunc, WitIfaceIdx, WitIfaceRef, WitIndex, WitIndexVec,
    WitInterface, WitItem, WitItemIdx, WitItemKind, WitMember, WitMemberKind, WitPackage,
    WitPackageDeclaration, WitPackageName, WitSpan, WitType, WitTypeRef, WitWorld, WitWorldIdx,
    WitWorldItem, WitWorldItemDirection, WitWorldItemOrder, WitWorldRef,
};
use sai_stable_hash::{StableHash, StableHasher};

macro_rules! stable_struct {
    ($type:ty, $tag:literal, $($field:ident),+ $(,)?) => {
        impl StableHash for $type {
            fn stable_hash(&self, hasher: &mut StableHasher) {
                hasher.write_tag($tag);
                $(self.$field.stable_hash(hasher);)+
            }
        }
    };
}

fn stable_version<T: ToString>(version: &Option<T>, hasher: &mut StableHasher) {
    version
        .as_ref()
        .map(ToString::to_string)
        .stable_hash(hasher);
}

impl StableHash for WitIfaceIdx {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x91);
        self.raw().stable_hash(hasher);
    }
}

impl StableHash for WitWorldIdx {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x92);
        self.raw().stable_hash(hasher);
    }
}

impl StableHash for WitItemIdx {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x93);
        self.raw().stable_hash(hasher);
    }
}

impl<I: WitIndex, T: StableHash> StableHash for WitIndexVec<I, T> {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0x90);
        hasher.write_tag(I::STABLE_TAG);
        hasher.write_len(u64::try_from(self.len()).unwrap_or(u64::MAX));
        for value in self.iter() {
            value.stable_hash(hasher);
        }
    }
}

impl StableHash for WitPackage {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC0);
        self.unit.stable_hash(hasher);
        self.name.stable_hash(hasher);
        self.interfaces.stable_hash(hasher);
        self.worlds.stable_hash(hasher);
        self.declaration_order.stable_hash(hasher);
    }
}

impl StableHash for WitPackageName {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC1);
        self.namespace.stable_hash(hasher);
        self.name.stable_hash(hasher);
        stable_version(&self.version, hasher);
    }
}

impl StableHash for WitInterface {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC2);
        self.name.stable_hash(hasher);
        self.docs.stable_hash(hasher);
        stable_version(&self.since, hasher);
        stable_version(&self.deprecated, hasher);
        self.items.stable_hash(hasher);
        self.span.stable_hash(hasher);
    }
}

impl StableHash for WitItem {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC3);
        self.name.stable_hash(hasher);
        self.docs.stable_hash(hasher);
        stable_version(&self.since, hasher);
        stable_version(&self.deprecated, hasher);
        self.kind.stable_hash(hasher);
        self.unsupported.stable_hash(hasher);
        self.span.stable_hash(hasher);
    }
}

impl StableHash for WitItemKind {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC4);
        match self {
            Self::Record(fields) => {
                hasher.write_tag(0x01);
                fields.stable_hash(hasher);
            }
            Self::Variant(cases) => {
                hasher.write_tag(0x02);
                cases.stable_hash(hasher);
            }
            Self::Enum(cases) => {
                hasher.write_tag(0x03);
                cases.stable_hash(hasher);
            }
            Self::Flags(flags) => {
                hasher.write_tag(0x04);
                flags.stable_hash(hasher);
            }
            Self::Resource(members) => {
                hasher.write_tag(0x05);
                members.stable_hash(hasher);
            }
            Self::Alias(ty) => {
                hasher.write_tag(0x06);
                ty.stable_hash(hasher);
            }
            Self::Used(reference) => {
                hasher.write_tag(0x07);
                reference.stable_hash(hasher);
            }
            Self::Func(function) => {
                hasher.write_tag(0x08);
                function.stable_hash(hasher);
            }
        }
    }
}

stable_struct!(WitField, 0xC5, name, docs, ty);
stable_struct!(WitCase, 0xC6, name, docs, payload);

impl StableHash for WitMember {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC7);
        self.name.stable_hash(hasher);
        self.kind.stable_hash(hasher);
        self.func.stable_hash(hasher);
        self.docs.stable_hash(hasher);
        stable_version(&self.since, hasher);
        stable_version(&self.deprecated, hasher);
        self.unsupported.stable_hash(hasher);
    }
}

impl StableHash for WitMemberKind {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xC8);
        hasher.write_tag(match self {
            Self::Constructor => 0x01,
            Self::Method => 0x02,
            Self::Static => 0x03,
        });
    }
}

stable_struct!(WitFunc, 0xC9, params, result, is_async);

impl StableHash for WitType {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xCA);
        match self {
            Self::Bool => hasher.write_tag(0x01),
            Self::S8 => hasher.write_tag(0x02),
            Self::S16 => hasher.write_tag(0x03),
            Self::S32 => hasher.write_tag(0x04),
            Self::S64 => hasher.write_tag(0x05),
            Self::U8 => hasher.write_tag(0x06),
            Self::U16 => hasher.write_tag(0x07),
            Self::U32 => hasher.write_tag(0x08),
            Self::U64 => hasher.write_tag(0x09),
            Self::F32 => hasher.write_tag(0x0A),
            Self::F64 => hasher.write_tag(0x0B),
            Self::Char => hasher.write_tag(0x0C),
            Self::String => hasher.write_tag(0x0D),
            Self::List(inner) => {
                hasher.write_tag(0x0E);
                inner.as_ref().stable_hash(hasher);
            }
            Self::Option(inner) => {
                hasher.write_tag(0x0F);
                inner.as_ref().stable_hash(hasher);
            }
            Self::Result { ok, err } => {
                hasher.write_tag(0x10);
                ok.as_deref().stable_hash(hasher);
                err.as_deref().stable_hash(hasher);
            }
            Self::Tuple(types) => {
                hasher.write_tag(0x11);
                types.stable_hash(hasher);
            }
            Self::Named(reference) => {
                hasher.write_tag(0x12);
                reference.stable_hash(hasher);
            }
            Self::Own(reference) => {
                hasher.write_tag(0x13);
                reference.stable_hash(hasher);
            }
            Self::Borrow(reference) => {
                hasher.write_tag(0x14);
                reference.stable_hash(hasher);
            }
            Self::Future(inner) => {
                hasher.write_tag(0x15);
                inner.as_deref().stable_hash(hasher);
            }
            Self::Stream(inner) => {
                hasher.write_tag(0x16);
                inner.as_deref().stable_hash(hasher);
            }
            Self::Unsupported(value) => {
                hasher.write_tag(0x17);
                value.stable_hash(hasher);
            }
        }
    }
}

stable_struct!(WitTypeRef, 0xCB, interface, item, item_name);
stable_struct!(WitIfaceRef, 0xCC, unit, interface, name);

impl StableHash for WitWorldItemDirection {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD0);
        hasher.write_tag(match self {
            Self::Import => 0x01,
            Self::Export => 0x02,
            Self::Include => 0x03,
        });
    }
}

impl StableHash for WitWorldItemOrder {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD1);
        self.direction.stable_hash(hasher);
        self.index.stable_hash(hasher);
    }
}

impl StableHash for WitWorldRef {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD2);
        self.unit.stable_hash(hasher);
        self.world.stable_hash(hasher);
        self.name.stable_hash(hasher);
        hasher.write_len(u64::try_from(self.names.len()).unwrap_or(u64::MAX));
        for (name, alias) in &self.names {
            name.stable_hash(hasher);
            alias.stable_hash(hasher);
        }
        stable_version(&self.since, hasher);
        stable_version(&self.deprecated, hasher);
        self.span.stable_hash(hasher);
    }
}

impl StableHash for WitPackageDeclaration {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD3);
        match self {
            Self::Interface(index) => {
                hasher.write_tag(0x01);
                index.stable_hash(hasher);
            }
            Self::World(index) => {
                hasher.write_tag(0x02);
                index.stable_hash(hasher);
            }
        }
    }
}

impl StableHash for WitWorld {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xCD);
        self.name.stable_hash(hasher);
        self.docs.stable_hash(hasher);
        stable_version(&self.since, hasher);
        stable_version(&self.deprecated, hasher);
        self.imports.stable_hash(hasher);
        self.exports.stable_hash(hasher);
        self.includes.stable_hash(hasher);
        self.source_order.stable_hash(hasher);
        self.unsupported.stable_hash(hasher);
        self.span.stable_hash(hasher);
    }
}

impl StableHash for WitWorldItem {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xCE);
        match self {
            Self::Interface {
                reference,
                docs,
                since,
                deprecated,
                span,
            } => {
                hasher.write_tag(0x01);
                reference.stable_hash(hasher);
                docs.stable_hash(hasher);
                stable_version(since, hasher);
                stable_version(deprecated, hasher);
                span.stable_hash(hasher);
            }
            Self::Func {
                name,
                docs,
                func,
                unsupported,
                since,
                deprecated,
                span,
            } => {
                hasher.write_tag(0x02);
                name.stable_hash(hasher);
                docs.stable_hash(hasher);
                func.stable_hash(hasher);
                unsupported.stable_hash(hasher);
                stable_version(since, hasher);
                stable_version(deprecated, hasher);
                span.stable_hash(hasher);
            }
        }
    }
}

impl StableHash for WitSpan {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xCF);
        self.path.stable_hash(hasher);
        hasher.write_tag(0x01);
        hasher.write_u64(u64::try_from(self.range.start).unwrap_or(u64::MAX));
        hasher.write_u64(u64::try_from(self.range.end).unwrap_or(u64::MAX));
    }
}

impl StableHash for Unsupported {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD0);
        hasher.write_tag(match self {
            Self::ErrorContext => 0x01,
            Self::FixedList => 0x02,
            Self::Map => 0x03,
            Self::NamedResults => 0x04,
            Self::Async => 0x05,
            Self::Future => 0x06,
            Self::Stream => 0x07,
            Self::WorldType => 0x08,
            Self::InlineInterface => 0x09,
        });
    }
}
