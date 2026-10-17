//! Stable SAI-facing definitions for a resolved WIT package.
//!
//! Runtime package and symbol IDs are allocated by session-local interners.
//! Persistent identity is derived only from the unit's canonical key and the
//! resolved strings in each definition path.

use crate::{
    WitIfaceIdx, WitItem, WitItemIdx, WitItemKind, WitMemberKind, WitPackage, WitUnit, WitUnitKind,
    WitWorldIdx, WitWorldItem,
};
use sai_hir::{
    DefId, DefIndex, DefPath, DefPathData, DefPathError, DefPathHash, DefPathInterner,
    DefPathInternerError, DisambiguatedDefPathData, IndexVec, PackageId, Symbol,
};
use sai_intern::{InternError, Interner, LookupError, PackageIdError, PackageInterner};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fmt;
use std::sync::Arc;

/// The kind of entity represented by one definition in a WIT view.
///
/// This stays local to `sai_wit`; importing `sai_resolve::DefKind` here would
/// reverse the intended crate dependency.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WitViewDefKind {
    Interface,
    World,
    Struct,
    Enum,
    Resource,
    Alias,
    Fn,
    Field,
    Variant,
    AssocFn,
    WorldFn,
}

/// A stable model address for one definition in a WIT view.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WitViewEntity {
    Interface {
        interface: WitIfaceIdx,
    },
    World {
        world: WitWorldIdx,
    },
    Item {
        interface: WitIfaceIdx,
        item: WitItemIdx,
    },
    Field {
        interface: WitIfaceIdx,
        item: WitItemIdx,
        field: u32,
    },
    Variant {
        interface: WitIfaceIdx,
        item: WitItemIdx,
        case: u32,
    },
    ResourceMember {
        interface: WitIfaceIdx,
        item: WitItemIdx,
        member: u32,
    },
    FlagsMethod {
        interface: WitIfaceIdx,
        item: WitItemIdx,
        method: WitFlagsMethod,
    },
    WorldFunction {
        world: WitWorldIdx,
        import: bool,
        item: u32,
    },
}

/// The compiler-provided operations exposed by a flags type.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum WitFlagsMethod {
    Empty,
    All,
    Contains,
    IsEmpty,
}

impl WitFlagsMethod {
    fn name(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::All => "all",
            Self::Contains => "contains",
            Self::IsEmpty => "is_empty",
        }
    }
}

/// A view definition indexed by its stable `DefIndex`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitViewDef {
    pub entity: WitViewEntity,
    pub kind: WitViewDefKind,
    /// Persistent identity computed from canonical package and path strings.
    pub path_hash: DefPathHash,
    /// A readable, tagged serialization used for deterministic hash tie breaks.
    pub canonical_path: String,
}

/// A WIT item exposed in an interface's SAI member namespace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewMember {
    pub def: DefId,
    pub kind: WitViewDefKind,
    pub item: WitItemIdx,
    pub unsupported: Option<crate::Unsupported>,
}

/// An interface in the SAI view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewInterface {
    pub def: DefId,
    pub wit: WitIfaceIdx,
    pub sai_name: Symbol,
    /// SAI name to member. The canonical string key gives deterministic
    /// iteration; `Symbol` itself is session-local and intentionally has no
    /// ordering contract.
    pub members: BTreeMap<String, ViewMember>,
    pub shadowed: Vec<Shadowed>,
    /// Generated-text provenance is attached by the caller that owns the
    /// generated-WIT hook. A plain `WitPackage` does not carry that metadata.
    pub origin: Option<DefId>,
    /// Stable identity for `origin`, independent of session-local IDs.
    pub origin_identity: Option<WitViewOriginIdentity>,
}

/// Persistent identity for the source interface represented by generated WIT.
///
/// `path_hash` includes the source package's canonical key. The canonical path
/// is retained as a readable collision tie-breaker.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WitViewOriginIdentity {
    pub path_hash: DefPathHash,
    pub canonical_path: String,
}

/// A world in the SAI view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ViewWorld {
    pub def: DefId,
    pub wit: WitWorldIdx,
    pub sai_name: Symbol,
    /// Imported world functions, ordered by their canonical SAI names.
    pub import_fns: BTreeMap<String, DefId>,
    /// Exported world function names, ordered by their canonical SAI names.
    ///
    /// Export definitions remain in the view definition table; this set lets
    /// name resolution recognize the exported function namespace without
    /// constructing imported-function-style bindings for them.
    pub export_fns: BTreeSet<String>,
}

/// A WIT item that is hidden by another item with the same SAI name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Shadowed {
    pub wit_name: String,
    pub resource: Option<String>,
    pub winner: DefId,
}

/// The stable SAI view package of one successfully loaded WIT unit.
#[derive(Clone)]
pub struct WitView {
    pub unit: WitUnit,
    pub package: PackageId,
    pub interfaces: BTreeMap<String, ViewInterface>,
    pub worlds: BTreeMap<String, ViewWorld>,
    /// The current definitions indexed by the persistent path interner.
    ///
    /// `None` entries are stable indices of definitions removed by a later
    /// revision of this view.
    pub defs: IndexVec<DefIndex, Option<WitViewDef>>,
    definition_by_entity: HashMap<WitViewEntity, DefId>,
    /// The model is retained so entity addresses in `defs` remain meaningful.
    pub model: WitPackage,
    /// Owns the scope used by this view's `DefId`s.
    def_paths: Arc<DefPathInterner>,
}

impl fmt::Debug for WitView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WitView")
            .field("unit", &self.unit)
            .field("package", &self.package)
            .field("interfaces", &self.interfaces)
            .field("worlds", &self.worlds)
            .field("defs", &self.defs)
            .field("model", &self.model)
            .finish()
    }
}

impl PartialEq for WitView {
    fn eq(&self, other: &Self) -> bool {
        self.unit == other.unit
            && self.package == other.package
            && self.interfaces == other.interfaces
            && self.worlds == other.worlds
            && self.defs == other.defs
            && self.model == other.model
    }
}

impl Eq for WitView {}

impl WitView {
    /// Builds a view package with the caller-owned per-package path interner.
    ///
    /// The interner must be retained for the lifetime of the compiler session
    /// and reused for later builds of this unit. A new interner assigns indices
    /// in deterministic `DefPathHash` order; later additions append new paths
    /// while existing definition IDs remain stable.
    pub fn build(
        model: WitPackage,
        packages: &PackageInterner,
        symbols: &Interner,
        def_paths: Arc<DefPathInterner>,
    ) -> Result<Self, WitViewError> {
        Self::build_inner(model, packages, symbols, def_paths, None, &[])
    }

    /// Builds a view and attaches generated-interface provenance.
    ///
    /// `origins` maps WIT interface names to the source interface definitions
    /// that generated them. It is read only for `Generated` units.
    ///
    /// `source_def_paths` must be the retained path interner for the source
    /// package, not the generated view package. It validates origin scope and
    /// kind and provides the stable identity for each source definition.
    pub fn build_with_origins(
        model: WitPackage,
        packages: &PackageInterner,
        symbols: &Interner,
        def_paths: Arc<DefPathInterner>,
        source_def_paths: &DefPathInterner,
        origins: &[(String, DefId)],
    ) -> Result<Self, WitViewError> {
        Self::build_inner(
            model,
            packages,
            symbols,
            def_paths,
            Some(source_def_paths),
            origins,
        )
    }

    fn build_inner(
        model: WitPackage,
        packages: &PackageInterner,
        symbols: &Interner,
        def_paths: Arc<DefPathInterner>,
        source_def_paths: Option<&DefPathInterner>,
        origins: &[(String, DefId)],
    ) -> Result<Self, WitViewError> {
        let unit = model.unit.clone();
        let actual_name = model
            .name
            .version
            .as_ref()
            .map(|version| format!("{}:{}@{version}", model.name.namespace, model.name.name))
            .unwrap_or_else(|| format!("{}:{}", model.name.namespace, model.name.name));
        if actual_name != unit.canonical_name() {
            return Err(WitViewError::PackageNameMismatch {
                expected: unit.canonical_name().to_owned(),
                actual: actual_name,
            });
        }
        let package = packages
            .intern(unit.stable_key())
            .map_err(WitViewError::PackageId)?;
        if def_paths.package() != package {
            return Err(WitViewError::DefPathPackageMismatch {
                expected: package,
                actual: def_paths.package(),
            });
        }
        let generated_origins = validate_generated_origins(
            &unit,
            &model,
            packages,
            symbols,
            source_def_paths,
            origins,
        )?;

        let mut interface_plans = make_interface_plans(&model, symbols)?;
        let mut candidates = Vec::new();
        collect_interface_candidates(
            &model,
            &interface_plans,
            package,
            packages,
            symbols,
            &mut candidates,
        )?;
        let world_plans =
            collect_world_candidates(&model, package, packages, symbols, &mut candidates)?;

        candidates.sort_by(|left, right| {
            left.path_hash
                .cmp(&right.path_hash)
                .then_with(|| left.canonical_path.cmp(&right.canonical_path))
        });

        let mut seen_paths = HashSet::with_capacity(candidates.len());
        let mut seen_entities = HashSet::with_capacity(candidates.len());
        for candidate in &candidates {
            if !seen_paths.insert(candidate.path.clone()) {
                return Err(WitViewError::DuplicateDefinitionPath(
                    candidate.canonical_path.clone(),
                ));
            }
            if !seen_entities.insert(candidate.entity) {
                return Err(WitViewError::DuplicateDefinitionEntity(candidate.entity));
            }
        }
        let local_defs = def_paths
            .intern_ordered_paths(
                candidates
                    .iter()
                    .map(|candidate| candidate.path.clone())
                    .collect(),
            )
            .map_err(WitViewError::DefPathInterner)?;

        let mut entity_defs = HashMap::with_capacity(candidates.len());
        let mut current_defs = Vec::with_capacity(candidates.len());
        for (candidate, local) in candidates.into_iter().zip(local_defs) {
            let definition = WitViewDef {
                entity: candidate.entity,
                kind: candidate.kind,
                path_hash: candidate.path_hash,
                canonical_path: candidate.canonical_path,
            };
            if entity_defs
                .insert(candidate.entity, local.to_def_id())
                .is_some()
            {
                return Err(WitViewError::DuplicateDefinitionIndex(local.index()));
            }
            current_defs.push((local.index(), definition));
        }
        let def_count = def_paths.len().map_err(WitViewError::DefPathInterner)?;
        let mut defs = IndexVec::with_capacity(def_count);
        for _ in 0..def_count {
            defs.push(None)
                .map_err(|_| WitViewError::TooManyDefinitions)?;
        }
        for (index, definition) in current_defs {
            let slot = defs
                .get_mut(index)
                .ok_or(WitViewError::DefinitionIndexOutOfBounds(index))?;
            if slot.replace(definition).is_some() {
                return Err(WitViewError::DuplicateDefinitionIndex(index));
            }
        }

        let mut interfaces = BTreeMap::new();
        for plan in interface_plans.drain(..) {
            let def = lookup_entity(
                &entity_defs,
                WitViewEntity::Interface {
                    interface: plan.wit,
                },
            )?;
            let mut members = BTreeMap::new();
            for item in &plan.items {
                let entity = WitViewEntity::Item {
                    interface: plan.wit,
                    item: item.wit,
                };
                let item_def = lookup_entity(&entity_defs, entity)?;
                members.insert(
                    item.sai_name.clone(),
                    ViewMember {
                        def: item_def,
                        kind: item.kind,
                        item: item.wit,
                        unsupported: model.interfaces[plan.wit].items[item.wit].unsupported,
                    },
                );
            }
            let mut shadowed = plan
                .shadowed
                .into_iter()
                .map(|pending| {
                    let winner = lookup_entity(&entity_defs, pending.winner)?;
                    Ok((
                        pending.order,
                        Shadowed {
                            wit_name: pending.wit_name,
                            resource: pending.resource,
                            winner,
                        },
                    ))
                })
                .collect::<Result<Vec<_>, WitViewError>>()?;
            shadowed.sort_by_key(|(order, _)| *order);
            let shadowed = shadowed.into_iter().map(|(_, shadow)| shadow).collect();
            let origin = generated_origins.get(&plan.wit_name);
            insert_unique(
                &mut interfaces,
                plan.wit_name,
                ViewInterface {
                    def,
                    wit: plan.wit,
                    sai_name: plan.sai_name,
                    members,
                    shadowed,
                    origin: origin.map(|origin| origin.def),
                    origin_identity: origin.map(|origin| origin.identity.clone()),
                },
            )?;
        }

        let mut worlds = BTreeMap::new();
        for plan in world_plans {
            let def = lookup_entity(&entity_defs, WitViewEntity::World { world: plan.wit })?;
            let mut import_fns = BTreeMap::new();
            for (name, item) in plan.import_functions {
                let def = lookup_entity(
                    &entity_defs,
                    WitViewEntity::WorldFunction {
                        world: plan.wit,
                        import: true,
                        item,
                    },
                )?;
                if import_fns.insert(name.clone(), def).is_some() {
                    return Err(WitViewError::DuplicateWorldImportFunction {
                        world: plan.wit_name.clone(),
                        name,
                    });
                }
            }
            insert_unique(
                &mut worlds,
                plan.wit_name,
                ViewWorld {
                    def,
                    wit: plan.wit,
                    sai_name: plan.sai_name,
                    import_fns,
                    export_fns: plan.export_fns,
                },
            )?;
        }

        Ok(Self {
            unit,
            package,
            interfaces,
            worlds,
            defs,
            definition_by_entity: entity_defs,
            model,
            def_paths,
        })
    }

    /// Returns the exact scoped `DefId` for one view entity.
    ///
    /// Reconstructing a foreign ID from its package and index loses the
    /// definition-path scope carried by the view, so adapters that translate
    /// model addresses must use this lookup.
    pub fn definition_for_entity(&self, entity: WitViewEntity) -> Option<DefId> {
        self.definition_by_entity.get(&entity).copied()
    }

    /// Returns a view definition by a runtime `DefId`.
    pub fn definition(&self, id: DefId) -> Option<&WitViewDef> {
        self.def_paths
            .resolve_def_id(id)
            .ok()
            .flatten()
            .and_then(|_| self.defs.get(id.index).and_then(Option::as_ref))
    }

    /// Resolves the scoped definition path associated with a view `DefId`.
    pub fn def_path(&self, id: DefId) -> Result<Option<DefPath>, DefPathInternerError> {
        self.def_paths.resolve_def_id(id)
    }

    /// Returns this view's path interner for queries that need path lookup.
    pub fn def_path_interner(&self) -> &DefPathInterner {
        &self.def_paths
    }

    /// Returns the canonical package key used for the view package.
    pub fn package_key(&self, packages: &PackageInterner) -> Result<String, WitViewError> {
        packages
            .resolve(self.package)
            .map(|key| key.to_string())
            .map_err(WitViewError::PackageId)
    }
}

fn validate_generated_origins(
    unit: &WitUnit,
    model: &WitPackage,
    packages: &PackageInterner,
    symbols: &Interner,
    source_def_paths: Option<&DefPathInterner>,
    origins: &[(String, DefId)],
) -> Result<HashMap<String, ValidatedGeneratedOrigin>, WitViewError> {
    let WitUnitKind::Generated { package } = unit.kind() else {
        return Ok(HashMap::new());
    };

    if let Some(source_def_paths) = source_def_paths {
        if source_def_paths.package() != package {
            return Err(WitViewError::GeneratedOriginInternerPackageMismatch {
                expected: package,
                actual: source_def_paths.package(),
            });
        }
    } else if !origins.is_empty() {
        return Err(WitViewError::GeneratedOriginInternerRequired);
    }

    let mut generated_origins = HashMap::with_capacity(origins.len());
    let mut source_definitions = HashSet::with_capacity(origins.len());
    for (name, def) in origins {
        if generated_origins.contains_key(name) {
            return Err(WitViewError::DuplicateGeneratedOrigin(name.clone()));
        }
        if def.package != package {
            return Err(WitViewError::GeneratedOriginPackageMismatch {
                interface: name.clone(),
                expected: package,
                actual: def.package,
            });
        }
        let source_def_paths =
            source_def_paths.ok_or(WitViewError::GeneratedOriginInternerRequired)?;
        let path = source_def_paths
            .resolve_def_id(*def)
            .map_err(|error| WitViewError::GeneratedOriginPath {
                interface: name.clone(),
                error,
            })?
            .ok_or_else(|| WitViewError::GeneratedOriginDefinitionNotFound {
                interface: name.clone(),
                def: *def,
            })?;
        if !matches!(
            path.data().last().map(|component| &component.data),
            Some(DefPathData::Interface(_))
        ) {
            return Err(WitViewError::GeneratedOriginNotInterface {
                interface: name.clone(),
                def: *def,
            });
        }
        let identity = WitViewOriginIdentity {
            path_hash: DefPathHash::compute(&path, packages, symbols)
                .map_err(WitViewError::DefPath)?,
            canonical_path: canonical_path(path.data(), symbols)?,
        };
        if !source_definitions.insert(*def) {
            return Err(WitViewError::DuplicateGeneratedOriginDefinition(*def));
        }
        generated_origins.insert(
            name.clone(),
            ValidatedGeneratedOrigin {
                def: *def,
                identity,
            },
        );
    }
    let interface_names = model
        .interfaces
        .iter()
        .map(|interface| interface.name.as_str())
        .collect::<std::collections::HashSet<_>>();
    if let Some((name, _)) = origins
        .iter()
        .find(|(name, _)| !interface_names.contains(name.as_str()))
    {
        return Err(WitViewError::UnknownGeneratedOrigin(name.clone()));
    }
    for interface in model.interfaces.iter() {
        if !generated_origins.contains_key(&interface.name) {
            return Err(WitViewError::MissingGeneratedOrigin(interface.name.clone()));
        }
    }
    Ok(generated_origins)
}

#[derive(Clone)]
struct ValidatedGeneratedOrigin {
    def: DefId,
    identity: WitViewOriginIdentity,
}

/// Returns the SAI source spelling for a WIT identifier.
///
/// This helper is public so the resolver and printer can share exactly the
/// same WIT-to-SAI conversion without depending on `sai_resolve`.
pub fn wit_name_to_sai(name: &str, upper: bool) -> Result<String, WitViewError> {
    let name = name.strip_prefix('%').unwrap_or(name);
    if name.is_empty() {
        return Err(WitViewError::InvalidName(name.to_owned()));
    }

    let mut words = Vec::new();
    for word in name.split('-') {
        let bytes = word.as_bytes();
        let valid_lower_word = bytes.first().is_some_and(u8::is_ascii_lowercase)
            && bytes[1..]
                .iter()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
        let valid_upper_word = bytes.first().is_some_and(u8::is_ascii_uppercase)
            && bytes[1..]
                .iter()
                .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit());
        if !valid_lower_word && !valid_upper_word {
            return Err(WitViewError::InvalidName(name.to_owned()));
        }
        let normalized = if word.bytes().any(|byte| byte.is_ascii_alphabetic())
            && word
                .bytes()
                .filter(u8::is_ascii_alphabetic)
                .all(|byte| byte.is_ascii_uppercase())
        {
            word.to_ascii_lowercase()
        } else {
            word.to_owned()
        };
        words.push(normalized);
    }

    let mut converted = if upper {
        let mut output = String::new();
        for word in words {
            let mut chars = word.chars();
            if let Some(first) = chars.next() {
                output.extend(first.to_uppercase());
                output.extend(chars);
            }
        }
        output
    } else {
        words.join("_")
    };
    if is_sai_keyword_or_reserved(&converted) {
        converted.insert_str(0, "r#");
    }
    Ok(converted)
}

/// Returns the semantic identifier name, without its raw-identifier prefix.
fn wit_name_to_sai_name(name: &str, upper: bool) -> Result<String, WitViewError> {
    let spelling = wit_name_to_sai(name, upper)?;
    Ok(spelling.strip_prefix("r#").unwrap_or(&spelling).to_owned())
}

fn is_sai_keyword_or_reserved(name: &str) -> bool {
    matches!(
        name,
        "and"
            | "as"
            | "assert"
            | "async"
            | "await"
            | "borrow"
            | "break"
            | "const"
            | "continue"
            | "else"
            | "enum"
            | "export"
            | "extern"
            | "false"
            | "fn"
            | "for"
            | "if"
            | "impl"
            | "in"
            | "let"
            | "loop"
            | "match"
            | "move"
            | "mut"
            | "not"
            | "null"
            | "nil"
            | "undefined"
            | "or"
            | "owned"
            | "pub"
            | "return"
            | "self"
            | "Self"
            | "trait"
            | "true"
            | "try"
            | "catch"
            | "throw"
            | "type"
            | "unsafe"
            | "use"
            | "var"
            | "where"
            | "while"
            | "actor"
            | "class"
            | "defer"
            | "dyn"
            | "macro"
            | "static"
            | "yield"
    )
}

/// Failures while creating a stable package view.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WitViewError {
    PackageId(PackageIdError),
    Symbol(InternError),
    Lookup(LookupError),
    DefPath(DefPathError),
    DefPathInterner(DefPathInternerError),
    DefPathPackageMismatch {
        expected: PackageId,
        actual: PackageId,
    },
    PackageNameMismatch {
        expected: String,
        actual: String,
    },
    GeneratedOriginPackageMismatch {
        interface: String,
        expected: PackageId,
        actual: PackageId,
    },
    GeneratedOriginInternerRequired,
    GeneratedOriginInternerPackageMismatch {
        expected: PackageId,
        actual: PackageId,
    },
    GeneratedOriginPath {
        interface: String,
        error: DefPathInternerError,
    },
    GeneratedOriginDefinitionNotFound {
        interface: String,
        def: DefId,
    },
    GeneratedOriginNotInterface {
        interface: String,
        def: DefId,
    },
    DuplicateGeneratedOrigin(String),
    DuplicateGeneratedOriginDefinition(DefId),
    MissingGeneratedOrigin(String),
    UnknownGeneratedOrigin(String),
    InvalidName(String),
    DuplicateViewName {
        kind: &'static str,
        name: String,
    },
    DuplicateWorldImportFunction {
        world: String,
        name: String,
    },
    MissingDefinition(WitViewEntity),
    TooManyDefinitions,
    DefinitionIndexOutOfBounds(DefIndex),
    DuplicateDefinitionIndex(DefIndex),
    DuplicateDefinitionPath(String),
    DuplicateDefinitionEntity(WitViewEntity),
}

impl fmt::Display for WitViewError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PackageId(error) => {
                write!(formatter, "could not intern WIT view package: {error:?}")
            }
            Self::Symbol(error) => write!(formatter, "could not intern WIT view name: {error:?}"),
            Self::Lookup(error) => write!(formatter, "could not resolve WIT view name: {error:?}"),
            Self::DefPath(error) => {
                write!(formatter, "could not hash WIT definition path: {error:?}")
            }
            Self::DefPathInterner(error) => {
                write!(formatter, "could not intern WIT definition path: {error:?}")
            }
            Self::DefPathPackageMismatch { expected, actual } => write!(
                formatter,
                "WIT definition interner belongs to package {actual:?}, expected {expected:?}"
            ),
            Self::PackageNameMismatch { expected, actual } => {
                write!(
                    formatter,
                    "WIT view package is `{actual}`, expected `{expected}`"
                )
            }
            Self::GeneratedOriginPackageMismatch {
                interface,
                expected,
                actual,
            } => write!(
                formatter,
                "generated WIT interface `{interface}` belongs to package {expected:?}, but its origin belongs to {actual:?}"
            ),
            Self::GeneratedOriginInternerRequired => formatter.write_str(
                "generated WIT origins require the source package's definition-path interner",
            ),
            Self::GeneratedOriginInternerPackageMismatch { expected, actual } => write!(
                formatter,
                "generated WIT origin interner belongs to package {actual:?}, expected source package {expected:?}"
            ),
            Self::GeneratedOriginPath { interface, error } => write!(
                formatter,
                "could not resolve generated WIT origin for interface `{interface}`: {error:?}"
            ),
            Self::GeneratedOriginDefinitionNotFound { interface, def } => write!(
                formatter,
                "generated WIT origin for interface `{interface}` does not exist: {def:?}"
            ),
            Self::GeneratedOriginNotInterface { interface, def } => write!(
                formatter,
                "generated WIT origin for interface `{interface}` is not an interface definition: {def:?}"
            ),
            Self::DuplicateGeneratedOrigin(name) => {
                write!(
                    formatter,
                    "duplicate generated WIT origin for interface `{name}`"
                )
            }
            Self::DuplicateGeneratedOriginDefinition(def) => write!(
                formatter,
                "generated WIT interfaces have duplicate source origin {def:?}"
            ),
            Self::MissingGeneratedOrigin(name) => {
                write!(
                    formatter,
                    "generated WIT interface `{name}` has no source origin"
                )
            }
            Self::UnknownGeneratedOrigin(name) => {
                write!(
                    formatter,
                    "generated WIT origin names unknown interface `{name}`"
                )
            }
            Self::InvalidName(name) => write!(formatter, "invalid WIT identifier `{name}`"),
            Self::DuplicateViewName { kind, name } => {
                write!(formatter, "duplicate WIT {kind} name `{name}` in view")
            }
            Self::DuplicateWorldImportFunction { world, name } => write!(
                formatter,
                "duplicate WIT world import function `{name}` in world `{world}`"
            ),
            Self::MissingDefinition(entity) => {
                write!(formatter, "missing WIT view definition for {entity:?}")
            }
            Self::TooManyDefinitions => formatter.write_str("WIT view has too many definitions"),
            Self::DefinitionIndexOutOfBounds(index) => write!(
                formatter,
                "WIT view definition index {index:?} is outside its path interner"
            ),
            Self::DuplicateDefinitionIndex(index) => write!(
                formatter,
                "multiple WIT view definitions use index {index:?}"
            ),
            Self::DuplicateDefinitionPath(path) => {
                write!(formatter, "multiple WIT view definitions use path `{path}`")
            }
            Self::DuplicateDefinitionEntity(entity) => {
                write!(
                    formatter,
                    "WIT view entity {entity:?} was collected more than once"
                )
            }
        }
    }
}

impl std::error::Error for WitViewError {}

struct Candidate {
    path: DefPath,
    path_hash: DefPathHash,
    canonical_path: String,
    entity: WitViewEntity,
    kind: WitViewDefKind,
}

struct InterfacePlan {
    wit: WitIfaceIdx,
    wit_name: String,
    sai_name: Symbol,
    items: Vec<ItemPlan>,
    shadowed: Vec<PendingShadow>,
}

struct ItemPlan {
    wit: WitItemIdx,
    sai_name: String,
    kind: WitViewDefKind,
    resource_members: Vec<MemberPlan>,
}

struct MemberPlan {
    wit: u32,
    sai_name: String,
}

struct PendingShadow {
    wit_name: String,
    resource: Option<String>,
    winner: WitViewEntity,
    order: (u32, u32),
}

struct WorldPlan {
    wit: WitWorldIdx,
    wit_name: String,
    sai_name: Symbol,
    import_functions: Vec<(String, u32)>,
    export_fns: BTreeSet<String>,
}

fn make_interface_plans(
    model: &WitPackage,
    symbols: &Interner,
) -> Result<Vec<InterfacePlan>, WitViewError> {
    let mut plans = Vec::with_capacity(model.interfaces.len());
    let mut package_interface_names = HashMap::new();

    for (wit, interface) in model.interfaces.iter_enumerated() {
        let sai_name = wit_name_to_sai_name(&interface.name, false)?;
        let symbol = symbols.intern(&sai_name).map_err(WitViewError::Symbol)?;
        if package_interface_names
            .insert(sai_name.clone(), interface.name.clone())
            .is_some()
        {
            return Err(WitViewError::DuplicateViewName {
                kind: "interface",
                name: interface.name.clone(),
            });
        }

        let mut selected = Vec::new();
        let mut first_items = HashMap::<String, WitItemIdx>::new();
        let mut shadowed = Vec::new();
        for (item_idx, item) in interface.items.iter_enumerated() {
            let item_sai_name = item_name(item)?;
            if let Some(winner) = first_items.get(&item_sai_name).copied() {
                let winner_entity = WitViewEntity::Item {
                    interface: wit,
                    item: winner,
                };
                shadowed.push(PendingShadow {
                    wit_name: item.name.clone(),
                    resource: None,
                    winner: winner_entity,
                    order: (item_idx.raw(), 0),
                });
                continue;
            }
            first_items.insert(item_sai_name.clone(), item_idx);

            let kind = item_view_kind(&item.kind);
            let resource_members = if let WitItemKind::Resource(members) = &item.kind {
                plan_resource_members(wit, item_idx, item, members, &mut shadowed)?
            } else {
                Vec::new()
            };
            selected.push(ItemPlan {
                wit: item_idx,
                sai_name: item_sai_name,
                kind,
                resource_members,
            });
        }
        plans.push(InterfacePlan {
            wit,
            wit_name: interface.name.clone(),
            sai_name: symbol,
            items: selected,
            shadowed,
        });
    }

    Ok(plans)
}

fn plan_resource_members(
    interface: WitIfaceIdx,
    item_idx: WitItemIdx,
    item: &WitItem,
    members: &[crate::WitMember],
    shadowed: &mut Vec<PendingShadow>,
) -> Result<Vec<MemberPlan>, WitViewError> {
    let mut winners = HashMap::<String, usize>::new();
    for (index, member) in members.iter().enumerate() {
        let name = if member.kind == WitMemberKind::Constructor {
            "new".to_owned()
        } else {
            wit_name_to_sai_name(&member.name, false)?
        };
        let Some(previous) = winners.get(&name).copied() else {
            winners.insert(name, index);
            continue;
        };
        let previous_is_constructor = members[previous].kind == WitMemberKind::Constructor;
        if member.kind == WitMemberKind::Constructor && !previous_is_constructor {
            winners.insert(name, index);
        }
    }

    let mut plans = Vec::new();
    for (index, member) in members.iter().enumerate() {
        let name = if member.kind == WitMemberKind::Constructor {
            "new".to_owned()
        } else {
            wit_name_to_sai_name(&member.name, false)?
        };
        let winner_index = winners[&name];
        if winner_index == index {
            plans.push(MemberPlan {
                wit: u32::try_from(index).map_err(|_| WitViewError::TooManyDefinitions)?,
                sai_name: name,
            });
            continue;
        }
        let winner_entity = WitViewEntity::ResourceMember {
            interface,
            item: item_idx,
            member: u32::try_from(winner_index).map_err(|_| WitViewError::TooManyDefinitions)?,
        };
        shadowed.push(PendingShadow {
            wit_name: member.name.clone(),
            resource: Some(item.name.clone()),
            winner: winner_entity,
            order: (
                item_idx.raw(),
                u32::try_from(index)
                    .map_err(|_| WitViewError::TooManyDefinitions)?
                    .saturating_add(1),
            ),
        });
    }
    Ok(plans)
}

fn item_name(item: &WitItem) -> Result<String, WitViewError> {
    wit_name_to_sai_name(&item.name, !matches!(&item.kind, WitItemKind::Func(_)))
}

fn item_view_kind(kind: &WitItemKind) -> WitViewDefKind {
    match kind {
        WitItemKind::Record(_) => WitViewDefKind::Struct,
        WitItemKind::Variant(_) | WitItemKind::Enum(_) | WitItemKind::Flags(_) => {
            WitViewDefKind::Enum
        }
        WitItemKind::Resource(_) => WitViewDefKind::Resource,
        WitItemKind::Alias(_) | WitItemKind::Used(_) => WitViewDefKind::Alias,
        WitItemKind::Func(_) => WitViewDefKind::Fn,
    }
}

fn collect_interface_candidates(
    model: &WitPackage,
    plans: &[InterfacePlan],
    package: PackageId,
    packages: &PackageInterner,
    symbols: &Interner,
    candidates: &mut Vec<Candidate>,
) -> Result<(), WitViewError> {
    let mut interface_components = HashMap::new();
    for plan in plans {
        let interface_data = DefPathData::Interface(plan.sai_name);
        let interface_disambiguator =
            next_disambiguator(&mut interface_components, &interface_data)?;
        let interface_path = vec![DisambiguatedDefPathData {
            data: interface_data,
            disambiguator: interface_disambiguator,
        }];
        add_candidate(
            candidates,
            package,
            packages,
            symbols,
            &interface_path,
            WitViewEntity::Interface {
                interface: plan.wit,
            },
            WitViewDefKind::Interface,
        )?;

        let interface = &model.interfaces[plan.wit];
        let mut item_components = HashMap::new();
        for item_plan in &plan.items {
            let item = &interface.items[item_plan.wit];
            let data = item_component(item, symbols)?;
            let disambiguator = next_disambiguator(&mut item_components, &data)?;
            let mut item_path = interface_path.clone();
            item_path.push(DisambiguatedDefPathData {
                data,
                disambiguator,
            });
            add_candidate(
                candidates,
                package,
                packages,
                symbols,
                &item_path,
                WitViewEntity::Item {
                    interface: plan.wit,
                    item: item_plan.wit,
                },
                item_plan.kind,
            )?;

            match &item.kind {
                WitItemKind::Record(fields) => {
                    let mut field_components = HashMap::new();
                    for (field, field_data) in fields.iter().enumerate() {
                        let name = wit_name_to_sai_name(&field_data.name, false)?;
                        let symbol = symbols.intern(&name).map_err(WitViewError::Symbol)?;
                        let data = DefPathData::Field(symbol);
                        let disambiguator = next_disambiguator(&mut field_components, &data)?;
                        let mut path = item_path.clone();
                        path.push(DisambiguatedDefPathData {
                            data,
                            disambiguator,
                        });
                        add_candidate(
                            candidates,
                            package,
                            packages,
                            symbols,
                            &path,
                            WitViewEntity::Field {
                                interface: plan.wit,
                                item: item_plan.wit,
                                field: u32::try_from(field)
                                    .map_err(|_| WitViewError::TooManyDefinitions)?,
                            },
                            WitViewDefKind::Field,
                        )?;
                    }
                }
                WitItemKind::Variant(cases) => {
                    collect_case_candidates(
                        candidates,
                        package,
                        packages,
                        symbols,
                        plan.wit,
                        item_plan.wit,
                        &item_path,
                        cases.iter().map(|case| case.name.as_str()),
                    )?;
                }
                WitItemKind::Enum(cases) => {
                    collect_case_candidates(
                        candidates,
                        package,
                        packages,
                        symbols,
                        plan.wit,
                        item_plan.wit,
                        &item_path,
                        cases.iter().map(String::as_str),
                    )?;
                }
                WitItemKind::Flags(_) => {
                    for method in [
                        WitFlagsMethod::Empty,
                        WitFlagsMethod::All,
                        WitFlagsMethod::Contains,
                        WitFlagsMethod::IsEmpty,
                    ] {
                        let symbol = symbols
                            .intern(method.name())
                            .map_err(WitViewError::Symbol)?;
                        let data = DefPathData::AssocFn(symbol);
                        let mut path = item_path.clone();
                        path.push(DisambiguatedDefPathData {
                            data,
                            disambiguator: 0,
                        });
                        add_candidate(
                            candidates,
                            package,
                            packages,
                            symbols,
                            &path,
                            WitViewEntity::FlagsMethod {
                                interface: plan.wit,
                                item: item_plan.wit,
                                method,
                            },
                            WitViewDefKind::AssocFn,
                        )?;
                    }
                }
                WitItemKind::Resource(_) => {
                    let mut member_components = HashMap::new();
                    for member_plan in &item_plan.resource_members {
                        let symbol = symbols
                            .intern(&member_plan.sai_name)
                            .map_err(WitViewError::Symbol)?;
                        let data = DefPathData::AssocFn(symbol);
                        let disambiguator = next_disambiguator(&mut member_components, &data)?;
                        let mut path = item_path.clone();
                        path.push(DisambiguatedDefPathData {
                            data,
                            disambiguator,
                        });
                        add_candidate(
                            candidates,
                            package,
                            packages,
                            symbols,
                            &path,
                            WitViewEntity::ResourceMember {
                                interface: plan.wit,
                                item: item_plan.wit,
                                member: member_plan.wit,
                            },
                            WitViewDefKind::AssocFn,
                        )?;
                    }
                }
                WitItemKind::Alias(_) | WitItemKind::Used(_) | WitItemKind::Func(_) => {}
            }
        }
    }
    Ok(())
}

fn collect_case_candidates<'a>(
    candidates: &mut Vec<Candidate>,
    package: PackageId,
    packages: &PackageInterner,
    symbols: &Interner,
    interface: WitIfaceIdx,
    item: WitItemIdx,
    parent: &[DisambiguatedDefPathData],
    names: impl Iterator<Item = &'a str>,
) -> Result<(), WitViewError> {
    let mut components = HashMap::new();
    for (case, name) in names.enumerate() {
        let name = wit_name_to_sai_name(name, true)?;
        let symbol = symbols.intern(&name).map_err(WitViewError::Symbol)?;
        let data = DefPathData::Variant(symbol);
        let disambiguator = next_disambiguator(&mut components, &data)?;
        let mut path = parent.to_vec();
        path.push(DisambiguatedDefPathData {
            data,
            disambiguator,
        });
        add_candidate(
            candidates,
            package,
            packages,
            symbols,
            &path,
            WitViewEntity::Variant {
                interface,
                item,
                case: u32::try_from(case).map_err(|_| WitViewError::TooManyDefinitions)?,
            },
            WitViewDefKind::Variant,
        )?;
    }
    Ok(())
}

fn collect_world_candidates(
    model: &WitPackage,
    package: PackageId,
    packages: &PackageInterner,
    symbols: &Interner,
    candidates: &mut Vec<Candidate>,
) -> Result<Vec<WorldPlan>, WitViewError> {
    let mut plans = Vec::with_capacity(model.worlds.len());
    let mut world_names = HashMap::new();
    let mut world_components = HashMap::new();

    for (wit, world) in model.worlds.iter_enumerated() {
        let name = wit_name_to_sai_name(&world.name, false)?;
        let symbol = symbols.intern(&name).map_err(WitViewError::Symbol)?;
        if world_names
            .insert(name.clone(), world.name.clone())
            .is_some()
        {
            return Err(WitViewError::DuplicateViewName {
                kind: "world",
                name: world.name.clone(),
            });
        }
        let data = DefPathData::World(symbol);
        let disambiguator = next_disambiguator(&mut world_components, &data)?;
        let root = vec![DisambiguatedDefPathData {
            data,
            disambiguator,
        }];
        add_candidate(
            candidates,
            package,
            packages,
            symbols,
            &root,
            WitViewEntity::World { world: wit },
            WitViewDefKind::World,
        )?;

        let mut function_components = HashMap::new();
        let mut import_function_names = HashSet::new();
        let mut import_functions = Vec::new();
        let mut export_fns = BTreeSet::new();
        collect_world_function_candidates(
            &world.imports,
            true,
            wit,
            &world.name,
            &root,
            package,
            packages,
            symbols,
            &mut function_components,
            &mut import_function_names,
            &mut import_functions,
            &mut export_fns,
            candidates,
        )?;
        let mut export_function_names = HashSet::new();
        collect_world_function_candidates(
            &world.exports,
            false,
            wit,
            &world.name,
            &root,
            package,
            packages,
            symbols,
            &mut function_components,
            &mut export_function_names,
            &mut Vec::new(),
            &mut export_fns,
            candidates,
        )?;
        plans.push(WorldPlan {
            wit,
            wit_name: world.name.clone(),
            sai_name: symbol,
            import_functions,
            export_fns,
        });
    }
    Ok(plans)
}

#[allow(clippy::too_many_arguments)]
fn collect_world_function_candidates(
    items: &[WitWorldItem],
    import: bool,
    world: WitWorldIdx,
    world_name: &str,
    root: &[DisambiguatedDefPathData],
    package: PackageId,
    packages: &PackageInterner,
    symbols: &Interner,
    components: &mut HashMap<DefPathData, u32>,
    seen_names: &mut HashSet<String>,
    import_functions: &mut Vec<(String, u32)>,
    export_functions: &mut BTreeSet<String>,
    candidates: &mut Vec<Candidate>,
) -> Result<(), WitViewError> {
    for (ordinal, item) in items.iter().enumerate() {
        let WitWorldItem::Func { name, .. } = item else {
            continue;
        };
        let sai_name = wit_name_to_sai_name(name, false)?;
        if import && !seen_names.insert(sai_name.clone()) {
            return Err(WitViewError::DuplicateWorldImportFunction {
                world: world_name.to_owned(),
                name: sai_name,
            });
        }
        let symbol = symbols.intern(&sai_name).map_err(WitViewError::Symbol)?;
        let data = DefPathData::WorldItem {
            import,
            name: symbol,
        };
        let disambiguator = next_disambiguator(components, &data)?;
        let mut path = root.to_vec();
        path.push(DisambiguatedDefPathData {
            data,
            disambiguator,
        });
        let ordinal = u32::try_from(ordinal).map_err(|_| WitViewError::TooManyDefinitions)?;
        add_candidate(
            candidates,
            package,
            packages,
            symbols,
            &path,
            WitViewEntity::WorldFunction {
                world,
                import,
                item: ordinal,
            },
            WitViewDefKind::WorldFn,
        )?;
        if import {
            import_functions.push((sai_name, ordinal));
        } else {
            export_functions.insert(sai_name);
        }
    }
    Ok(())
}

fn item_component(item: &WitItem, symbols: &Interner) -> Result<DefPathData, WitViewError> {
    let name = item_name(item)?;
    let symbol = symbols.intern(&name).map_err(WitViewError::Symbol)?;
    Ok(if matches!(&item.kind, WitItemKind::Func(_)) {
        DefPathData::ValueNs(symbol)
    } else {
        DefPathData::TypeNs(symbol)
    })
}

fn add_candidate(
    candidates: &mut Vec<Candidate>,
    package: PackageId,
    packages: &PackageInterner,
    symbols: &Interner,
    data: &[DisambiguatedDefPathData],
    entity: WitViewEntity,
    kind: WitViewDefKind,
) -> Result<(), WitViewError> {
    let path =
        DefPath::new(package, data.to_vec(), packages, symbols).map_err(WitViewError::DefPath)?;
    let path_hash =
        DefPathHash::compute(&path, packages, symbols).map_err(WitViewError::DefPath)?;
    let canonical_path = canonical_path(data, symbols)?;
    candidates.push(Candidate {
        path,
        path_hash,
        canonical_path,
        entity,
        kind,
    });
    Ok(())
}

fn canonical_path(
    data: &[DisambiguatedDefPathData],
    symbols: &Interner,
) -> Result<String, WitViewError> {
    let mut output = String::new();
    for component in data {
        output.push_str(&canonical_path_component(component, symbols)?);
    }
    Ok(output)
}

fn canonical_path_component(
    component: &DisambiguatedDefPathData,
    symbols: &Interner,
) -> Result<String, WitViewError> {
    let mut output = String::new();
    let mut named_symbol = None;
    let mut named_string: Option<&str> = None;
    let mut extra: Option<&str> = None;
    let mut numeric: Option<String> = None;
    let tag = match &component.data {
        DefPathData::Module(name) => {
            named_symbol = Some(*name);
            "module"
        }
        DefPathData::TypeNs(name) => {
            named_symbol = Some(*name);
            "type"
        }
        DefPathData::ValueNs(name) => {
            named_symbol = Some(*name);
            "value"
        }
        DefPathData::Interface(name) => {
            named_symbol = Some(*name);
            "interface"
        }
        DefPathData::World(name) => {
            named_symbol = Some(*name);
            "world"
        }
        DefPathData::Effect(name) => {
            named_symbol = Some(*name);
            "effect"
        }
        DefPathData::Field(name) => {
            named_symbol = Some(*name);
            "field"
        }
        DefPathData::Variant(name) => {
            named_symbol = Some(*name);
            "variant"
        }
        DefPathData::AssocType(name) => {
            named_symbol = Some(*name);
            "assoc-type"
        }
        DefPathData::AssocFn(name) => {
            named_symbol = Some(*name);
            "assoc-fn"
        }
        DefPathData::TypeParam(name) => {
            named_symbol = Some(*name);
            "type-param"
        }
        DefPathData::ConstParam(name) => {
            named_symbol = Some(*name);
            "const-param"
        }
        DefPathData::Impl { header_hash } => {
            numeric = Some(format!("{header_hash:016x}"));
            "impl"
        }
        DefPathData::DerivedImpl { trait_name } => {
            named_symbol = Some(*trait_name);
            "derived-impl"
        }
        DefPathData::Invariant(name) => {
            named_symbol = Some(*name);
            "invariant"
        }
        DefPathData::ExternBlock { abi, module } => {
            named_symbol = Some(*abi);
            numeric = Some(if let Some(module) = module {
                let module = symbols.resolve(*module).map_err(WitViewError::Lookup)?;
                format!("module:{}:{}", module.len(), module)
            } else {
                "module:none".to_owned()
            });
            "extern-block"
        }
        DefPathData::Test(name) => {
            named_string = Some(name);
            "test"
        }
        DefPathData::Property(name) => {
            named_string = Some(name);
            "property"
        }
        DefPathData::Closure => "closure",
        DefPathData::AsyncBlock => "async-block",
        DefPathData::WorldItem { import, name } => {
            named_symbol = Some(*name);
            extra = Some(if *import { "import" } else { "export" });
            "world-import"
        }
        DefPathData::WorldInclude(name) => {
            named_symbol = Some(*name);
            "world-include"
        }
    };

    output.push_str(tag);
    output.push('[');
    if let Some(extra) = extra {
        write_length_prefixed(&mut output, extra);
        output.push(';');
    }
    let mut wrote_value = false;
    if let Some(name) = named_symbol {
        let name = symbols.resolve(name).map_err(WitViewError::Lookup)?;
        write_length_prefixed(&mut output, &name);
        wrote_value = true;
    }
    if let Some(name) = named_string {
        if wrote_value {
            output.push(';');
        }
        write_length_prefixed(&mut output, name);
        wrote_value = true;
    }
    if let Some(value) = numeric {
        if wrote_value {
            output.push(';');
        }
        write_length_prefixed(&mut output, &value);
    }
    output.push(';');
    output.push_str(&component.disambiguator.to_string());
    output.push_str("]/");
    Ok(output)
}

fn write_length_prefixed(output: &mut String, value: &str) {
    output.push_str(&value.len().to_string());
    output.push(':');
    output.push_str(value);
}

fn next_disambiguator(
    counts: &mut HashMap<DefPathData, u32>,
    data: &DefPathData,
) -> Result<u32, WitViewError> {
    let next = counts.entry(data.clone()).or_insert(0);
    let current = *next;
    *next = next
        .checked_add(1)
        .ok_or(WitViewError::TooManyDefinitions)?;
    Ok(current)
}

fn lookup_entity(
    entity_defs: &HashMap<WitViewEntity, DefId>,
    entity: WitViewEntity,
) -> Result<DefId, WitViewError> {
    entity_defs
        .get(&entity)
        .copied()
        .ok_or(WitViewError::MissingDefinition(entity))
}

fn insert_unique<V>(
    map: &mut BTreeMap<String, V>,
    key: String,
    value: V,
) -> Result<(), WitViewError> {
    if map.contains_key(&key) {
        return Err(WitViewError::DuplicateViewName {
            kind: "view",
            name: key,
        });
    }
    map.insert(key.clone(), value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{WitView, WitViewDefKind, WitViewEntity, WitViewOriginIdentity, wit_name_to_sai};
    use crate::{
        WitCase, WitField, WitIfaceIdx, WitIndexVec, WitInterface, WitItem, WitItemIdx,
        WitItemKind, WitMember, WitMemberKind, WitPackage, WitPackageName, WitType, WitUnit,
        WitWorld, WitWorldIdx, WitWorldItem,
    };
    use sai_hir::{DefPath, DefPathData, DefPathInterner, DisambiguatedDefPathData, IndexVec};
    use sai_intern::{DefIndex, Interner, PackageInterner};
    use sai_package::mvs::MvsPackageVersion;
    use sai_package::version::Version;
    use std::collections::BTreeSet;
    use std::sync::Arc;

    fn model(unit: WitUnit, reverse: bool) -> WitPackage {
        let mut items = vec![
            WitItem {
                name: "user-profile".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                kind: WitItemKind::Record(vec![
                    WitField {
                        name: "created-at".to_owned(),
                        docs: None,
                        ty: WitType::String,
                    },
                    WitField {
                        name: "type".to_owned(),
                        docs: None,
                        ty: WitType::Bool,
                    },
                ]),
                unsupported: None,
                span: None,
            },
            WitItem {
                name: "status".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                kind: WitItemKind::Variant(vec![
                    WitCase {
                        name: "not-found".to_owned(),
                        docs: None,
                        payload: None,
                    },
                    WitCase {
                        name: "ok".to_owned(),
                        docs: None,
                        payload: None,
                    },
                ]),
                unsupported: None,
                span: None,
            },
            WitItem {
                name: "get-user".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                kind: WitItemKind::Func(crate::WitFunc {
                    params: Vec::new(),
                    result: None,
                    is_async: false,
                }),
                unsupported: None,
                span: None,
            },
            WitItem {
                name: "resource".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                kind: WitItemKind::Resource(vec![
                    WitMember {
                        name: "new".to_owned(),
                        kind: WitMemberKind::Static,
                        func: crate::WitFunc {
                            params: Vec::new(),
                            result: None,
                            is_async: false,
                        },
                        docs: None,
                        since: None,
                        deprecated: None,
                        unsupported: None,
                    },
                    WitMember {
                        name: "constructor".to_owned(),
                        kind: WitMemberKind::Constructor,
                        func: crate::WitFunc {
                            params: Vec::new(),
                            result: None,
                            is_async: false,
                        },
                        docs: None,
                        since: None,
                        deprecated: None,
                        unsupported: None,
                    },
                    WitMember {
                        name: "read".to_owned(),
                        kind: WitMemberKind::Method,
                        func: crate::WitFunc {
                            params: Vec::new(),
                            result: None,
                            is_async: false,
                        },
                        docs: None,
                        since: None,
                        deprecated: None,
                        unsupported: None,
                    },
                ]),
                unsupported: None,
                span: None,
            },
            WitItem {
                name: "permissions".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                kind: WitItemKind::Flags(vec!["read".to_owned(), "write".to_owned()]),
                unsupported: None,
                span: None,
            },
        ];
        if reverse {
            items.reverse();
        }
        let mut item_vec = WitIndexVec::<WitItemIdx, _>::new();
        for item in items {
            item_vec.push(item).unwrap();
        }

        let mut interfaces = vec![
            WitInterface {
                name: "files".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                items: item_vec,
                span: None,
            },
            WitInterface {
                name: "metadata".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                items: WitIndexVec::new(),
                span: None,
            },
        ];
        if reverse {
            interfaces.reverse();
        }
        let first_interface_name = interfaces[0].name.clone();
        let mut interface_vec = WitIndexVec::<WitIfaceIdx, _>::new();
        for interface in interfaces {
            interface_vec.push(interface).unwrap();
        }

        let mut worlds = vec![
            WitWorld {
                name: "client".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                imports: vec![
                    WitWorldItem::Func {
                        name: "connect".to_owned(),
                        func: crate::WitFunc {
                            params: Vec::new(),
                            result: None,
                            is_async: false,
                        },
                        unsupported: None,
                        since: None,
                        deprecated: None,
                        span: None,
                    },
                    interface_ref(&unit, 0, &first_interface_name),
                ],
                exports: vec![WitWorldItem::Func {
                    name: "shutdown".to_owned(),
                    func: crate::WitFunc {
                        params: Vec::new(),
                        result: None,
                        is_async: false,
                    },
                    unsupported: None,
                    since: None,
                    deprecated: None,
                    span: None,
                }],
                includes: Vec::new(),
                source_order: vec![
                    crate::WitWorldItemOrder::import(0),
                    crate::WitWorldItemOrder::import(1),
                    crate::WitWorldItemOrder::export(0),
                ],
                unsupported: None,
                span: None,
            },
            WitWorld {
                name: "server".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                imports: Vec::new(),
                exports: Vec::new(),
                includes: Vec::new(),
                source_order: Vec::new(),
                unsupported: None,
                span: None,
            },
        ];
        if reverse {
            worlds.reverse();
        }
        let mut world_vec = WitIndexVec::new();
        for world in worlds {
            world_vec.push(world).unwrap();
        }

        WitPackage {
            unit,
            name: WitPackageName {
                namespace: "wasi".to_owned(),
                name: "cli".to_owned(),
                version: Some(Version::parse("0.2.3").unwrap()),
            },
            interfaces: interface_vec,
            worlds: world_vec,
            declaration_order: Vec::new(),
        }
    }

    fn interface_ref(unit: &WitUnit, index: u32, name: &str) -> WitWorldItem {
        WitWorldItem::Interface {
            reference: crate::WitIfaceRef {
                unit: unit.clone(),
                interface: crate::WitIfaceIdx::from_raw(index),
                name: name.to_owned(),
            },
            since: None,
            deprecated: None,
            span: None,
        }
    }

    fn built(reverse: bool, pad_interners: bool) -> WitView {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        if pad_interners {
            packages.intern("padding#1").unwrap();
            symbols.intern("padding").unwrap();
        }
        let unit = {
            let name = symbols.intern("cli").unwrap();
            WitUnit::wasi(name, &symbols, &Version::parse("0.2.3").unwrap()).unwrap()
        };
        let package = packages.intern(unit.stable_key()).unwrap();
        let def_paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        WitView::build(model(unit, reverse), &packages, &symbols, def_paths).unwrap()
    }

    fn source_def(
        package: sai_intern::PackageId,
        symbols: &Interner,
        packages: &PackageInterner,
        source_def_paths: &DefPathInterner,
        name: &str,
        interface: bool,
    ) -> sai_hir::DefId {
        let name = symbols.intern(name).unwrap();
        let data = if interface {
            DefPathData::Interface(name)
        } else {
            DefPathData::TypeNs(name)
        };
        let path = DefPath::new(
            package,
            vec![DisambiguatedDefPathData {
                data,
                disambiguator: 0,
            }],
            packages,
            symbols,
        )
        .unwrap();
        source_def_paths.intern(path).unwrap().to_def_id()
    }

    fn stable_defs(view: &WitView) -> Vec<(String, u128, u32, WitViewDefKind)> {
        view.defs
            .iter_enumerated()
            .filter_map(|(index, definition)| {
                let definition = definition.as_ref()?;
                Some((
                    definition.canonical_path.clone(),
                    definition.path_hash.raw(),
                    index.raw(),
                    definition.kind,
                ))
            })
            .collect()
    }

    #[test]
    fn definition_indices_follow_hash_order_independent_of_model_order() {
        let first = built(false, false);
        let reordered = built(true, false);
        assert_eq!(stable_defs(&first), stable_defs(&reordered));
        let definitions = first
            .defs
            .iter_enumerated()
            .filter_map(|(_, definition)| definition.as_ref())
            .collect::<Vec<_>>();
        assert!(definitions.windows(2).all(|pair| {
            pair[0].path_hash < pair[1].path_hash
                || (pair[0].path_hash == pair[1].path_hash
                    && pair[0].canonical_path <= pair[1].canonical_path)
        }));
        assert!(
            definitions
                .iter()
                .any(|definition| matches!(definition.entity, WitViewEntity::Field { .. }))
        );
        assert!(
            definitions
                .iter()
                .any(|definition| matches!(definition.entity, WitViewEntity::Variant { .. }))
        );
        assert!(
            definitions
                .iter()
                .any(|definition| matches!(definition.entity, WitViewEntity::FlagsMethod { .. }))
        );
        assert!(definitions.iter().any(|definition| matches!(
            definition.entity,
            WitViewEntity::WorldFunction { import: true, .. }
        )));
        assert!(definitions.iter().any(|definition| matches!(
            definition.entity,
            WitViewEntity::WorldFunction { import: false, .. }
        )));
    }

    #[test]
    fn world_exports_expose_only_canonical_function_names() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let unit_name = symbols.intern("cli").unwrap();
        let unit = WitUnit::wasi(unit_name, &symbols, &Version::parse("0.2.3").unwrap()).unwrap();
        let mut view_model = model(unit.clone(), false);
        let client = &mut view_model.worlds[WitWorldIdx::from_raw(0)];
        let WitWorldItem::Func { name, .. } = &mut client.exports[0] else {
            panic!("fixture starts with a function export");
        };
        *name = "ready-to-shutdown".to_owned();
        client.exports.push(interface_ref(&unit, 0, "files"));
        client.exports.push(WitWorldItem::Func {
            name: "type".to_owned(),
            func: crate::WitFunc {
                params: Vec::new(),
                result: None,
                is_async: false,
            },
            unsupported: None,
            since: None,
            deprecated: None,
            span: None,
        });
        client
            .source_order
            .push(crate::WitWorldItemOrder::export(1));
        client
            .source_order
            .push(crate::WitWorldItemOrder::export(2));

        let package = packages.intern(unit.stable_key()).unwrap();
        let paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        let view = WitView::build(view_model, &packages, &symbols, paths).unwrap();

        let client = &view.worlds["client"];
        assert_eq!(
            client.export_fns,
            BTreeSet::from(["ready_to_shutdown".to_owned(), "type".to_owned()])
        );
        assert_eq!(
            client
                .import_fns
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["connect"]
        );

        let mut reordered_model = model(unit.clone(), true);
        let client = &mut reordered_model.worlds[WitWorldIdx::from_raw(1)];
        let WitWorldItem::Func { name, .. } = &mut client.exports[0] else {
            panic!("fixture starts with a function export");
        };
        *name = "ready-to-shutdown".to_owned();
        let interface_name = reordered_model.interfaces[WitIfaceIdx::from_raw(0)]
            .name
            .clone();
        client
            .exports
            .push(interface_ref(&unit, 0, &interface_name));
        client.exports.push(WitWorldItem::Func {
            name: "type".to_owned(),
            func: crate::WitFunc {
                params: Vec::new(),
                result: None,
                is_async: false,
            },
            unsupported: None,
            since: None,
            deprecated: None,
            span: None,
        });
        client
            .source_order
            .push(crate::WitWorldItemOrder::export(1));
        client
            .source_order
            .push(crate::WitWorldItemOrder::export(2));
        let reversed_paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        let reordered =
            WitView::build(reordered_model, &packages, &symbols, reversed_paths).unwrap();
        assert_eq!(
            reordered.worlds["client"].export_fns,
            view.worlds["client"].export_fns
        );
    }

    #[test]
    fn persistent_definition_identities_ignore_interner_sessions() {
        let first = built(false, false);
        let second = built(false, true);
        assert_ne!(first.package, second.package);
        assert_eq!(first.unit.stable_key(), second.unit.stable_key());
        assert_eq!(stable_defs(&first), stable_defs(&second));
    }

    #[test]
    fn converts_wit_names_and_escapes_sai_keywords() {
        assert_eq!(
            wit_name_to_sai("outgoing-handler", false).unwrap(),
            "outgoing_handler"
        );
        assert_eq!(
            wit_name_to_sai("incoming-request", true).unwrap(),
            "IncomingRequest"
        );
        assert_eq!(wit_name_to_sai("type", false).unwrap(), "r#type");
        assert_eq!(wit_name_to_sai("interface", false).unwrap(), "interface");
        assert_eq!(wit_name_to_sai("world", false).unwrap(), "world");
        assert_eq!(wit_name_to_sai("nil", false).unwrap(), "r#nil");
        assert_eq!(wit_name_to_sai("HTTP-error", true).unwrap(), "HttpError");
    }

    #[test]
    fn constructor_name_maps_to_new_and_wins_over_static_new() {
        let view = built(false, false);
        let interface = view.interfaces.get("files").unwrap();
        assert_eq!(interface.shadowed.len(), 1);
        let shadowed = &interface.shadowed[0];
        assert_eq!(shadowed.wit_name, "new");
        assert_eq!(shadowed.resource.as_deref(), Some("resource"));
        assert!(matches!(
            view.definition(shadowed.winner)
                .map(|definition| definition.entity),
            Some(WitViewEntity::ResourceMember { member: 1, .. })
        ));
        assert_eq!(
            view.defs
                .iter_enumerated()
                .filter_map(|(_, definition)| definition.as_ref())
                .filter(|definition| matches!(
                    definition.entity,
                    WitViewEntity::ResourceMember { .. }
                ))
                .count(),
            2
        );
        let constructor = view
            .defs
            .iter_enumerated()
            .filter_map(|(_, definition)| definition.as_ref())
            .find(|definition| {
                matches!(
                    definition.entity,
                    WitViewEntity::ResourceMember { member: 1, .. }
                )
            })
            .unwrap();
        assert!(constructor.canonical_path.contains("assoc-fn[3:new;"));
    }

    #[test]
    fn definition_lookup_rejects_a_matching_package_and_index_from_another_scope() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let unit_name = symbols.intern("cli").unwrap();
        let unit = WitUnit::wasi(unit_name, &symbols, &Version::parse("0.2.3").unwrap()).unwrap();
        let package = packages.intern(unit.stable_key()).unwrap();
        let def_paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        let view_model = model(unit, false);
        let view = WitView::build(
            view_model.clone(),
            &packages,
            &symbols,
            Arc::clone(&def_paths),
        )
        .unwrap();
        let other_paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        let other_view = WitView::build(view_model, &packages, &symbols, other_paths).unwrap();
        let definition = view.interfaces.get("files").unwrap().def;
        let foreign_id = other_view.interfaces.get("files").unwrap().def;
        assert!(view.definition(definition).is_some());
        assert_eq!(foreign_id.package, definition.package);
        assert_eq!(foreign_id.index, definition.index);
        assert_ne!(foreign_id, definition);
        assert!(view.definition(foreign_id).is_none());
    }

    #[test]
    fn entity_lookup_returns_exact_scoped_interface_world_and_function_ids() {
        let view = built(false, false);
        let interface = &view.interfaces["files"];
        assert_eq!(
            view.definition_for_entity(WitViewEntity::Interface {
                interface: interface.wit
            }),
            Some(interface.def)
        );

        let world = &view.worlds["client"];
        assert_eq!(
            view.definition_for_entity(WitViewEntity::World { world: world.wit }),
            Some(world.def)
        );

        let import = view
            .definition_for_entity(WitViewEntity::WorldFunction {
                world: world.wit,
                import: true,
                item: 0,
            })
            .expect("the imported function has a view definition");
        assert_eq!(world.import_fns.get("connect"), Some(&import));
        assert_eq!(
            view.definition(import).map(|definition| definition.entity),
            Some(WitViewEntity::WorldFunction {
                world: world.wit,
                import: true,
                item: 0,
            })
        );
        let export = view
            .definition_for_entity(WitViewEntity::WorldFunction {
                world: world.wit,
                import: false,
                item: 0,
            })
            .expect("the exported function has a view definition");
        assert_eq!(
            view.definition(export).map(|definition| definition.entity),
            Some(WitViewEntity::WorldFunction {
                world: world.wit,
                import: false,
                item: 0,
            })
        );
        assert_eq!(
            view.definition_for_entity(WitViewEntity::WorldFunction {
                world: world.wit,
                import: true,
                item: 100,
            }),
            None
        );
    }

    #[test]
    fn rebuilding_a_view_reuses_definition_ids_and_value_equality() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let unit_name = symbols.intern("cli").unwrap();
        let unit = WitUnit::wasi(unit_name, &symbols, &Version::parse("0.2.3").unwrap()).unwrap();
        let package = packages.intern(unit.stable_key()).unwrap();
        let def_paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        let model = model(unit, false);

        let first =
            WitView::build(model.clone(), &packages, &symbols, Arc::clone(&def_paths)).unwrap();
        let second = WitView::build(model, &packages, &symbols, def_paths).unwrap();

        assert_eq!(first, second);
        assert_eq!(
            first.interfaces["files"].def,
            second.interfaces["files"].def
        );
    }

    #[test]
    fn removed_definitions_leave_tombstones_without_reassigning_other_ids() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let unit_name = symbols.intern("cli").unwrap();
        let unit = WitUnit::wasi(unit_name, &symbols, &Version::parse("0.2.3").unwrap()).unwrap();
        let package = packages.intern(unit.stable_key()).unwrap();
        let def_paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        let full_model = model(unit, false);
        let full = WitView::build(
            full_model.clone(),
            &packages,
            &symbols,
            Arc::clone(&def_paths),
        )
        .unwrap();
        let removed = full.interfaces["metadata"].def;
        let retained = full.interfaces["files"].def;

        let mut reduced_model = full_model;
        reduced_model.interfaces = WitIndexVec::try_from_vec(vec![
            reduced_model.interfaces[WitIfaceIdx::from_raw(0)].clone(),
        ])
        .unwrap();
        let reduced = WitView::build(reduced_model, &packages, &symbols, def_paths).unwrap();

        assert!(reduced.definition(removed).is_none());
        assert_eq!(reduced.interfaces["files"].def, retained);
        assert!(reduced.defs.get(removed.index).is_some_and(Option::is_none));
    }

    #[test]
    fn reserved_wit_names_use_semantic_symbols_without_the_raw_prefix() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let unit_name = symbols.intern("cli").unwrap();
        let unit = WitUnit::wasi(unit_name, &symbols, &Version::parse("0.2.3").unwrap()).unwrap();
        let mut model = model(unit.clone(), false);
        model.interfaces[WitIfaceIdx::from_raw(0)]
            .items
            .push(WitItem {
                name: "type".to_owned(),
                docs: None,
                since: None,
                deprecated: None,
                kind: WitItemKind::Func(crate::WitFunc {
                    params: Vec::new(),
                    result: None,
                    is_async: false,
                }),
                unsupported: None,
                span: None,
            })
            .unwrap();
        let package = packages.intern(unit.stable_key()).unwrap();
        let def_paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());
        let view = WitView::build(model, &packages, &symbols, def_paths).unwrap();
        let interface = &view.interfaces["files"];
        let member = &interface.members["type"];

        let path = view.def_path(member.def).unwrap().unwrap();
        let Some(sai_hir::DefPathData::ValueNs(symbol)) = path.data().last().map(|part| &part.data)
        else {
            panic!("function view path ends in the value namespace");
        };
        assert_eq!(symbols.resolve(*symbol).unwrap().as_ref(), "type");
        let definition = view.definition(member.def).unwrap();
        assert!(definition.canonical_path.contains("value[4:type;"));
        assert!(!definition.canonical_path.contains("r#type"));
    }

    #[test]
    fn generated_origins_must_match_the_generated_interfaces_exactly() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let selected =
            MvsPackageVersion::new("acme:graphics", Version::parse("1.2.3").unwrap()).unwrap();
        let package = packages.intern("acme:graphics#1").unwrap();
        let unit = WitUnit::generated(package, &packages, &selected).unwrap();
        let mut model = model(unit.clone(), false);
        model.name = WitPackageName {
            namespace: "acme".to_owned(),
            name: "graphics".to_owned(),
            version: Some(Version::parse("1.2.3").unwrap()),
        };
        let source = package;
        let source_def_paths = DefPathInterner::new(source, &packages, &symbols).unwrap();
        let files_origin = source_def(
            source,
            &symbols,
            &packages,
            &source_def_paths,
            "SaiFiles",
            true,
        );
        let metadata_origin = source_def(
            source,
            &symbols,
            &packages,
            &source_def_paths,
            "SaiMetadata",
            true,
        );
        let unknown_origin = source_def(
            source,
            &symbols,
            &packages,
            &source_def_paths,
            "SaiUnknown",
            true,
        );
        let second_unknown_origin = source_def(
            source,
            &symbols,
            &packages,
            &source_def_paths,
            "SaiSecondUnknown",
            true,
        );
        let origins = vec![
            ("files".to_owned(), files_origin),
            ("metadata".to_owned(), metadata_origin),
        ];

        let view_package = packages.intern(model.unit.stable_key()).unwrap();
        let def_paths = Arc::new(DefPathInterner::new(view_package, &packages, &symbols).unwrap());
        let view = WitView::build_with_origins(
            model.clone(),
            &packages,
            &symbols,
            Arc::clone(&def_paths),
            &source_def_paths,
            &origins,
        )
        .unwrap();
        assert_eq!(view.interfaces["files"].origin, Some(files_origin));
        assert_eq!(view.interfaces["metadata"].origin, Some(metadata_origin));
        assert_eq!(
            view.interfaces["files"]
                .origin_identity
                .as_ref()
                .unwrap()
                .canonical_path,
            "interface[8:SaiFiles;0]/"
        );

        assert!(matches!(
            WitView::build_with_origins(
                model.clone(),
                &packages,
                &symbols,
                Arc::clone(&def_paths),
                &source_def_paths,
                &[origins[0].clone()]
            ),
            Err(super::WitViewError::MissingGeneratedOrigin(name)) if name == "metadata"
        ));
        assert!(matches!(
            WitView::build_with_origins(
                model.clone(),
                &packages,
                &symbols,
                Arc::clone(&def_paths),
                &source_def_paths,
                &[origins[0].clone(), origins[0].clone(), origins[1].clone()]
            ),
            Err(super::WitViewError::DuplicateGeneratedOrigin(name)) if name == "files"
        ));
        assert!(matches!(
            WitView::build_with_origins(
                model.clone(),
                &packages,
                &symbols,
                Arc::clone(&def_paths),
                &source_def_paths,
                &[
                    ("files".to_owned(), files_origin),
                    ("metadata".to_owned(), files_origin),
                ]
            ),
            Err(super::WitViewError::DuplicateGeneratedOriginDefinition(def))
                if def == files_origin
        ));
        assert!(matches!(
            WitView::build_with_origins(
                model,
                &packages,
                &symbols,
                Arc::clone(&def_paths),
                &source_def_paths,
                &[
                    origins[0].clone(),
                    origins[1].clone(),
                    ("unknown".to_owned(), unknown_origin),
                ]
            ),
            Err(super::WitViewError::UnknownGeneratedOrigin(name)) if name == "unknown"
        ));
        assert!(matches!(
            WitView::build_with_origins(
                view.model.clone(),
                &packages,
                &symbols,
                Arc::clone(&def_paths),
                &source_def_paths,
                &[
                    origins[0].clone(),
                    origins[1].clone(),
                    ("zulu".to_owned(), unknown_origin),
                    ("alpha".to_owned(), second_unknown_origin),
                ]
            ),
            Err(super::WitViewError::UnknownGeneratedOrigin(name)) if name == "zulu"
        ));

        let other_package = packages.intern("acme:other#1").unwrap();
        let other_source_def_paths =
            DefPathInterner::new(other_package, &packages, &symbols).unwrap();
        let wrong_origin = source_def(
            other_package,
            &symbols,
            &packages,
            &other_source_def_paths,
            "Foreign",
            true,
        );
        let invalid_def_paths =
            Arc::new(DefPathInterner::new(view_package, &packages, &symbols).unwrap());
        assert!(matches!(
            WitView::build_with_origins(
                view.model.clone(),
                &packages,
                &symbols,
                Arc::clone(&invalid_def_paths),
                &source_def_paths,
                &[("files".to_owned(), wrong_origin), origins[1].clone()]
            ),
            Err(super::WitViewError::GeneratedOriginPackageMismatch {
                interface,
                expected,
                actual,
            }) if interface == "files" && expected == source && actual == other_package
        ));

        let wrong_interner = DefPathInterner::new(other_package, &packages, &symbols).unwrap();
        assert!(matches!(
            WitView::build_with_origins(
                view.model.clone(),
                &packages,
                &symbols,
                Arc::clone(&invalid_def_paths),
                &wrong_interner,
                &origins
            ),
            Err(super::WitViewError::GeneratedOriginInternerPackageMismatch {
                expected,
                actual,
            }) if expected == source && actual == other_package
        ));

        let foreign_scope_interner = DefPathInterner::new(source, &packages, &symbols).unwrap();
        let foreign_scope_origin = source_def(
            source,
            &symbols,
            &packages,
            &foreign_scope_interner,
            "ForeignScope",
            true,
        );
        assert!(matches!(
            WitView::build_with_origins(
                view.model.clone(),
                &packages,
                &symbols,
                Arc::clone(&invalid_def_paths),
                &source_def_paths,
                &[
                    ("files".to_owned(), foreign_scope_origin),
                    origins[1].clone()
                ]
            ),
            Err(super::WitViewError::GeneratedOriginPath {
                error: sai_hir::DefPathInternerError::ForeignDefinitionId,
                ..
            })
        ));

        let non_interface = source_def(
            source,
            &symbols,
            &packages,
            &source_def_paths,
            "NotAnInterface",
            false,
        );
        assert!(matches!(
            WitView::build_with_origins(
                view.model.clone(),
                &packages,
                &symbols,
                Arc::clone(&invalid_def_paths),
                &source_def_paths,
                &[("files".to_owned(), non_interface), origins[1].clone()]
            ),
            Err(super::WitViewError::GeneratedOriginNotInterface { interface, def })
                if interface == "files" && def == non_interface
        ));

        let mut indices = IndexVec::<DefIndex, ()>::new();
        for _ in 0..source_def_paths.len().unwrap() {
            indices.push(()).unwrap();
        }
        let mut unknown_index = files_origin;
        unknown_index.index = indices.push(()).unwrap();
        assert!(matches!(
            WitView::build_with_origins(
                view.model.clone(),
                &packages,
                &symbols,
                Arc::clone(&invalid_def_paths),
                &source_def_paths,
                &[("files".to_owned(), unknown_index), origins[1].clone()]
            ),
            Err(super::WitViewError::GeneratedOriginDefinitionNotFound { interface, def })
                if interface == "files" && def == unknown_index
        ));
        assert_eq!(invalid_def_paths.len().unwrap(), 0);
    }

    #[test]
    fn generated_origin_identity_ignores_runtime_package_and_symbol_ids() {
        fn build_origin(padded: bool) -> (sai_intern::PackageId, WitViewOriginIdentity) {
            let packages = PackageInterner::new();
            let symbols = Interner::new();
            if padded {
                packages.intern("padding#1").unwrap();
                symbols.intern("padding").unwrap();
            }
            let selected =
                MvsPackageVersion::new("acme:graphics", Version::parse("1.2.3").unwrap()).unwrap();
            let source = packages.intern("acme:graphics#1").unwrap();
            let unit = WitUnit::generated(source, &packages, &selected).unwrap();
            let source_def_paths = DefPathInterner::new(source, &packages, &symbols).unwrap();
            let files_origin = source_def(
                source,
                &symbols,
                &packages,
                &source_def_paths,
                "SaiFiles",
                true,
            );
            let metadata_origin = source_def(
                source,
                &symbols,
                &packages,
                &source_def_paths,
                "SaiMetadata",
                true,
            );
            let mut model = model(unit, false);
            model.name = WitPackageName {
                namespace: "acme".to_owned(),
                name: "graphics".to_owned(),
                version: Some(Version::parse("1.2.3").unwrap()),
            };
            let view_package = packages.intern(model.unit.stable_key()).unwrap();
            let view_def_paths =
                Arc::new(DefPathInterner::new(view_package, &packages, &symbols).unwrap());
            let view = WitView::build_with_origins(
                model,
                &packages,
                &symbols,
                view_def_paths,
                &source_def_paths,
                &[
                    ("files".to_owned(), files_origin),
                    ("metadata".to_owned(), metadata_origin),
                ],
            )
            .unwrap();
            (
                source,
                view.interfaces["files"].origin_identity.clone().unwrap(),
            )
        }

        let (first_package, first_identity) = build_origin(false);
        let (second_package, second_identity) = build_origin(true);
        assert_ne!(first_package, second_package);
        assert_eq!(first_identity, second_identity);
    }

    #[test]
    fn duplicate_normalized_world_import_names_are_rejected_before_interning() {
        let packages = PackageInterner::new();
        let symbols = Interner::new();
        let unit_name = symbols.intern("cli").unwrap();
        let unit = WitUnit::wasi(unit_name, &symbols, &Version::parse("0.2.3").unwrap()).unwrap();
        let mut model = model(unit, false);
        let duplicate = |name: &str| WitWorldItem::Func {
            name: name.to_owned(),
            func: crate::WitFunc {
                params: Vec::new(),
                result: None,
                is_async: false,
            },
            unsupported: None,
            since: None,
            deprecated: None,
            span: None,
        };
        model.worlds[crate::WitWorldIdx::from_raw(0)]
            .imports
            .extend([duplicate("HTTP"), duplicate("http")]);
        let package = packages.intern(model.unit.stable_key()).unwrap();
        let def_paths = Arc::new(DefPathInterner::new(package, &packages, &symbols).unwrap());

        assert!(matches!(
            WitView::build(model, &packages, &symbols, Arc::clone(&def_paths)),
            Err(super::WitViewError::DuplicateWorldImportFunction { world, name })
                if world == "client" && name == "http"
        ));
        assert_eq!(def_paths.len().unwrap(), 0);
    }
}
