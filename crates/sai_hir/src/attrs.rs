use crate::item_tree::{AttrName, RawAttr, RawAttrArg, RawAttrValue};
use crate::types::LangItem;
use sai_diagnostics::{
    Applicability, Child, ChildKind, Code, Diagnostic, Fix, FixOperation, FixOperationName,
    FixOperationPlanner, LINT_REGISTRY, Label, LintLevel as DiagnosticLintLevel, LintNameLookup,
    Location, NodeGroup, NodeId, OperationPlanError, Severity, SourceRef, TextEdit, Value,
    ValueObject, WorkspaceSnapshot,
};
use sai_intern::{Interner, Symbol};
use sai_vfs::{FileId, Span, WorkspaceSnapshot as VfsWorkspaceSnapshot};
use std::collections::{BTreeSet, HashMap, HashSet};

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(u8)]
pub enum TargetKind {
    Fn,
    TraitFn,
    ImplFn,
    ExternFn,
    InterfaceFn,
    Record,
    Enum,
    Distinct,
    Refined,
    Alias,
    Resource,
    Trait,
    Impl,
    Const,
    Invariant,
    Effect,
    Interface,
    World,
    ExternBlock,
    Use,
    Test,
    Property,
    Field,
    Variant,
    AssocType,
}

impl TargetKind {
    pub const ALL: [Self; 25] = [
        Self::Fn,
        Self::TraitFn,
        Self::ImplFn,
        Self::ExternFn,
        Self::InterfaceFn,
        Self::Record,
        Self::Enum,
        Self::Distinct,
        Self::Refined,
        Self::Alias,
        Self::Resource,
        Self::Trait,
        Self::Impl,
        Self::Const,
        Self::Invariant,
        Self::Effect,
        Self::Interface,
        Self::World,
        Self::ExternBlock,
        Self::Use,
        Self::Test,
        Self::Property,
        Self::Field,
        Self::Variant,
        Self::AssocType,
    ];
}

/// Additional information needed to decide whether a syntactic target is
/// allowed. The validator obtains these facts from the containing item tree.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetCondition {
    Any,
    ModuleLevel,
    HasBody,
    IsExported,
    InherentImpl,
    ExternWasmOrUnsafeC,
    ExternWasm,
    InsideInterface,
    TraitDeclaration,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TargetRule {
    pub kind: TargetKind,
    pub condition: TargetCondition,
}

impl TargetRule {
    pub const fn new(kind: TargetKind, condition: TargetCondition) -> Self {
        Self { kind, condition }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ArgGrammar {
    None,
    DeriveList,
    Inline,
    OptionalNonEmptyString,
    Deprecated,
    LintCodes,
    NonEmptyString,
    Repr,
    TargetPredicate,
    IntegerRange { min: u64, max: u64 },
    Word(&'static str),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ConflictCondition {
    Always,
    InlineIsHint,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SpecialTargetCase {
    DeriveOnResource,
    ReprInsideInterface,
    FlagsOutsideInterface,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttributeConflict {
    pub other: AttrName,
    pub condition: ConflictCondition,
}

impl AttributeConflict {
    pub const fn new(other: AttrName, condition: ConflictCondition) -> Self {
        Self { other, condition }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttrSchema {
    pub name: AttrName,
    pub stage: u8,
    pub targets: &'static [TargetRule],
    pub args: ArgGrammar,
    pub repeatable: bool,
    pub conflicts: &'static [AttributeConflict],
    pub special_target_cases: &'static [SpecialTargetCase],
}

const ANY: TargetCondition = TargetCondition::Any;
const MODULE: TargetCondition = TargetCondition::ModuleLevel;

const FUNCTION_WITH_BODY: &[TargetRule] = &[
    TargetRule::new(TargetKind::Fn, TargetCondition::HasBody),
    TargetRule::new(TargetKind::TraitFn, TargetCondition::HasBody),
    TargetRule::new(TargetKind::ImplFn, TargetCondition::HasBody),
];
const MUST_USE_TARGETS: &[TargetRule] = &[
    TargetRule::new(TargetKind::Fn, ANY),
    TargetRule::new(TargetKind::TraitFn, ANY),
    TargetRule::new(TargetKind::ImplFn, ANY),
    TargetRule::new(TargetKind::ExternFn, ANY),
    TargetRule::new(TargetKind::InterfaceFn, ANY),
    TargetRule::new(TargetKind::Record, ANY),
    TargetRule::new(TargetKind::Enum, ANY),
    TargetRule::new(TargetKind::Distinct, ANY),
    TargetRule::new(TargetKind::Refined, ANY),
    TargetRule::new(TargetKind::Resource, ANY),
];
const DEPRECATED_TARGETS: &[TargetRule] = &[
    TargetRule::new(TargetKind::Fn, ANY),
    TargetRule::new(TargetKind::TraitFn, ANY),
    TargetRule::new(TargetKind::ImplFn, TargetCondition::InherentImpl),
    TargetRule::new(TargetKind::ExternFn, ANY),
    TargetRule::new(TargetKind::InterfaceFn, ANY),
    TargetRule::new(TargetKind::Record, ANY),
    TargetRule::new(TargetKind::Enum, ANY),
    TargetRule::new(TargetKind::Distinct, ANY),
    TargetRule::new(TargetKind::Refined, ANY),
    TargetRule::new(TargetKind::Alias, ANY),
    TargetRule::new(TargetKind::Resource, ANY),
    TargetRule::new(TargetKind::Trait, ANY),
    TargetRule::new(TargetKind::Const, ANY),
    TargetRule::new(TargetKind::Effect, ANY),
    TargetRule::new(TargetKind::Interface, ANY),
    TargetRule::new(TargetKind::World, ANY),
    TargetRule::new(TargetKind::Field, ANY),
    TargetRule::new(TargetKind::Variant, ANY),
    TargetRule::new(TargetKind::AssocType, TargetCondition::TraitDeclaration),
];
const LINT_TARGETS: &[TargetRule] = &[
    TargetRule::new(TargetKind::Fn, ANY),
    TargetRule::new(TargetKind::TraitFn, ANY),
    TargetRule::new(TargetKind::ImplFn, ANY),
    TargetRule::new(TargetKind::ExternFn, ANY),
    TargetRule::new(TargetKind::InterfaceFn, ANY),
    TargetRule::new(TargetKind::Record, ANY),
    TargetRule::new(TargetKind::Enum, ANY),
    TargetRule::new(TargetKind::Distinct, ANY),
    TargetRule::new(TargetKind::Refined, ANY),
    TargetRule::new(TargetKind::Alias, ANY),
    TargetRule::new(TargetKind::Resource, ANY),
    TargetRule::new(TargetKind::Trait, ANY),
    TargetRule::new(TargetKind::Impl, ANY),
    TargetRule::new(TargetKind::Const, ANY),
    TargetRule::new(TargetKind::Invariant, ANY),
    TargetRule::new(TargetKind::Effect, ANY),
    TargetRule::new(TargetKind::Interface, ANY),
    TargetRule::new(TargetKind::World, ANY),
    TargetRule::new(TargetKind::ExternBlock, ANY),
    TargetRule::new(TargetKind::Use, ANY),
    TargetRule::new(TargetKind::Test, ANY),
    TargetRule::new(TargetKind::Property, ANY),
    TargetRule::new(TargetKind::Field, ANY),
    TargetRule::new(TargetKind::Variant, ANY),
    TargetRule::new(TargetKind::AssocType, ANY),
];
const DERIVE_TARGETS: &[TargetRule] = &[
    TargetRule::new(TargetKind::Record, ANY),
    TargetRule::new(TargetKind::Enum, ANY),
    TargetRule::new(TargetKind::Distinct, ANY),
    TargetRule::new(TargetKind::Refined, ANY),
];
const MODULE_ITEM_TARGETS: &[TargetRule] = &[
    TargetRule::new(TargetKind::Fn, MODULE),
    TargetRule::new(TargetKind::Record, MODULE),
    TargetRule::new(TargetKind::Enum, MODULE),
    TargetRule::new(TargetKind::Distinct, MODULE),
    TargetRule::new(TargetKind::Refined, MODULE),
    TargetRule::new(TargetKind::Alias, MODULE),
    TargetRule::new(TargetKind::Resource, MODULE),
    TargetRule::new(TargetKind::Trait, MODULE),
    TargetRule::new(TargetKind::Impl, MODULE),
    TargetRule::new(TargetKind::Const, MODULE),
    TargetRule::new(TargetKind::Invariant, MODULE),
    TargetRule::new(TargetKind::Effect, MODULE),
    TargetRule::new(TargetKind::ExternBlock, MODULE),
    TargetRule::new(TargetKind::Use, MODULE),
    TargetRule::new(TargetKind::Test, MODULE),
    TargetRule::new(TargetKind::Property, MODULE),
];
const DOC_TARGETS: &[TargetRule] = &[
    TargetRule::new(TargetKind::Fn, ANY),
    TargetRule::new(TargetKind::TraitFn, ANY),
    TargetRule::new(TargetKind::ImplFn, ANY),
    TargetRule::new(TargetKind::ExternFn, ANY),
    TargetRule::new(TargetKind::InterfaceFn, ANY),
    TargetRule::new(TargetKind::Record, ANY),
    TargetRule::new(TargetKind::Enum, ANY),
    TargetRule::new(TargetKind::Distinct, ANY),
    TargetRule::new(TargetKind::Refined, ANY),
    TargetRule::new(TargetKind::Alias, ANY),
    TargetRule::new(TargetKind::Resource, ANY),
    TargetRule::new(TargetKind::Trait, ANY),
    TargetRule::new(TargetKind::Impl, ANY),
    TargetRule::new(TargetKind::Const, ANY),
    TargetRule::new(TargetKind::Invariant, ANY),
    TargetRule::new(TargetKind::Effect, ANY),
    TargetRule::new(TargetKind::Interface, ANY),
    TargetRule::new(TargetKind::World, ANY),
    TargetRule::new(TargetKind::ExternBlock, ANY),
    TargetRule::new(TargetKind::Use, ANY),
    TargetRule::new(TargetKind::Test, ANY),
    TargetRule::new(TargetKind::Property, ANY),
    TargetRule::new(TargetKind::Field, ANY),
    TargetRule::new(TargetKind::Variant, ANY),
    TargetRule::new(TargetKind::AssocType, ANY),
];

const EXPORT_NAME_TARGETS: &[TargetRule] =
    &[TargetRule::new(TargetKind::Fn, TargetCondition::IsExported)];
const IMPORT_NAME_TARGETS: &[TargetRule] = &[TargetRule::new(
    TargetKind::ExternFn,
    TargetCondition::ExternWasmOrUnsafeC,
)];
const INLINE_CONFLICT: &[AttributeConflict] = &[AttributeConflict::new(
    AttrName::Cold,
    ConflictCondition::InlineIsHint,
)];
const COLD_CONFLICT: &[AttributeConflict] = &[AttributeConflict::new(
    AttrName::Inline,
    ConflictCondition::InlineIsHint,
)];
const DERIVE_CONFLICT: &[AttributeConflict] = &[AttributeConflict::new(
    AttrName::Flags,
    ConflictCondition::Always,
)];
const FLAGS_CONFLICT: &[AttributeConflict] = &[AttributeConflict::new(
    AttrName::Derive,
    ConflictCondition::Always,
)];
const DERIVE_SPECIAL_CASES: &[SpecialTargetCase] = &[SpecialTargetCase::DeriveOnResource];
const REPR_SPECIAL_CASES: &[SpecialTargetCase] = &[SpecialTargetCase::ReprInsideInterface];
const FLAGS_SPECIAL_CASES: &[SpecialTargetCase] = &[SpecialTargetCase::FlagsOutsideInterface];

pub const ATTRIBUTE_SCHEMAS: [AttrSchema; 17] = [
    AttrSchema {
        name: AttrName::Derive,
        stage: 2,
        targets: DERIVE_TARGETS,
        args: ArgGrammar::DeriveList,
        repeatable: false,
        conflicts: DERIVE_CONFLICT,
        special_target_cases: DERIVE_SPECIAL_CASES,
    },
    AttrSchema {
        name: AttrName::ExportName,
        stage: 1,
        targets: EXPORT_NAME_TARGETS,
        args: ArgGrammar::NonEmptyString,
        repeatable: false,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Inline,
        stage: 0,
        targets: FUNCTION_WITH_BODY,
        args: ArgGrammar::Inline,
        repeatable: false,
        conflicts: INLINE_CONFLICT,
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::ImportName,
        stage: 1,
        targets: IMPORT_NAME_TARGETS,
        args: ArgGrammar::NonEmptyString,
        repeatable: false,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Cold,
        stage: 0,
        targets: FUNCTION_WITH_BODY,
        args: ArgGrammar::None,
        repeatable: false,
        conflicts: COLD_CONFLICT,
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Repr,
        stage: 0,
        targets: &[
            TargetRule::new(TargetKind::Record, MODULE),
            TargetRule::new(TargetKind::Enum, MODULE),
        ],
        args: ArgGrammar::Repr,
        repeatable: false,
        conflicts: &[],
        special_target_cases: REPR_SPECIAL_CASES,
    },
    AttrSchema {
        name: AttrName::MustUse,
        stage: 0,
        targets: MUST_USE_TARGETS,
        args: ArgGrammar::OptionalNonEmptyString,
        repeatable: false,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Flags,
        stage: 4,
        targets: &[TargetRule::new(
            TargetKind::Enum,
            TargetCondition::InsideInterface,
        )],
        args: ArgGrammar::None,
        repeatable: false,
        conflicts: FLAGS_CONFLICT,
        special_target_cases: FLAGS_SPECIAL_CASES,
    },
    AttrSchema {
        name: AttrName::Deprecated,
        stage: 0,
        targets: DEPRECATED_TARGETS,
        args: ArgGrammar::Deprecated,
        repeatable: false,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Target,
        stage: 0,
        targets: MODULE_ITEM_TARGETS,
        args: ArgGrammar::TargetPredicate,
        repeatable: false,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Allow,
        stage: 0,
        targets: LINT_TARGETS,
        args: ArgGrammar::LintCodes,
        repeatable: true,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Cases,
        stage: 3,
        targets: &[TargetRule::new(TargetKind::Property, ANY)],
        args: ArgGrammar::IntegerRange {
            min: 1,
            max: u32::MAX as u64,
        },
        repeatable: false,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Warn,
        stage: 0,
        targets: LINT_TARGETS,
        args: ArgGrammar::LintCodes,
        repeatable: true,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Seed,
        stage: 3,
        targets: &[TargetRule::new(TargetKind::Property, ANY)],
        args: ArgGrammar::IntegerRange {
            min: 0,
            max: u64::MAX,
        },
        repeatable: false,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Deny,
        stage: 0,
        targets: LINT_TARGETS,
        args: ArgGrammar::LintCodes,
        repeatable: true,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::Doc,
        stage: 0,
        targets: DOC_TARGETS,
        args: ArgGrammar::Word("hidden"),
        repeatable: false,
        conflicts: &[],
        special_target_cases: &[],
    },
    AttrSchema {
        name: AttrName::PanicHook,
        stage: 1,
        targets: &[TargetRule::new(
            TargetKind::ExternFn,
            TargetCondition::ExternWasm,
        )],
        args: ArgGrammar::None,
        repeatable: false,
        conflicts: &[],
        special_target_cases: &[],
    },
];

pub fn schema(name: AttrName) -> &'static AttrSchema {
    &ATTRIBUTE_SCHEMAS[name as usize]
}

/// The highest attribute stage whose validation is implemented in this crate.
pub const IMPLEMENTED_ATTRIBUTE_STAGE: u8 = 0;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImplKind {
    Inherent,
    Trait,
    Interface,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExternAbi {
    Wasm,
    C,
    Other,
}

/// Syntactic facts used by the target rules for one attribute owner.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttributeTarget {
    pub kind: TargetKind,
    pub module_level: bool,
    pub has_body: bool,
    pub is_exported: bool,
    pub impl_kind: Option<ImplKind>,
    pub extern_abi: Option<ExternAbi>,
    pub extern_unsafe: bool,
    pub inside_interface: bool,
    pub assoc_type_in_trait: bool,
}

impl AttributeTarget {
    pub const fn new(kind: TargetKind) -> Self {
        Self {
            kind,
            module_level: false,
            has_body: false,
            is_exported: false,
            impl_kind: None,
            extern_abi: None,
            extern_unsafe: false,
            inside_interface: false,
            assoc_type_in_trait: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReprInteger {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReprAttr {
    C,
    Integer(ReprInteger),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum InlineHint {
    Hint,
    Never,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Deprecation {
    pub message: Option<String>,
    /// Name-resolution validation of this dotted replacement path is deferred.
    pub instead: Option<String>,
    pub instead_span: Option<Span>,
    pub span: Span,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LintAttr {
    pub level: DiagnosticLintLevel,
    pub name: Symbol,
    pub span: Span,
}

/// The validated value of one `@export_name` attribute.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidatedExportName {
    /// The decoded, non-empty string supplied to the attribute.
    pub value: String,
    /// Source span of the string token, including its delimiters.
    pub string_span: Span,
}

/// Result of validating only `@export_name` attributes for one target.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ExportNameValidation {
    pub export_name: Option<ValidatedExportName>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TargetPredicate {
    Target(String),
    Capability(String),
    Not(Box<Self>),
    All(Vec<Self>),
    Any(Vec<Self>),
}

impl TargetPredicate {
    /// Evaluates this predicate against one build target and its effective
    /// capability set.
    pub fn matches(&self, target: &TargetCfg) -> bool {
        match self {
            Self::Target(name) => name == target.target_name(),
            Self::Capability(capability) => target.capabilities().contains(capability),
            Self::Not(predicate) => !predicate.matches(target),
            Self::All(predicates) => predicates.iter().all(|predicate| predicate.matches(target)),
            Self::Any(predicates) => predicates.iter().any(|predicate| predicate.matches(target)),
        }
    }
}

/// The target facts used when filtering one module's items.
///
/// The driver supplies the effective capability set for the requested
/// artifact, including artifact-dependent capabilities such as `host`.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TargetCfg {
    target_name: String,
    pointer_width: u8,
    capabilities: BTreeSet<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TargetCfgError {
    UnknownTarget,
    UnknownCapability,
    CapabilityUnavailable,
    MissingRequiredCapability,
}

impl TargetCfg {
    /// Creates a target configuration and rejects capabilities that contradict
    /// the target table. Artifact-dependent capabilities such as `host` must
    /// already reflect the artifact selected by the driver.
    pub fn new<I, S>(
        target_name: impl Into<String>,
        capabilities: I,
    ) -> Result<Self, TargetCfgError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let target_name = target_name.into();
        let pointer_width =
            target_pointer_width(&target_name).ok_or(TargetCfgError::UnknownTarget)?;

        let capabilities = capabilities
            .into_iter()
            .map(Into::into)
            .collect::<BTreeSet<_>>();
        if capabilities
            .iter()
            .any(|capability| !is_target_capability(capability))
        {
            return Err(TargetCfgError::UnknownCapability);
        }
        if capabilities.contains("threads") {
            return Err(TargetCfgError::CapabilityUnavailable);
        }

        let wasm = target_name.starts_with("wasm");
        let wasm64 = target_name.starts_with("wasm64-");
        if wasm
            && capabilities
                .iter()
                .any(|capability| matches!(capability.as_str(), "native_ffi" | "process"))
        {
            return Err(TargetCfgError::CapabilityUnavailable);
        }
        if capabilities.contains("memory64") && !wasm64 {
            return Err(TargetCfgError::CapabilityUnavailable);
        }
        if wasm64 && !capabilities.contains("memory64") {
            return Err(TargetCfgError::MissingRequiredCapability);
        }

        Ok(Self {
            target_name,
            pointer_width,
            capabilities,
        })
    }

    pub fn target_name(&self) -> &str {
        &self.target_name
    }

    /// Pointer width in bits for this target, including the width of `ISize`
    /// and `USize`.
    pub const fn pointer_width(&self) -> u8 {
        self.pointer_width
    }

    pub fn capabilities(&self) -> &BTreeSet<String> {
        &self.capabilities
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ValidatedAttrs {
    pub derives: Vec<(LangItem, Span)>,
    pub export_name: Option<ValidatedExportName>,
    pub inline: Option<(InlineHint, Span)>,
    pub cold: Option<Span>,
    pub must_use: Option<(Option<String>, Span)>,
    pub deprecated: Option<Deprecation>,
    pub lint_levels: Vec<LintAttr>,
    pub repr: Option<(ReprAttr, Span)>,
    pub flags: Option<Span>,
    pub target: Option<(TargetPredicate, Span)>,
    pub doc_hidden: Option<Span>,
    pub diagnostics: Vec<Diagnostic>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttrValidationError {
    MissingFile,
    InvalidSourceSpan,
    SnapshotMismatch,
    ForeignSymbol,
    FixPlanningFailed,
}

/// Validates the attributes of one syntactic target at the implemented stage.
///
/// A missing target kind means syntax recovery could not determine what was
/// annotated. In that case the attributes are discarded without diagnostics.
pub fn validate_attrs(
    attributes: &[RawAttr],
    target: Option<AttributeTarget>,
    symbols: &Interner,
    snapshot: &VfsWorkspaceSnapshot,
) -> Result<ValidatedAttrs, AttrValidationError> {
    validate_attrs_at_stage(
        attributes,
        target,
        symbols,
        snapshot,
        IMPLEMENTED_ATTRIBUTE_STAGE,
    )
}

/// Validates and returns only an `@export_name` attribute for a target.
///
/// This narrow API validates `@export_name` at its declared schema stage while
/// leaving [`IMPLEMENTED_ATTRIBUTE_STAGE`] unchanged. All other attributes in
/// `attributes` are ignored here; callers continue to use [`validate_attrs`]
/// for the attributes implemented by the general HIR pipeline.
pub fn validate_export_name(
    attributes: &[RawAttr],
    target: Option<AttributeTarget>,
    symbols: &Interner,
    snapshot: &VfsWorkspaceSnapshot,
) -> Result<ExportNameValidation, AttrValidationError> {
    let export_name_attributes = attributes
        .iter()
        .filter(|attribute| attribute.name == AttrName::ExportName)
        .cloned()
        .collect::<Vec<_>>();
    let validated = validate_attrs_at_stage(&export_name_attributes, target, symbols, snapshot, 1)?;
    Ok(ExportNameValidation {
        export_name: validated.export_name,
        diagnostics: validated.diagnostics,
    })
}

pub(crate) fn validate_attrs_at_stage(
    attributes: &[RawAttr],
    target: Option<AttributeTarget>,
    symbols: &Interner,
    snapshot: &VfsWorkspaceSnapshot,
    implemented_stage: u8,
) -> Result<ValidatedAttrs, AttrValidationError> {
    let Some(target) = target else {
        return Ok(ValidatedAttrs::default());
    };

    let mut validated = ValidatedAttrs::default();
    let mut accepted = Vec::<AcceptedAttribute>::new();
    let mut first_by_name = HashMap::<AttrName, Span>::new();
    let mut first_attribute_by_name = HashMap::<AttrName, usize>::new();
    let mut seen_lint_codes = HashSet::<(DiagnosticLintLevel, Symbol)>::new();
    let mut first_lint_level_by_code = HashMap::<Symbol, (DiagnosticLintLevel, Span)>::new();

    for (attribute_index, attribute) in attributes.iter().enumerate() {
        if attribute.has_error {
            continue;
        }

        let attribute_schema = schema(attribute.name);
        if attribute_schema.stage > implemented_stage {
            let mut diagnostic = diagnostic_at(
                Code::FeatureNotImplemented,
                format!(
                    "attribute @{} is not implemented at this stage",
                    attribute.name.as_str()
                ),
                attribute.span,
                snapshot,
            )?;
            diagnostic.data.insert(
                "feature".to_owned(),
                Value::String(format!("attribute @{}", attribute.name.as_str())),
            );
            diagnostic.data.insert(
                "stage".to_owned(),
                Value::Integer(u128::from(attribute_schema.stage)),
            );
            validated.diagnostics.push(diagnostic);
            continue;
        }

        if !target_is_allowed(attribute_schema, target) {
            let code = special_target_error(attribute_schema, target)
                .unwrap_or(Code::InvalidAttributeTarget);
            let message = match code {
                Code::DeriveNotSupported => "derive is not supported for resources".to_owned(),
                Code::WitIncompatibleType => {
                    "repr attributes are incompatible with interface types".to_owned()
                }
                Code::InvalidFlagsEnum => {
                    "flags attributes require an enum inside an interface".to_owned()
                }
                _ => format!(
                    "attribute @{} is not allowed on this target",
                    attribute.name.as_str()
                ),
            };
            validated
                .diagnostics
                .push(diagnostic_at(code, message, attribute.span, snapshot)?);
            continue;
        }

        let Some(mut prepared) = prepare_attribute(
            attribute,
            target,
            symbols,
            snapshot,
            &mut validated.diagnostics,
        )?
        else {
            continue;
        };

        if !attribute_schema.repeatable {
            if let Some(previous_span) = first_by_name.get(&attribute.name).copied() {
                let mut diagnostic = diagnostic_at(
                    Code::DuplicateAttribute,
                    format!("duplicate attribute @{}", attribute.name.as_str()),
                    attribute.span,
                    snapshot,
                )?;
                push_label(
                    &mut diagnostic,
                    previous_span,
                    "first occurrence is here",
                    snapshot,
                )?;
                if attribute.name == AttrName::Derive {
                    let first_index = first_attribute_by_name
                        .get(&attribute.name)
                        .copied()
                        .ok_or(AttrValidationError::FixPlanningFailed)?;
                    let first_attribute = attributes
                        .get(first_index)
                        .ok_or(AttrValidationError::FixPlanningFailed)?;
                    let primary = diagnostic
                        .primary()
                        .cloned()
                        .ok_or(AttrValidationError::FixPlanningFailed)?;
                    let fix = derive_merge_fix(first_attribute, attribute, snapshot, &primary)?;
                    diagnostic
                        .push_fix(fix)
                        .map_err(|_| AttrValidationError::FixPlanningFailed)?;
                }
                validated.diagnostics.push(diagnostic);
                continue;
            }
        }

        drop_duplicate_arguments(
            &mut prepared,
            &mut seen_lint_codes,
            &mut first_lint_level_by_code,
            symbols,
            snapshot,
            &mut validated.diagnostics,
        )?;

        if !attribute_schema.repeatable {
            first_by_name.insert(attribute.name, attribute.span);
            first_attribute_by_name
                .entry(attribute.name)
                .or_insert(attribute_index);
        }

        if let Some(previous) = accepted
            .iter()
            .find(|previous| conflicts_with(attribute_schema, &prepared, attribute.name, previous))
        {
            let mut diagnostic = diagnostic_at(
                Code::ConflictingAttributes,
                format!(
                    "attribute @{} conflicts with @{}",
                    attribute.name.as_str(),
                    previous.name.as_str()
                ),
                attribute.span,
                snapshot,
            )?;
            push_label(
                &mut diagnostic,
                previous.span,
                "conflicting attribute is here",
                snapshot,
            )?;
            validated.diagnostics.push(diagnostic);
            continue;
        }

        let Some(prepared) =
            validate_derive_names(prepared, symbols, snapshot, &mut validated.diagnostics)?
        else {
            continue;
        };
        let Some(prepared) =
            validate_lint_names(prepared, symbols, snapshot, &mut validated.diagnostics)?
        else {
            continue;
        };
        apply_prepared(&mut validated, attribute, prepared.clone());
        accepted.push(AcceptedAttribute {
            name: attribute.name,
            span: attribute.span,
            inline: prepared.inline(),
        });
    }

    validated.diagnostics.sort_by_key(|diagnostic| {
        diagnostic
            .primary()
            .map(|primary| primary.span.range.start())
    });
    Ok(validated)
}

#[derive(Clone, Debug)]
struct AcceptedAttribute {
    name: AttrName,
    span: Span,
    inline: Option<InlineHint>,
}

#[derive(Clone, Debug)]
enum PreparedAttribute {
    Derives(Vec<(Symbol, Span)>),
    ValidatedDerives(Vec<(LangItem, Span)>),
    ExportName(ValidatedExportName),
    Inline(InlineHint),
    Cold,
    MustUse(Option<String>),
    Deprecated(Option<String>, Option<(String, Span)>),
    Lints(Vec<LintAttr>),
    Repr(ReprAttr),
    Flags,
    Target(PendingTarget),
    ValidatedTarget(TargetPredicate),
    DocHidden,
}

impl PreparedAttribute {
    fn inline(&self) -> Option<InlineHint> {
        match self {
            Self::Inline(hint) => Some(*hint),
            _ => None,
        }
    }
}

fn validate_derive_names(
    prepared: PreparedAttribute,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<PreparedAttribute>, AttrValidationError> {
    let PreparedAttribute::Derives(names) = prepared else {
        return Ok(Some(prepared));
    };
    let mut derives = Vec::with_capacity(names.len());
    let mut has_unknown = false;
    for (name, span) in names {
        let spelling = symbols
            .resolve(name)
            .map_err(|_| AttrValidationError::ForeignSymbol)?;
        let Some(item) = derive_lang_item(&spelling) else {
            let mut diagnostic = diagnostic_at(
                Code::UnknownDerive,
                format!("unknown derive `{spelling}`"),
                span,
                snapshot,
            )?;
            if let Some(suggestion) = unique_derive_suggestion(&spelling) {
                let primary = diagnostic
                    .primary()
                    .cloned()
                    .ok_or(AttrValidationError::FixPlanningFailed)?;
                let fix = sai_syntax::token_replacement_fix(
                    snapshot,
                    span.file,
                    span.range,
                    suggestion,
                    format!("replace with `{suggestion}`"),
                    &primary,
                )
                .map_err(|_| AttrValidationError::FixPlanningFailed)?;
                diagnostic
                    .push_fix(fix)
                    .map_err(|_| AttrValidationError::FixPlanningFailed)?;
            }
            diagnostics.push(diagnostic);
            has_unknown = true;
            continue;
        };
        derives.push((item, span));
    }
    if has_unknown {
        Ok(None)
    } else {
        Ok(Some(PreparedAttribute::ValidatedDerives(derives)))
    }
}

fn validate_lint_names(
    prepared: PreparedAttribute,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<PreparedAttribute>, AttrValidationError> {
    let PreparedAttribute::Lints(lints) = prepared else {
        return Ok(Some(prepared));
    };
    let mut retained = Vec::with_capacity(lints.len());

    for lint in lints {
        let spelling = symbols
            .resolve(lint.name)
            .map_err(|_| AttrValidationError::ForeignSymbol)?;
        match sai_diagnostics::lookup_lint_name(&spelling) {
            LintNameLookup::Lint(_) => retained.push(lint),
            LintNameLookup::HardDiagnostic(severity) => {
                let mut diagnostic = diagnostic_at(
                    Code::NotALint,
                    format!("`{spelling}` is a hard diagnostic, not a lint"),
                    lint.span,
                    snapshot,
                )?;
                let help = if severity == Severity::Warning {
                    "hard warnings have a fixed level; use `--deny-warnings` to report them as errors"
                } else {
                    "hard diagnostics have a fixed severity and cannot be controlled by lint levels"
                };
                diagnostic
                    .push_child(Child {
                        kind: ChildKind::Help,
                        message: help.to_owned(),
                        location: None,
                    })
                    .map_err(|_| AttrValidationError::SnapshotMismatch)?;
                diagnostics.push(diagnostic);
            }
            LintNameLookup::Unknown => {
                let candidates = lint_name_candidates(&spelling);
                let mut diagnostic = diagnostic_at(
                    Code::UnknownLint,
                    format!("unknown lint `{spelling}`"),
                    lint.span,
                    snapshot,
                )?;
                diagnostic
                    .data
                    .insert("name".to_owned(), Value::String(spelling.to_string()));
                diagnostic.data.insert(
                    "candidates".to_owned(),
                    Value::Array(
                        candidates
                            .iter()
                            .map(|(name, _)| Value::String((*name).to_owned()))
                            .collect(),
                    ),
                );
                if !candidates.is_empty() {
                    diagnostic
                        .push_child(Child {
                            kind: ChildKind::Help,
                            message: format!(
                                "possible lint names: {}",
                                candidates
                                    .iter()
                                    .map(|(name, _)| *name)
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ),
                            location: None,
                        })
                        .map_err(|_| AttrValidationError::SnapshotMismatch)?;
                }

                let uppercase = spelling.to_ascii_uppercase();
                if LINT_REGISTRY
                    .iter()
                    .any(|candidate| candidate.code == uppercase)
                {
                    let primary = diagnostic
                        .primary()
                        .cloned()
                        .ok_or(AttrValidationError::FixPlanningFailed)?;
                    let fix = sai_syntax::token_replacement_fix(
                        snapshot,
                        lint.span.file,
                        lint.span.range,
                        &uppercase,
                        format!("replace with `{uppercase}`"),
                        &primary,
                    )
                    .map_err(|_| AttrValidationError::FixPlanningFailed)?;
                    diagnostic
                        .push_fix(fix)
                        .map_err(|_| AttrValidationError::SnapshotMismatch)?;
                }
                diagnostics.push(diagnostic);
            }
        }
    }

    Ok(Some(PreparedAttribute::Lints(retained)))
}

fn lint_name_candidates(name: &str) -> Vec<(&'static str, usize)> {
    let mut candidates = LINT_REGISTRY
        .iter()
        .filter_map(|candidate| {
            let distance = damerau_levenshtein(name, candidate.code);
            (distance <= 2).then_some((candidate.code, distance))
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|(_, distance)| *distance);
    candidates.truncate(10);
    candidates
}

pub(crate) fn derive_lang_item(name: &str) -> Option<LangItem> {
    Some(match name {
        "Copy" => LangItem::Copy,
        "Clone" => LangItem::Clone,
        "PartialEq" => LangItem::PartialEq,
        "Eq" => LangItem::Eq,
        "PartialOrd" => LangItem::PartialOrd,
        "Ord" => LangItem::Ord,
        "Hash" => LangItem::Hash,
        "Default" => LangItem::Default,
        "Arbitrary" => LangItem::Arbitrary,
        "Add" => LangItem::Add,
        "Sub" => LangItem::Sub,
        "Mul" => LangItem::Mul,
        "Div" => LangItem::Div,
        "Rem" => LangItem::Rem,
        "Neg" => LangItem::Neg,
        "BitAnd" => LangItem::BitAnd,
        "BitOr" => LangItem::BitOr,
        "BitXor" => LangItem::BitXor,
        "BitNot" => LangItem::BitNot,
        _ => return None,
    })
}

const DERIVE_NAMES: &[&str] = &[
    "Copy",
    "Clone",
    "PartialEq",
    "Eq",
    "PartialOrd",
    "Ord",
    "Hash",
    "Default",
    "Arbitrary",
    "Add",
    "Sub",
    "Mul",
    "Div",
    "Rem",
    "Neg",
    "BitAnd",
    "BitOr",
    "BitXor",
    "BitNot",
];

fn unique_derive_suggestion(name: &str) -> Option<&'static str> {
    let name_chars = name.chars().count();
    let mut suggestions = DERIVE_NAMES
        .iter()
        .copied()
        .filter(|candidate| name_chars.abs_diff(candidate.chars().count()) <= 2)
        .filter(|candidate| damerau_levenshtein(name, candidate) <= 2);
    let suggestion = suggestions.next()?;
    suggestions.next().is_none().then_some(suggestion)
}

fn damerau_levenshtein(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let max_distance = left.len().saturating_add(right.len());
    let mut distances = vec![vec![0; right.len() + 2]; left.len() + 2];
    distances[0][0] = max_distance;
    for row in 0..=left.len() {
        distances[row + 1][0] = max_distance;
        distances[row + 1][1] = row;
    }
    for column in 0..=right.len() {
        distances[0][column + 1] = max_distance;
        distances[1][column + 1] = column;
    }

    let mut last_row = HashMap::new();
    for row in 1..=left.len() {
        let mut last_match_column = 0;
        for column in 1..=right.len() {
            let prior_row = last_row.get(&right[column - 1]).copied().unwrap_or(0);
            let prior_column = last_match_column;
            let mut substitution_cost = 1;
            if left[row - 1] == right[column - 1] {
                substitution_cost = 0;
                last_match_column = column;
            }

            distances[row + 1][column + 1] = [
                distances[row][column] + substitution_cost,
                distances[row + 1][column] + 1,
                distances[row][column + 1] + 1,
                distances[prior_row][prior_column]
                    + (row - prior_row - 1)
                    + 1
                    + (column - prior_column - 1),
            ]
            .into_iter()
            .min()
            .unwrap_or(max_distance);
        }
        last_row.insert(left[row - 1], row);
    }
    distances[left.len() + 1][right.len() + 1]
}

fn derive_merge_fix(
    first: &RawAttr,
    duplicate: &RawAttr,
    snapshot: &VfsWorkspaceSnapshot,
    primary: &Location,
) -> Result<Fix, AttrValidationError> {
    let file = snapshot
        .file(first.span.file)
        .filter(|_| first.span.file == duplicate.span.file)
        .ok_or(AttrValidationError::MissingFile)?;
    let source = file.source().text();
    let lexed = sai_syntax::lex(snapshot, first.span.file)
        .map_err(|_| AttrValidationError::FixPlanningFailed)?;
    let first_tokens = derive_attribute_tokens(&lexed, source, first.span)?;
    let duplicate_tokens = derive_attribute_tokens(&lexed, source, duplicate.span)?;

    let second_args = source
        .get(
            duplicate_tokens.open.range.end().to_usize()
                ..duplicate_tokens.close.range.start().to_usize(),
        )
        .ok_or(AttrValidationError::InvalidSourceSpan)?;
    if second_args.trim().is_empty() {
        return Err(AttrValidationError::FixPlanningFailed);
    }
    let trivia_after_at = source
        .get(duplicate_tokens.at_end.to_usize()..duplicate_tokens.name_start.to_usize())
        .ok_or(AttrValidationError::InvalidSourceSpan)?;
    let trivia_before_args = source
        .get(duplicate_tokens.name_end.to_usize()..duplicate_tokens.open.range.start().to_usize())
        .ok_or(AttrValidationError::InvalidSourceSpan)?;
    let first_has_trailing_comma = lexed
        .tokens
        .iter()
        .rfind(|token| {
            token.range.start() >= first_tokens.open.range.end()
                && token.range.end() <= first_tokens.close.range.start()
        })
        .is_some_and(|token| token.kind == sai_syntax::SyntaxKind::COMMA);
    let separator = if first_has_trailing_comma { " " } else { ", " };
    let inserted_text = format!("{separator}{trivia_after_at}{trivia_before_args}{second_args}")
        .replace("\r\n", "\n")
        .replace('\r', "\n");
    let insertion_range = sai_vfs::TextRange::new(
        first_tokens.close.range.start(),
        first_tokens.close.range.start(),
    )
    .ok_or(AttrValidationError::FixPlanningFailed)?;
    let first_name = NodeId::new(NodeGroup::Token, first_tokens.name_index);
    let duplicate_name = NodeId::new(NodeGroup::Token, duplicate_tokens.name_index);
    let operation = FixOperation::new(
        FixOperationName::MergeDeriveAttributes,
        duplicate_name,
        ValueObject::from([("into".to_owned(), Value::String(first_name.to_string()))]),
    )
    .map_err(|_| AttrValidationError::FixPlanningFailed)?;
    let planner = DeriveMergeFixPlanner {
        file: first.span.file,
        first_name,
        duplicate_name,
        insertion_range,
        duplicate_range: duplicate.span.range,
        inserted_text,
    };
    Fix::new(
        "merge duplicate @derive attributes",
        Applicability::MachineApplicable,
        operation,
        Vec::new(),
        primary,
        snapshot,
        &planner,
    )
    .map_err(|_| AttrValidationError::FixPlanningFailed)
}

struct DeriveAttributeTokens {
    at_end: sai_vfs::TextSize,
    name_index: u64,
    name_start: sai_vfs::TextSize,
    name_end: sai_vfs::TextSize,
    open: sai_syntax::Token,
    close: sai_syntax::Token,
}

fn derive_attribute_tokens(
    lexed: &sai_syntax::LexOutput,
    source: &str,
    span: Span,
) -> Result<DeriveAttributeTokens, AttrValidationError> {
    let tokens_in_attribute = lexed.tokens.iter().enumerate().filter(|(_, token)| {
        token.range.start() >= span.range.start() && token.range.end() <= span.range.end()
    });
    let mut at_end = None;
    let mut name_index = None;
    let mut name_start = None;
    let mut name_end = None;
    let mut open = None;
    let mut close = None;
    for (index, token) in tokens_in_attribute {
        match token.kind {
            sai_syntax::SyntaxKind::AT if at_end.is_none() => at_end = Some(token.range.end()),
            sai_syntax::SyntaxKind::LIDENT | sai_syntax::SyntaxKind::UIDENT
                if name_index.is_none()
                    && source.get(token.range.start().to_usize()..token.range.end().to_usize())
                        == Some("derive") =>
            {
                name_index =
                    Some(u64::try_from(index).map_err(|_| AttrValidationError::FixPlanningFailed)?);
                name_start = Some(token.range.start());
                name_end = Some(token.range.end());
            }
            sai_syntax::SyntaxKind::L_PAREN if open.is_none() => open = Some(token.clone()),
            sai_syntax::SyntaxKind::R_PAREN => close = Some(token.clone()),
            _ => {}
        }
    }
    let at_end = at_end.ok_or(AttrValidationError::FixPlanningFailed)?;
    let name_index = name_index.ok_or(AttrValidationError::FixPlanningFailed)?;
    let name_start = name_start.ok_or(AttrValidationError::FixPlanningFailed)?;
    let name_end = name_end.ok_or(AttrValidationError::FixPlanningFailed)?;
    let open = open.ok_or(AttrValidationError::FixPlanningFailed)?;
    let close = close.ok_or(AttrValidationError::FixPlanningFailed)?;
    if open.range.start() >= close.range.start() {
        return Err(AttrValidationError::FixPlanningFailed);
    }
    Ok(DeriveAttributeTokens {
        at_end,
        name_index,
        name_start,
        name_end,
        open,
        close,
    })
}

struct DeriveMergeFixPlanner {
    file: FileId,
    first_name: NodeId,
    duplicate_name: NodeId,
    insertion_range: sai_vfs::TextRange,
    duplicate_range: sai_vfs::TextRange,
    inserted_text: String,
}

impl DeriveMergeFixPlanner {
    fn edits(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<TextEdit>, OperationPlanError> {
        let file = snapshot
            .file(self.file)
            .ok_or(OperationPlanError::TargetNotFound)?;
        let duplicate_original = file
            .source()
            .to_original_range(self.duplicate_range)
            .ok_or(OperationPlanError::NotApplicable)?;
        let into = operation.args().get("into");
        if operation.name() != FixOperationName::MergeDeriveAttributes
            || operation.node() != self.duplicate_name
            || into != Some(&Value::String(self.first_name.to_string()))
            || primary.span.file != self.file
            || primary.span.range != duplicate_original
        {
            return Err(OperationPlanError::NotApplicable);
        }
        let source = SourceRef {
            path: file.path().to_owned(),
            package: file.package().map(str::to_owned),
            snapshot: file.document_version(),
        };
        let insertion = file
            .source()
            .to_original_range(self.insertion_range)
            .ok_or(OperationPlanError::NotApplicable)?;
        Ok(vec![
            TextEdit {
                file: source.clone(),
                range: Span::new(self.file, insertion),
                new_text: self.inserted_text.clone(),
            },
            TextEdit {
                file: source,
                range: Span::new(self.file, duplicate_original),
                new_text: String::new(),
            },
        ])
    }
}

impl FixOperationPlanner for DeriveMergeFixPlanner {
    fn apply_operation(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<std::collections::BTreeMap<FileId, Vec<u8>>, OperationPlanError> {
        let edits = self.edits(operation, primary, snapshot)?;
        let file = snapshot
            .file(self.file)
            .ok_or(OperationPlanError::TargetNotFound)?;
        let mut bytes = file.source().raw_bytes().to_vec();
        let crlf = sai_diagnostics::first_line_uses_crlf(&bytes);
        for edit in edits.iter().rev() {
            let start = edit.range.range.start().to_usize();
            let end = edit.range.range.end().to_usize();
            let replacement = if crlf {
                edit.new_text.replace('\n', "\r\n").into_bytes()
            } else {
                edit.new_text.as_bytes().to_vec()
            };
            if start > end || end > bytes.len() {
                return Err(OperationPlanError::NotApplicable);
            }
            bytes.splice(start..end, replacement);
        }
        Ok(std::collections::BTreeMap::from([(self.file, bytes)]))
    }

    fn plan_edits(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<TextEdit>, OperationPlanError> {
        self.edits(operation, primary, snapshot)
    }
}

fn conflicts_with(
    attribute_schema: &AttrSchema,
    prepared: &PreparedAttribute,
    name: AttrName,
    previous: &AcceptedAttribute,
) -> bool {
    let conflict = attribute_schema
        .conflicts
        .iter()
        .find(|conflict| conflict.other == previous.name);
    let Some(conflict) = conflict else {
        return false;
    };
    match conflict.condition {
        ConflictCondition::Always => true,
        ConflictCondition::InlineIsHint => {
            if name == AttrName::Inline {
                prepared.inline() == Some(InlineHint::Hint)
            } else if previous.name == AttrName::Inline {
                previous.inline == Some(InlineHint::Hint)
            } else {
                false
            }
        }
    }
}

fn target_is_allowed(attribute_schema: &AttrSchema, target: AttributeTarget) -> bool {
    attribute_schema.targets.iter().any(|rule| {
        if rule.kind != target.kind {
            return false;
        }
        match rule.condition {
            TargetCondition::Any => true,
            TargetCondition::ModuleLevel => target.module_level,
            TargetCondition::HasBody => target.has_body,
            TargetCondition::IsExported => target.is_exported,
            TargetCondition::InherentImpl => target.impl_kind == Some(ImplKind::Inherent),
            TargetCondition::ExternWasmOrUnsafeC => {
                target.extern_abi == Some(ExternAbi::Wasm)
                    || (target.extern_abi == Some(ExternAbi::C) && target.extern_unsafe)
            }
            TargetCondition::ExternWasm => target.extern_abi == Some(ExternAbi::Wasm),
            TargetCondition::InsideInterface => target.inside_interface,
            TargetCondition::TraitDeclaration => target.assoc_type_in_trait,
        }
    })
}

fn special_target_error(attribute_schema: &AttrSchema, target: AttributeTarget) -> Option<Code> {
    attribute_schema
        .special_target_cases
        .iter()
        .find_map(|special_case| match special_case {
            SpecialTargetCase::DeriveOnResource if target.kind == TargetKind::Resource => {
                Some(Code::DeriveNotSupported)
            }
            SpecialTargetCase::ReprInsideInterface
                if target.inside_interface
                    && matches!(target.kind, TargetKind::Record | TargetKind::Enum) =>
            {
                Some(Code::WitIncompatibleType)
            }
            SpecialTargetCase::FlagsOutsideInterface
                if target.kind == TargetKind::Enum && !target.inside_interface =>
            {
                Some(Code::InvalidFlagsEnum)
            }
            _ => None,
        })
}

fn diagnostic_at(
    code: Code,
    message: impl Into<String>,
    span: Span,
    snapshot: &WorkspaceSnapshot,
) -> Result<Diagnostic, AttrValidationError> {
    let file = snapshot
        .file(span.file)
        .ok_or(AttrValidationError::MissingFile)?;
    let location = Location::from_decoded_source(
        SourceRef {
            path: file.path().to_owned(),
            package: file.package().map(str::to_owned),
            snapshot: file.document_version(),
        },
        span,
        file.source(),
    )
    .ok_or(AttrValidationError::InvalidSourceSpan)?;
    Diagnostic::new(code, Severity::Error, message, Some(location))
        .with_workspace_snapshot(snapshot)
        .map_err(|_| AttrValidationError::SnapshotMismatch)
}

fn push_label(
    diagnostic: &mut Diagnostic,
    span: Span,
    message: &'static str,
    snapshot: &WorkspaceSnapshot,
) -> Result<(), AttrValidationError> {
    let file = snapshot
        .file(span.file)
        .ok_or(AttrValidationError::MissingFile)?;
    let location = Location::from_decoded_source(
        SourceRef {
            path: file.path().to_owned(),
            package: file.package().map(str::to_owned),
            snapshot: file.document_version(),
        },
        span,
        file.source(),
    )
    .ok_or(AttrValidationError::InvalidSourceSpan)?;
    diagnostic
        .push_label(Label {
            location,
            message: message.to_owned(),
        })
        .map_err(|_| AttrValidationError::SnapshotMismatch)
}

fn prepare_attribute(
    attribute: &RawAttr,
    target: AttributeTarget,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<PreparedAttribute>, AttrValidationError> {
    let invalid = |span, expected: &'static str, diagnostics: &mut Vec<Diagnostic>| {
        diagnostic_at(
            Code::InvalidAttributeArgument,
            format!(
                "invalid arguments to @{}; expected {expected}",
                attribute.name.as_str()
            ),
            span,
            snapshot,
        )
        .map(|diagnostic| diagnostics.push(diagnostic))
    };

    let prepared = match attribute.name {
        AttrName::ExportName => match attribute.args.as_deref() {
            Some([RawAttrArg::Str { value, span }]) if !value.is_empty() => {
                Some(PreparedAttribute::ExportName(ValidatedExportName {
                    value: value.clone(),
                    string_span: *span,
                }))
            }
            Some([RawAttrArg::Str { span, .. }]) => {
                invalid(*span, "one non-empty string", diagnostics)?;
                None
            }
            _ => {
                invalid(
                    invalid_argument_span(attribute),
                    "one non-empty string",
                    diagnostics,
                )?;
                None
            }
        },
        AttrName::Inline => match attribute.args.as_deref() {
            None => Some(PreparedAttribute::Inline(InlineHint::Hint)),
            Some(
                [
                    RawAttrArg::Word {
                        name,
                        raw: false,
                        args: None,
                        ..
                    },
                ],
            ) if symbols
                .resolve(*name)
                .map_err(|_| AttrValidationError::ForeignSymbol)?
                .as_ref()
                == "never" =>
            {
                Some(PreparedAttribute::Inline(InlineHint::Never))
            }
            _ => {
                invalid(
                    invalid_argument_span(attribute),
                    "no arguments or the single word `never`",
                    diagnostics,
                )?;
                None
            }
        },
        AttrName::Cold => {
            if attribute.args.is_none() {
                Some(PreparedAttribute::Cold)
            } else {
                invalid(
                    invalid_argument_span(attribute),
                    "no arguments",
                    diagnostics,
                )?;
                None
            }
        }
        AttrName::MustUse => match attribute.args.as_deref() {
            None => Some(PreparedAttribute::MustUse(None)),
            Some([RawAttrArg::Str { value, .. }]) if !value.is_empty() => {
                Some(PreparedAttribute::MustUse(Some(value.clone())))
            }
            Some([RawAttrArg::Str { span, .. }]) => {
                invalid(*span, "no arguments or one non-empty string", diagnostics)?;
                None
            }
            _ => {
                invalid(
                    invalid_argument_span(attribute),
                    "no arguments or one non-empty string",
                    diagnostics,
                )?;
                None
            }
        },
        AttrName::Deprecated => parse_deprecated(attribute, symbols, snapshot, diagnostics)?,
        AttrName::Allow | AttrName::Warn | AttrName::Deny => {
            let Some(arguments) = attribute.args.as_deref() else {
                invalid(
                    attribute.span,
                    "one or more plain lint-code identifiers",
                    diagnostics,
                )?;
                return Ok(None);
            };
            if arguments.is_empty() {
                invalid(
                    attribute.span,
                    "one or more plain lint-code identifiers",
                    diagnostics,
                )?;
                return Ok(None);
            }

            let level = match attribute.name {
                AttrName::Allow => DiagnosticLintLevel::Allow,
                AttrName::Warn => DiagnosticLintLevel::Warn,
                AttrName::Deny => DiagnosticLintLevel::Deny,
                _ => unreachable!("lint attribute group was matched above"),
            };
            let mut lint_arguments = Vec::new();
            for argument in arguments {
                match argument {
                    RawAttrArg::Word {
                        name,
                        raw: false,
                        args: None,
                        span,
                        ..
                    } => {
                        symbols
                            .resolve(*name)
                            .map_err(|_| AttrValidationError::ForeignSymbol)?;
                        lint_arguments.push(LintAttr {
                            level,
                            name: *name,
                            span: *span,
                        });
                    }
                    _ => {
                        invalid(
                            argument_span(argument),
                            "one or more plain lint-code identifiers",
                            diagnostics,
                        )?;
                    }
                }
            }
            Some(PreparedAttribute::Lints(lint_arguments))
        }
        AttrName::Repr => {
            let Some(
                [
                    RawAttrArg::Word {
                        name,
                        raw: false,
                        args: None,
                        span,
                        ..
                    },
                ],
            ) = attribute.args.as_deref()
            else {
                invalid(
                    invalid_argument_span(attribute),
                    "one plain word: `C` or a fixed-width integer type",
                    diagnostics,
                )?;
                return Ok(None);
            };
            let spelling = symbols
                .resolve(*name)
                .map_err(|_| AttrValidationError::ForeignSymbol)?;
            let repr = match spelling.as_ref() {
                "C" => ReprAttr::C,
                "I8" => ReprAttr::Integer(ReprInteger::I8),
                "I16" => ReprAttr::Integer(ReprInteger::I16),
                "I32" => ReprAttr::Integer(ReprInteger::I32),
                "I64" => ReprAttr::Integer(ReprInteger::I64),
                "U8" => ReprAttr::Integer(ReprInteger::U8),
                "U16" => ReprAttr::Integer(ReprInteger::U16),
                "U32" => ReprAttr::Integer(ReprInteger::U32),
                "U64" => ReprAttr::Integer(ReprInteger::U64),
                _ => {
                    invalid(*span, "`C` or a fixed-width integer type", diagnostics)?;
                    return Ok(None);
                }
            };
            if target.kind == TargetKind::Record && repr != ReprAttr::C {
                invalid(
                    *span,
                    "`C` on records, or `C` or a fixed-width integer type on enums",
                    diagnostics,
                )?;
                None
            } else {
                Some(PreparedAttribute::Repr(repr))
            }
        }
        AttrName::Target => {
            let Some([argument]) = attribute.args.as_deref() else {
                invalid(
                    invalid_argument_span(attribute),
                    "one target name, target capability, or all/any/none predicate",
                    diagnostics,
                )?;
                return Ok(None);
            };
            let Some(predicate) = parse_pending_target(argument, symbols, snapshot, diagnostics)?
            else {
                return Ok(None);
            };
            Some(PreparedAttribute::Target(predicate))
        }
        AttrName::Doc => match attribute.args.as_deref() {
            Some(
                [
                    RawAttrArg::Word {
                        name,
                        raw: false,
                        args: None,
                        ..
                    },
                ],
            ) if symbols
                .resolve(*name)
                .map_err(|_| AttrValidationError::ForeignSymbol)?
                .as_ref()
                == "hidden" =>
            {
                Some(PreparedAttribute::DocHidden)
            }
            _ => {
                invalid(
                    invalid_argument_span(attribute),
                    "the single word `hidden`",
                    diagnostics,
                )?;
                None
            }
        },
        AttrName::Derive => {
            let Some(arguments) = attribute.args.as_deref() else {
                invalid(
                    attribute.span,
                    "one or more plain derive names",
                    diagnostics,
                )?;
                return Ok(None);
            };
            if arguments.is_empty() {
                invalid(
                    attribute.span,
                    "one or more plain derive names",
                    diagnostics,
                )?;
                return Ok(None);
            }

            let mut derives = Vec::with_capacity(arguments.len());
            for argument in arguments {
                match argument {
                    RawAttrArg::Word {
                        name,
                        raw: false,
                        is_upper: true,
                        args: None,
                        span,
                        ..
                    } => {
                        symbols
                            .resolve(*name)
                            .map_err(|_| AttrValidationError::ForeignSymbol)?;
                        derives.push((*name, *span));
                    }
                    _ => {
                        invalid(
                            argument_span(argument),
                            "one or more plain derive names",
                            diagnostics,
                        )?;
                        return Ok(None);
                    }
                }
            }
            Some(PreparedAttribute::Derives(derives))
        }
        AttrName::Flags => {
            if attribute.args.is_none() {
                Some(PreparedAttribute::Flags)
            } else {
                invalid(
                    invalid_argument_span(attribute),
                    "no arguments",
                    diagnostics,
                )?;
                None
            }
        }
        AttrName::ImportName | AttrName::Cases | AttrName::Seed | AttrName::PanicHook => {
            unreachable!("attributes above the implemented stage are gated earlier")
        }
    };
    Ok(prepared)
}

fn parse_deprecated(
    attribute: &RawAttr,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<PreparedAttribute>, AttrValidationError> {
    let Some(arguments) = attribute.args.as_deref() else {
        return Ok(Some(PreparedAttribute::Deprecated(None, None)));
    };
    if arguments.is_empty() || arguments.len() > 2 {
        push_invalid_attribute_argument(
            attribute,
            attribute.span,
            "no arguments, one non-empty message, instead = a non-empty path, or both",
            snapshot,
            diagnostics,
        )?;
        return Ok(None);
    }

    let mut message = None;
    let mut instead = None;
    let mut instead_seen = false;
    for (index, argument) in arguments.iter().enumerate() {
        match argument {
            RawAttrArg::Str { value, span: _ } if index == 0 && !value.is_empty() => {
                message = Some(value.clone());
            }
            RawAttrArg::KeyValue {
                key,
                raw,
                value: RawAttrValue::Str(path),
                value_span,
                span,
            } => {
                let key = symbols
                    .resolve(*key)
                    .map_err(|_| AttrValidationError::ForeignSymbol)?;
                if *raw || key.as_ref() != "instead" || instead_seen {
                    push_invalid_attribute_argument(
                        attribute,
                        *span,
                        "one plain `instead =` key with a non-empty string",
                        snapshot,
                        diagnostics,
                    )?;
                    return Ok(None);
                }
                instead_seen = true;
                if path.is_empty() {
                    push_invalid_attribute_argument(
                        attribute,
                        *value_span,
                        "a non-empty `instead` path",
                        snapshot,
                        diagnostics,
                    )?;
                    continue;
                }
                instead = Some((path.clone(), *value_span));
            }
            RawAttrArg::Str { span, .. } => {
                push_invalid_attribute_argument(
                    attribute,
                    *span,
                    "a non-empty message before `instead =`",
                    snapshot,
                    diagnostics,
                )?;
                return Ok(None);
            }
            _ => {
                push_invalid_attribute_argument(
                    attribute,
                    argument_span(argument),
                    "a non-empty message or `instead =` with a non-empty string",
                    snapshot,
                    diagnostics,
                )?;
                return Ok(None);
            }
        }
    }

    Ok(Some(PreparedAttribute::Deprecated(message, instead)))
}

fn drop_duplicate_arguments(
    prepared: &mut PreparedAttribute,
    seen_lint_codes: &mut HashSet<(DiagnosticLintLevel, Symbol)>,
    first_lint_level_by_code: &mut HashMap<Symbol, (DiagnosticLintLevel, Span)>,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), AttrValidationError> {
    match prepared {
        PreparedAttribute::Derives(derives) => {
            let mut seen = HashSet::new();
            let mut retained = Vec::with_capacity(derives.len());
            for (name, span) in std::mem::take(derives) {
                if seen.insert(name) {
                    retained.push((name, span));
                } else {
                    diagnostics.push(diagnostic_at(
                        Code::DuplicateAttributeArgument,
                        "duplicate derive name",
                        span,
                        snapshot,
                    )?);
                }
            }
            *derives = retained;
        }
        PreparedAttribute::Lints(lints) => {
            let mut retained = Vec::with_capacity(lints.len());
            for lint in std::mem::take(lints) {
                if !seen_lint_codes.insert((lint.level, lint.name)) {
                    diagnostics.push(diagnostic_at(
                        Code::DuplicateAttributeArgument,
                        "duplicate lint code at the same level on this target",
                        lint.span,
                        snapshot,
                    )?);
                    continue;
                }

                if let Some((previous_level, previous_span)) =
                    first_lint_level_by_code.get(&lint.name).copied()
                {
                    if previous_level != lint.level {
                        let name = symbols
                            .resolve(lint.name)
                            .map_err(|_| AttrValidationError::ForeignSymbol)?;
                        let mut diagnostic = diagnostic_at(
                            Code::ConflictingLintLevel,
                            format!("conflicting levels for lint `{name}` on this target"),
                            lint.span,
                            snapshot,
                        )?;
                        push_label(
                            &mut diagnostic,
                            previous_span,
                            "first level is here",
                            snapshot,
                        )?;
                        diagnostics.push(diagnostic);
                        continue;
                    }
                } else {
                    first_lint_level_by_code.insert(lint.name, (lint.level, lint.span));
                }

                retained.push(lint);
            }
            *lints = retained;
        }
        PreparedAttribute::Target(pending) => {
            let predicate = finalize_target(pending.clone(), snapshot, diagnostics)?;
            *prepared = PreparedAttribute::ValidatedTarget(predicate);
        }
        _ => {}
    }
    Ok(())
}

fn apply_prepared(
    validated: &mut ValidatedAttrs,
    attribute: &RawAttr,
    prepared: PreparedAttribute,
) {
    match prepared {
        PreparedAttribute::ValidatedDerives(derives) => {
            validated.derives.extend(derives);
        }
        PreparedAttribute::ExportName(export_name) => {
            validated.export_name = Some(export_name);
        }
        PreparedAttribute::Inline(hint) => validated.inline = Some((hint, attribute.span)),
        PreparedAttribute::Cold => validated.cold = Some(attribute.span),
        PreparedAttribute::MustUse(reason) => {
            validated.must_use = Some((reason, attribute.span));
        }
        PreparedAttribute::Deprecated(message, instead) => {
            let (instead, instead_span) =
                instead.map_or((None, None), |(path, span)| (Some(path), Some(span)));
            validated.deprecated = Some(Deprecation {
                message,
                instead,
                instead_span,
                span: attribute.span,
            });
        }
        PreparedAttribute::Lints(lints) => validated.lint_levels.extend(lints),
        PreparedAttribute::Repr(repr) => validated.repr = Some((repr, attribute.span)),
        PreparedAttribute::Flags => validated.flags = Some(attribute.span),
        PreparedAttribute::ValidatedTarget(predicate) => {
            validated.target = Some((predicate, attribute.span));
        }
        PreparedAttribute::DocHidden => validated.doc_hidden = Some(attribute.span),
        PreparedAttribute::Derives(_) | PreparedAttribute::Target(_) => {
            unreachable!("attributes are finalized before application")
        }
    }
}

fn push_invalid_attribute_argument(
    attribute: &RawAttr,
    span: Span,
    expected: &'static str,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<(), AttrValidationError> {
    diagnostics.push(diagnostic_at(
        Code::InvalidAttributeArgument,
        format!(
            "invalid arguments to @{}; expected {expected}",
            attribute.name.as_str()
        ),
        span,
        snapshot,
    )?);
    Ok(())
}

fn invalid_argument_span(attribute: &RawAttr) -> Span {
    attribute
        .args
        .as_ref()
        .and_then(|args| args.first())
        .map(argument_span)
        .unwrap_or(attribute.span)
}

fn argument_span(argument: &RawAttrArg) -> Span {
    match argument {
        RawAttrArg::Word { span, .. }
        | RawAttrArg::Str { span, .. }
        | RawAttrArg::Int { span, .. }
        | RawAttrArg::KeyValue { span, .. } => *span,
    }
}

#[derive(Clone, Debug)]
enum PendingTarget {
    Target(String),
    Capability(String),
    All(Vec<(Self, Span)>),
    Any(Vec<(Self, Span)>),
    None(Vec<(Self, Span)>),
}

fn parse_pending_target(
    argument: &RawAttrArg,
    symbols: &Interner,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Option<PendingTarget>, AttrValidationError> {
    match argument {
        RawAttrArg::Str { value, span } => {
            if known_target(value) {
                Ok(Some(PendingTarget::Target(value.clone())))
            } else {
                diagnostics.push(diagnostic_at(
                    Code::UnknownTarget,
                    format!("unknown target `{value}`"),
                    *span,
                    snapshot,
                )?);
                Ok(None)
            }
        }
        RawAttrArg::Word {
            name,
            raw: false,
            args,
            span,
            ..
        } => {
            let spelling = symbols
                .resolve(*name)
                .map_err(|_| AttrValidationError::ForeignSymbol)?;
            match args {
                None if is_target_capability(&spelling) => {
                    Ok(Some(PendingTarget::Capability(spelling.to_string())))
                }
                None => {
                    let mut diagnostic = diagnostic_at(
                        Code::InvalidAttributeArgument,
                        format!("unknown target capability `{spelling}`"),
                        *span,
                        snapshot,
                    )?;
                    diagnostic
                        .push_child(Child {
                            kind: ChildKind::Help,
                            message: "a target predicate can test only target names and target capabilities"
                                .to_owned(),
                            location: None,
                        })
                        .map_err(|_| AttrValidationError::SnapshotMismatch)?;
                    diagnostics.push(diagnostic);
                    Ok(None)
                }
                Some(arguments) if matches!(spelling.as_ref(), "all" | "any" | "none") => {
                    if arguments.is_empty() {
                        diagnostics.push(diagnostic_at(
                            Code::InvalidAttributeArgument,
                            format!("{} requires one or more target predicates", spelling),
                            *span,
                            snapshot,
                        )?);
                        return Ok(None);
                    }

                    let mut operands = Vec::with_capacity(arguments.len());
                    for operand in arguments {
                        let operand_span = argument_span(operand);
                        let Some(operand) =
                            parse_pending_target(operand, symbols, snapshot, diagnostics)?
                        else {
                            return Ok(None);
                        };
                        operands.push((operand, operand_span));
                    }
                    Ok(Some(match spelling.as_ref() {
                        "all" => PendingTarget::All(operands),
                        "any" => PendingTarget::Any(operands),
                        "none" => PendingTarget::None(operands),
                        _ => unreachable!("combinator names were matched above"),
                    }))
                }
                Some(_) => {
                    diagnostics.push(diagnostic_at(
                        Code::InvalidAttributeArgument,
                        format!("`{spelling}` is not a target-predicate combinator"),
                        *span,
                        snapshot,
                    )?);
                    Ok(None)
                }
            }
        }
        _ => {
            diagnostics.push(diagnostic_at(
                Code::InvalidAttributeArgument,
                "expected a target name, target capability, or target-predicate combinator",
                argument_span(argument),
                snapshot,
            )?);
            Ok(None)
        }
    }
}

fn finalize_target(
    pending: PendingTarget,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<TargetPredicate, AttrValidationError> {
    match pending {
        PendingTarget::Target(target) => Ok(TargetPredicate::Target(target)),
        PendingTarget::Capability(capability) => Ok(TargetPredicate::Capability(capability)),
        PendingTarget::All(operands) => Ok(TargetPredicate::All(finalize_operands(
            operands,
            snapshot,
            diagnostics,
        )?)),
        PendingTarget::Any(operands) => Ok(TargetPredicate::Any(finalize_operands(
            operands,
            snapshot,
            diagnostics,
        )?)),
        PendingTarget::None(operands) => {
            let mut predicates = finalize_operands(operands, snapshot, diagnostics)?;
            let negated = match predicates.len() {
                1 => predicates.pop().expect("length was checked"),
                _ => TargetPredicate::Any(predicates),
            };
            Ok(TargetPredicate::Not(Box::new(negated)))
        }
    }
}

fn finalize_operands(
    operands: Vec<(PendingTarget, Span)>,
    snapshot: &WorkspaceSnapshot,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<TargetPredicate>, AttrValidationError> {
    let mut predicates = Vec::with_capacity(operands.len());
    let mut spellings = HashSet::with_capacity(operands.len());
    for (operand, span) in operands {
        let predicate = finalize_target(operand, snapshot, diagnostics)?;
        let file = snapshot
            .file(span.file)
            .ok_or(AttrValidationError::MissingFile)?;
        let start = span.range.start().to_usize();
        let end = span.range.end().to_usize();
        let spelling = file
            .source()
            .text()
            .get(start..end)
            .ok_or(AttrValidationError::InvalidSourceSpan)?;
        if !spellings.insert(spelling.to_owned()) {
            diagnostics.push(diagnostic_at(
                Code::DuplicateAttributeArgument,
                "duplicate target-predicate operand",
                span,
                snapshot,
            )?);
        } else {
            predicates.push(predicate);
        }
    }
    Ok(predicates)
}

fn known_target(name: &str) -> bool {
    target_pointer_width(name).is_some()
}

fn target_pointer_width(name: &str) -> Option<u8> {
    matches!(name, "wasm32-core" | "wasm32-wasi")
        .then_some(32)
        .or_else(|| {
            matches!(
                name,
                "wasm64-core"
                    | "wasm64-wasi"
                    | "x86_64-linux"
                    | "aarch64-linux"
                    | "aarch64-macos"
                    | "x86_64-windows"
            )
            .then_some(64)
        })
}

fn is_target_capability(name: &str) -> bool {
    matches!(
        name,
        "threads" | "host" | "native_ffi" | "process" | "memory64"
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_syntax::ATTRIBUTE_NAMES;
    use sai_vfs::{SnapshotFile, SourceFile};
    use std::sync::Arc;

    fn function_target() -> AttributeTarget {
        AttributeTarget {
            module_level: true,
            has_body: true,
            ..AttributeTarget::new(TargetKind::Fn)
        }
    }

    fn exported_function_target() -> AttributeTarget {
        AttributeTarget {
            is_exported: true,
            ..function_target()
        }
    }

    fn validate_source(
        source: &str,
        target: AttributeTarget,
    ) -> (ValidatedAttrs, WorkspaceSnapshot) {
        let file = sai_vfs::FileId::new(41);
        let source_file = Arc::new(SourceFile::new(file, source, false).expect("valid source"));
        validate_source_file(source_file, target)
    }

    fn validate_source_file(
        source_file: Arc<SourceFile>,
        target: AttributeTarget,
    ) -> (ValidatedAttrs, WorkspaceSnapshot) {
        let (attrs, symbols, snapshot) = collect_raw_attrs(source_file);
        let validated =
            validate_attrs(&attrs, Some(target), &symbols, &snapshot).expect("validation");
        (validated, snapshot)
    }

    fn validate_source_at_stage(
        source: &str,
        target: AttributeTarget,
        implemented_stage: u8,
    ) -> (ValidatedAttrs, WorkspaceSnapshot) {
        let file = sai_vfs::FileId::new(41);
        let source_file = Arc::new(SourceFile::new(file, source, false).expect("valid source"));
        let (attrs, symbols, snapshot) = collect_raw_attrs(source_file);
        let validated =
            validate_attrs_at_stage(&attrs, Some(target), &symbols, &snapshot, implemented_stage)
                .expect("validation");
        (validated, snapshot)
    }

    fn collect_raw_attrs(
        source_file: Arc<SourceFile>,
    ) -> (Vec<RawAttr>, Interner, WorkspaceSnapshot) {
        let file = source_file.id();
        let source = source_file.text().to_owned();
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source_file, "src/lib.sai", None, Some(7))])
                .expect("valid snapshot");
        let parsed = sai_syntax::parse(&snapshot, file).expect("parse succeeds");
        let ids = sai_syntax::ast::AstIdMap::new(file, &parsed.root).expect("AST IDs");
        let symbols = Interner::new();
        let tree = crate::item_tree::ItemTree::collect(file, &source, &parsed.root, &ids, &symbols)
            .expect("item tree");
        let attrs = tree.items.first().expect("one item").raw_attrs.clone();
        (attrs, symbols, snapshot)
    }

    #[test]
    fn schemas_cover_all_attributes_in_the_shared_source_name_order() {
        assert_eq!(ATTRIBUTE_SCHEMAS.len(), ATTRIBUTE_NAMES.len());
        for (attribute_name, item_schema) in ATTRIBUTE_NAMES.iter().zip(&ATTRIBUTE_SCHEMAS) {
            assert_eq!(
                AttrName::from_source_name(attribute_name),
                Some(item_schema.name)
            );
            assert_eq!(schema(item_schema.name).name, item_schema.name);
            assert!(!item_schema.targets.is_empty(), "{attribute_name}");
        }
    }

    #[test]
    fn stages_match_the_attribute_specification() {
        let stages = [
            (AttrName::Derive, 2),
            (AttrName::ExportName, 1),
            (AttrName::Inline, 0),
            (AttrName::ImportName, 1),
            (AttrName::Cold, 0),
            (AttrName::Repr, 0),
            (AttrName::MustUse, 0),
            (AttrName::Flags, 4),
            (AttrName::Deprecated, 0),
            (AttrName::Target, 0),
            (AttrName::Allow, 0),
            (AttrName::Cases, 3),
            (AttrName::Warn, 0),
            (AttrName::Seed, 3),
            (AttrName::Deny, 0),
            (AttrName::Doc, 0),
            (AttrName::PanicHook, 1),
        ];
        for (name, stage) in stages {
            assert_eq!(schema(name).stage, stage, "{name:?}");
        }
    }

    #[test]
    fn derives_validate_every_fixed_name_in_source_order() {
        let (validated, snapshot) = validate_source_at_stage(
            "@derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Arbitrary, Add, Sub, Mul, Div, Rem, Neg, BitAnd, BitOr, BitXor, BitNot)\ntype Value { field: I32 }",
            AttributeTarget::new(TargetKind::Record),
            2,
        );

        assert_eq!(
            validated
                .derives
                .iter()
                .map(|(item, _)| *item)
                .collect::<Vec<_>>(),
            [
                LangItem::Copy,
                LangItem::Clone,
                LangItem::PartialEq,
                LangItem::Eq,
                LangItem::PartialOrd,
                LangItem::Ord,
                LangItem::Hash,
                LangItem::Default,
                LangItem::Arbitrary,
                LangItem::Add,
                LangItem::Sub,
                LangItem::Mul,
                LangItem::Div,
                LangItem::Rem,
                LangItem::Neg,
                LangItem::BitAnd,
                LangItem::BitOr,
                LangItem::BitXor,
                LangItem::BitNot,
            ]
        );
        assert!(validated.diagnostics.is_empty());
        assert_eq!(
            validated.derives[0].1.range.start().to_usize(),
            "@derive(".len()
        );
        assert_eq!(
            snapshot
                .file(sai_vfs::FileId::new(41))
                .expect("file")
                .source()
                .text()
                .get(
                    validated.derives[0].1.range.start().to_usize()
                        ..validated.derives[0].1.range.end().to_usize()
                ),
            Some("Copy")
        );
    }

    #[test]
    fn duplicate_derive_names_are_dropped_but_unknown_names_drop_the_attribute() {
        let (duplicate, snapshot) = validate_source_at_stage(
            "@derive(Clone, Clone)\ntype Value { field: I32 }",
            AttributeTarget::new(TargetKind::Record),
            2,
        );
        assert_eq!(
            duplicate
                .derives
                .iter()
                .map(|(item, _)| *item)
                .collect::<Vec<_>>(),
            [LangItem::Clone]
        );
        assert_eq!(
            duplicate
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::DuplicateAttributeArgument]
        );
        assert_eq!(
            snapshot
                .file(sai_vfs::FileId::new(41))
                .expect("file")
                .source()
                .text()
                .get(
                    duplicate.diagnostics[0]
                        .primary()
                        .expect("primary")
                        .span
                        .range
                        .start()
                        .to_usize()
                        ..duplicate.diagnostics[0]
                            .primary()
                            .expect("primary")
                            .span
                            .range
                            .end()
                            .to_usize()
                ),
            Some("Clone")
        );

        let (unknown, snapshot) = validate_source_at_stage(
            "@derive(Clnoe, Clone)\ntype Value { field: I32 }",
            AttributeTarget::new(TargetKind::Record),
            2,
        );
        assert!(unknown.derives.is_empty());
        assert_eq!(
            unknown
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            [Code::UnknownDerive]
        );
        assert_eq!(
            snapshot
                .file(sai_vfs::FileId::new(41))
                .expect("file")
                .source()
                .text()
                .get(
                    unknown.diagnostics[0]
                        .primary()
                        .expect("primary")
                        .span
                        .range
                        .start()
                        .to_usize()
                        ..unknown.diagnostics[0]
                            .primary()
                            .expect("primary")
                            .span
                            .range
                            .end()
                            .to_usize()
                ),
            Some("Clnoe")
        );
        assert_eq!(unknown.diagnostics[0].fixes().len(), 1);
        let fix = &unknown.diagnostics[0].fixes()[0];
        assert_eq!(fix.title(), "replace with `Clone`");
        assert_eq!(
            fix.applicability(),
            sai_diagnostics::Applicability::MachineApplicable
        );
        assert_eq!(fix.edits()[0].new_text, "Clone");

        let (unicode_typo, _) = validate_source_at_stage(
            "@derive(Clonあ)\ntype Value { field: I32 }",
            AttributeTarget::new(TargetKind::Record),
            2,
        );
        assert_eq!(unicode_typo.diagnostics[0].code, Code::UnknownDerive);
        assert_eq!(
            unicode_typo.diagnostics[0].fixes()[0].title(),
            "replace with `Clone`"
        );
    }

    #[test]
    fn duplicate_derive_attributes_offer_a_machine_applicable_merge() {
        let source = "@derive(Clone,)\n@ /* keep this leading note */ derive /* keep this note */ (Copy, PartialEq,)\ntype Value { field: I32 }";
        let (validated, snapshot) =
            validate_source_at_stage(source, AttributeTarget::new(TargetKind::Record), 2);
        assert_eq!(
            validated
                .derives
                .iter()
                .map(|(item, _)| *item)
                .collect::<Vec<_>>(),
            [LangItem::Clone]
        );
        assert_eq!(validated.diagnostics[0].code, Code::DuplicateAttribute);
        let fix = validated.diagnostics[0]
            .fixes()
            .first()
            .expect("duplicate derive has a merge fix");
        assert_eq!(fix.title(), "merge duplicate @derive attributes");
        assert_eq!(
            fix.applicability(),
            sai_diagnostics::Applicability::MachineApplicable
        );
        assert_eq!(
            fix.operation().name(),
            FixOperationName::MergeDeriveAttributes
        );
        assert_eq!(fix.edits().len(), 2);
        assert_eq!(
            fix.edits()[0].new_text,
            "  /* keep this leading note */  /* keep this note */ Copy, PartialEq,"
        );
        assert_eq!(fix.edits()[1].new_text, "");

        let mut bytes = snapshot
            .file(sai_vfs::FileId::new(41))
            .expect("file")
            .source()
            .raw_bytes()
            .to_vec();
        for edit in fix.edits().iter().rev() {
            bytes.splice(
                edit.range.range.start().to_usize()..edit.range.range.end().to_usize(),
                edit.new_text.bytes(),
            );
        }
        let merged = String::from_utf8(bytes).expect("merged source is utf-8");
        assert!(merged.contains(
            "@derive(Clone,  /* keep this leading note */  /* keep this note */ Copy, PartialEq,)"
        ));
        assert_eq!(merged.matches("@derive").count(), 1);
    }

    #[test]
    fn duplicate_derive_merge_uses_the_first_line_ending_in_mixed_line_files() {
        let source = "@derive(Clone)\n@derive(Copy,\r\nPartialEq)\ntype Value { field: I32 }";
        let (validated, snapshot) =
            validate_source_at_stage(source, AttributeTarget::new(TargetKind::Record), 2);
        let fix = validated.diagnostics[0]
            .fixes()
            .first()
            .expect("duplicate derive has a merge fix");
        assert!(!sai_diagnostics::first_line_uses_crlf(
            snapshot
                .file(sai_vfs::FileId::new(41))
                .expect("file")
                .source()
                .raw_bytes()
        ));

        let mut bytes = snapshot
            .file(sai_vfs::FileId::new(41))
            .expect("file")
            .source()
            .raw_bytes()
            .to_vec();
        for edit in fix.edits().iter().rev() {
            bytes.splice(
                edit.range.range.start().to_usize()..edit.range.range.end().to_usize(),
                edit.new_text.bytes(),
            );
        }
        let merged = String::from_utf8(bytes).expect("merged source is utf-8");
        assert!(merged.contains("@derive(Clone, Copy,\nPartialEq)"));
        assert_eq!(merged.matches("@derive").count(), 1);
    }

    #[test]
    fn derive_keeps_stage_gate_and_rejects_invalid_argument_shapes() {
        let (stage_zero, _) = validate_source(
            "@derive(Clone)\ntype Value { field: I32 }",
            AttributeTarget::new(TargetKind::Record),
        );
        assert!(stage_zero.derives.is_empty());
        assert_eq!(stage_zero.diagnostics[0].code, Code::FeatureNotImplemented);

        for source in [
            "@derive()\ntype Value { field: I32 }",
            "@derive(\"Clone\")\ntype Value { field: I32 }",
            "@derive(Clone(Inner))\ntype Value { field: I32 }",
            "@derive(clone)\ntype Value { field: I32 }",
        ] {
            let (validated, _) =
                validate_source_at_stage(source, AttributeTarget::new(TargetKind::Record), 2);
            assert!(validated.derives.is_empty(), "{source}");
            assert!(
                validated
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == Code::InvalidAttributeArgument),
                "{source}"
            );
        }
    }

    #[test]
    fn argument_grammars_match_the_attribute_specification() {
        let grammars = [
            (AttrName::Derive, ArgGrammar::DeriveList),
            (AttrName::ExportName, ArgGrammar::NonEmptyString),
            (AttrName::Inline, ArgGrammar::Inline),
            (AttrName::ImportName, ArgGrammar::NonEmptyString),
            (AttrName::Cold, ArgGrammar::None),
            (AttrName::Repr, ArgGrammar::Repr),
            (AttrName::MustUse, ArgGrammar::OptionalNonEmptyString),
            (AttrName::Flags, ArgGrammar::None),
            (AttrName::Deprecated, ArgGrammar::Deprecated),
            (AttrName::Target, ArgGrammar::TargetPredicate),
            (AttrName::Allow, ArgGrammar::LintCodes),
            (
                AttrName::Cases,
                ArgGrammar::IntegerRange {
                    min: 1,
                    max: u32::MAX as u64,
                },
            ),
            (AttrName::Warn, ArgGrammar::LintCodes),
            (
                AttrName::Seed,
                ArgGrammar::IntegerRange {
                    min: 0,
                    max: u64::MAX,
                },
            ),
            (AttrName::Deny, ArgGrammar::LintCodes),
            (AttrName::Doc, ArgGrammar::Word("hidden")),
            (AttrName::PanicHook, ArgGrammar::None),
        ];
        for (name, grammar) in grammars {
            assert_eq!(schema(name).args, grammar, "{name:?}");
        }
    }

    #[test]
    fn target_rules_capture_contextual_restrictions() {
        let has_rule = |name, kind, condition| {
            schema(name)
                .targets
                .iter()
                .any(|rule| rule.kind == kind && rule.condition == condition)
        };

        assert!(has_rule(
            AttrName::Inline,
            TargetKind::TraitFn,
            TargetCondition::HasBody
        ));
        assert!(has_rule(
            AttrName::ExportName,
            TargetKind::Fn,
            TargetCondition::IsExported
        ));
        assert!(has_rule(
            AttrName::Deprecated,
            TargetKind::ImplFn,
            TargetCondition::InherentImpl
        ));
        assert!(has_rule(
            AttrName::ImportName,
            TargetKind::ExternFn,
            TargetCondition::ExternWasmOrUnsafeC
        ));
        assert!(has_rule(
            AttrName::PanicHook,
            TargetKind::ExternFn,
            TargetCondition::ExternWasm
        ));
        assert!(has_rule(
            AttrName::Flags,
            TargetKind::Enum,
            TargetCondition::InsideInterface
        ));

        assert_eq!(
            schema(AttrName::Derive).special_target_cases,
            &[SpecialTargetCase::DeriveOnResource]
        );
        assert_eq!(
            schema(AttrName::Repr).special_target_cases,
            &[SpecialTargetCase::ReprInsideInterface]
        );
        assert_eq!(
            schema(AttrName::Flags).special_target_cases,
            &[SpecialTargetCase::FlagsOutsideInterface]
        );

        let module_target_kinds = schema(AttrName::Target)
            .targets
            .iter()
            .map(|rule| rule.kind)
            .collect::<Vec<_>>();
        assert!(!module_target_kinds.contains(&TargetKind::Interface));
        assert!(!module_target_kinds.contains(&TargetKind::World));
        assert!(!module_target_kinds.contains(&TargetKind::Field));
    }

    #[test]
    fn repeatability_and_conditional_conflicts_match_the_specification() {
        for name in [AttrName::Allow, AttrName::Warn, AttrName::Deny] {
            assert!(schema(name).repeatable, "{name:?}");
        }
        for item in &ATTRIBUTE_SCHEMAS {
            if !matches!(item.name, AttrName::Allow | AttrName::Warn | AttrName::Deny) {
                assert!(!item.repeatable, "{:?}", item.name);
            }
        }

        assert_eq!(
            schema(AttrName::Inline).conflicts,
            &[AttributeConflict::new(
                AttrName::Cold,
                ConflictCondition::InlineIsHint
            )]
        );
        assert_eq!(
            schema(AttrName::Cold).conflicts,
            &[AttributeConflict::new(
                AttrName::Inline,
                ConflictCondition::InlineIsHint
            )]
        );
        assert_eq!(
            schema(AttrName::Flags).conflicts,
            &[AttributeConflict::new(
                AttrName::Derive,
                ConflictCondition::Always
            )]
        );
        assert_eq!(
            schema(AttrName::Derive).conflicts,
            &[AttributeConflict::new(
                AttrName::Flags,
                ConflictCondition::Always
            )]
        );
        assert_eq!(
            schema(AttrName::Derive).special_target_cases,
            &[SpecialTargetCase::DeriveOnResource]
        );
        assert_eq!(
            schema(AttrName::Repr).special_target_cases,
            &[SpecialTargetCase::ReprInsideInterface]
        );
        assert_eq!(
            schema(AttrName::Flags).special_target_cases,
            &[SpecialTargetCase::FlagsOutsideInterface]
        );
    }

    #[test]
    fn all_documented_target_kinds_are_represented() {
        assert_eq!(TargetKind::ALL.len(), 25);
        for (index, kind) in TargetKind::ALL.iter().enumerate() {
            assert_eq!(*kind as usize, index);
        }
    }

    #[test]
    fn later_stage_attributes_are_rejected_before_target_and_argument_checks() {
        let (validated, snapshot) = validate_source("@derive()\nfn f() {}\n", function_target());
        assert_eq!(validated.diagnostics.len(), 1);
        let diagnostic = &validated.diagnostics[0];
        assert_eq!(diagnostic.code, Code::FeatureNotImplemented);
        assert_eq!(
            diagnostic.data.get("feature"),
            Some(&Value::String("attribute @derive".to_owned()))
        );
        assert_eq!(diagnostic.data.get("stage"), Some(&Value::Integer(2)));
        assert!(
            diagnostic
                .clone()
                .with_workspace_snapshot(&snapshot)
                .is_ok()
        );
    }

    #[test]
    fn flags_marker_is_retained_only_when_stage_validation_accepts_it() {
        let mut target = AttributeTarget::new(TargetKind::Enum);
        target.inside_interface = true;

        let (derive_then_flags, _) =
            validate_source_at_stage("@derive(Clone)\n@flags\nenum Bits { One }\n", target, 4);
        assert_eq!(derive_then_flags.flags, None);
        assert_eq!(derive_then_flags.derives.len(), 1);
        assert!(
            derive_then_flags
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ConflictingAttributes)
        );

        let (flags_then_derive, _) =
            validate_source_at_stage("@flags\n@derive(Clone)\nenum Bits { One }\n", target, 4);
        assert!(flags_then_derive.flags.is_some());
        assert!(flags_then_derive.derives.is_empty());
        assert!(
            flags_then_derive
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::ConflictingAttributes)
        );

        let (accepted_flags, _) =
            validate_source_at_stage("@flags\nenum Bits { One }\n", target, 4);
        assert!(accepted_flags.flags.is_some());
        assert!(accepted_flags.diagnostics.is_empty());

        let (stage_zero, _) = validate_source_at_stage("@flags\nenum Bits { One }\n", target, 0);
        assert_eq!(stage_zero.flags, None);
        assert!(
            stage_zero
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == Code::FeatureNotImplemented)
        );

        let outside_interface = AttributeTarget::new(TargetKind::Enum);
        let (outside_interface, _) =
            validate_source_at_stage("@flags\nenum Bits { One }\n", outside_interface, 4);
        assert_eq!(outside_interface.flags, None);
        let outside_flags = outside_interface
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.code == Code::InvalidFlagsEnum)
            .expect("flags outside an interface have their dedicated diagnostic");
        let location = outside_flags
            .primary()
            .expect("flags target diagnostic has a source location");
        assert_eq!(location.span.range.start().raw(), 0);
        assert_eq!(location.span.range.end().raw(), "@flags".len() as u32);

        for source in [
            "@flags()\nenum Bits { One }\n",
            "@flags(foo)\nenum Bits { One }\n",
        ] {
            let (malformed_flags, _) = validate_source_at_stage(source, target, 4);
            assert_eq!(malformed_flags.flags, None, "{source}");
            assert!(
                malformed_flags
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == Code::InvalidAttributeArgument),
                "{source}"
            );
        }
    }

    #[test]
    fn stage_zero_attributes_decode_into_validated_values() {
        let source = concat!(
            "@inline(never)\n",
            "@must_use(\"keep the result\")\n",
            "@deprecated(\"old\", instead = \"new\")\n",
            "@allow(MISSING_DOCS, MISSING_DOCS)\n",
            "fn f() {}\n",
        );
        let (validated, _) = validate_source(source, function_target());
        assert_eq!(
            validated.inline.map(|(hint, _)| hint),
            Some(InlineHint::Never)
        );
        assert_eq!(
            validated
                .must_use
                .as_ref()
                .map(|(reason, _)| reason.as_deref()),
            Some(Some("keep the result"))
        );
        let deprecation = validated.deprecated.as_ref().expect("deprecation");
        assert_eq!(deprecation.message.as_deref(), Some("old"));
        assert_eq!(deprecation.instead.as_deref(), Some("new"));
        assert!(deprecation.instead_span.is_some());
        assert_eq!(validated.lint_levels.len(), 1);
        assert_eq!(validated.diagnostics.len(), 1);
        assert_eq!(
            validated.diagnostics[0].code,
            Code::DuplicateAttributeArgument
        );
    }

    #[test]
    fn export_name_api_decodes_only_export_name_without_changing_stage_zero() {
        let source = concat!(
            "@export_name(\"wire\\u{3A}name\")\n",
            "@import_name(\"ignored\")\n",
            "@panic_hook\n",
            "@inline\n",
            "fn f() {}\n",
        );
        let (attributes, symbols, snapshot) = collect_raw_attrs(Arc::new(
            SourceFile::new(sai_vfs::FileId::new(46), source, false).expect("source"),
        ));

        let export_name = validate_export_name(
            &attributes,
            Some(exported_function_target()),
            &symbols,
            &snapshot,
        )
        .expect("narrow export-name validation");
        let value = export_name.export_name.expect("validated export name");
        assert_eq!(value.value, "wire:name");
        let string_start = u32::try_from(source.find("\"wire\\u{3A}name\"").expect("string token"))
            .expect("source offset fits");
        assert_eq!(
            value.string_span.range.start(),
            sai_vfs::TextSize::new(string_start)
        );
        assert_eq!(
            value.string_span.range.end(),
            sai_vfs::TextSize::new(
                string_start + u32::try_from("\"wire\\u{3A}name\"".len()).expect("length fits")
            )
        );
        assert!(export_name.diagnostics.is_empty());
        assert_eq!(IMPLEMENTED_ATTRIBUTE_STAGE, 0);

        let stage_zero = validate_attrs(
            &attributes,
            Some(exported_function_target()),
            &symbols,
            &snapshot,
        )
        .expect("ordinary stage-zero validation");
        assert!(stage_zero.export_name.is_none());
        assert_eq!(
            stage_zero.inline.map(|(hint, _)| hint),
            Some(InlineHint::Hint)
        );
        assert_eq!(
            stage_zero
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.code == Code::FeatureNotImplemented)
                .count(),
            3,
            "the general validator must continue gating Stage 1 attributes"
        );
    }

    #[test]
    fn export_name_api_rejects_empty_and_malformed_arguments() {
        for attribute in [
            "@export_name(\"\")",
            "@export_name",
            "@export_name()",
            "@export_name(42)",
            "@export_name(\"one\", \"two\")",
        ] {
            let source = format!("{attribute}\nfn f() {{}}\n");
            let (attributes, symbols, snapshot) = collect_raw_attrs(Arc::new(
                SourceFile::new(sai_vfs::FileId::new(47), source.as_str(), false).expect("source"),
            ));
            let validation = validate_export_name(
                &attributes,
                Some(exported_function_target()),
                &symbols,
                &snapshot,
            )
            .expect("validation");
            assert!(validation.export_name.is_none(), "{attribute}");
            assert_eq!(validation.diagnostics.len(), 1, "{attribute}");
            assert_eq!(
                validation.diagnostics[0].code,
                Code::InvalidAttributeArgument,
                "{attribute}"
            );
        }
    }

    #[test]
    fn export_name_api_checks_export_target_and_duplicate_attributes() {
        let source = "@export_name(\"first\")\n@export_name(\"second\")\nfn f() {}\n";
        let (attributes, symbols, snapshot) = collect_raw_attrs(Arc::new(
            SourceFile::new(sai_vfs::FileId::new(48), source, false).expect("source"),
        ));

        let mis_target = validate_export_name(
            &attributes[..1],
            Some(function_target()),
            &symbols,
            &snapshot,
        )
        .expect("mis-target validation");
        assert!(mis_target.export_name.is_none());
        assert_eq!(mis_target.diagnostics.len(), 1);
        assert_eq!(mis_target.diagnostics[0].code, Code::InvalidAttributeTarget);

        let repeated = validate_export_name(
            &attributes,
            Some(exported_function_target()),
            &symbols,
            &snapshot,
        )
        .expect("repeated validation");
        assert_eq!(
            repeated.export_name.expect("first valid attribute").value,
            "first"
        );
        assert_eq!(repeated.diagnostics.len(), 1);
        assert_eq!(repeated.diagnostics[0].code, Code::DuplicateAttribute);
    }

    #[test]
    fn export_name_api_discards_error_marked_attributes() {
        let source = "@export_name(\"otherwise valid\")\nfn f() {}\n";
        let (mut attributes, symbols, snapshot) = collect_raw_attrs(Arc::new(
            SourceFile::new(sai_vfs::FileId::new(49), source, false).expect("source"),
        ));
        let export_name = attributes.first_mut().expect("attribute");
        assert_eq!(export_name.name, AttrName::ExportName);
        export_name.has_error = true;

        let validation = validate_export_name(
            &attributes,
            Some(exported_function_target()),
            &symbols,
            &snapshot,
        )
        .expect("error-marked validation");
        assert!(validation.export_name.is_none());
        assert!(validation.diagnostics.is_empty());
    }

    #[test]
    fn inline_and_cold_require_bodies_for_all_function_target_kinds() {
        let targets = [
            AttributeTarget {
                kind: TargetKind::Fn,
                module_level: true,
                ..AttributeTarget::new(TargetKind::Fn)
            },
            AttributeTarget {
                kind: TargetKind::TraitFn,
                ..AttributeTarget::new(TargetKind::TraitFn)
            },
            AttributeTarget {
                kind: TargetKind::ImplFn,
                impl_kind: Some(ImplKind::Inherent),
                ..AttributeTarget::new(TargetKind::ImplFn)
            },
        ];

        for target in targets {
            for name in ["inline", "cold"] {
                let source = format!("@{name}\nfn f() {{}}\n");
                let (validated, _) = validate_source(&source, target);
                assert_eq!(validated.diagnostics.len(), 1, "{name} on {target:?}");
                assert_eq!(
                    validated.diagnostics[0].code,
                    Code::InvalidAttributeTarget,
                    "{name} on {target:?}"
                );
            }
        }
    }

    #[test]
    fn lint_attribute_shape_rejects_non_name_arguments_and_keeps_valid_lints() {
        let source = Arc::new(
            SourceFile::new(
                sai_vfs::FileId::new(45),
                "@allow(UNUSED_VARIABLE, \"not-a-name\", UNUSED_IMPORT)\nfn f() {}\n",
                false,
            )
            .expect("source"),
        );
        let (attrs, symbols, snapshot) = collect_raw_attrs(source);
        let validated = validate_attrs(&attrs, Some(function_target()), &symbols, &snapshot)
            .expect("validation");

        assert_eq!(validated.diagnostics.len(), 1);
        assert_eq!(
            validated.diagnostics[0].code,
            Code::InvalidAttributeArgument
        );
        assert_eq!(validated.lint_levels.len(), 2);
        assert_eq!(
            symbols
                .resolve(validated.lint_levels[0].name)
                .expect("lint symbol")
                .as_ref(),
            "UNUSED_VARIABLE"
        );
        assert_eq!(
            symbols
                .resolve(validated.lint_levels[1].name)
                .expect("lint symbol")
                .as_ref(),
            "UNUSED_IMPORT"
        );
    }

    #[test]
    fn lint_name_validation_distinguishes_unknown_and_hard_codes() {
        let source = concat!(
            "@allow(UNUSED_VARIABLES, UNUSED_MUST_USE, MISPLACED_DOC_COMMENT, MISSING_SAFETY_DOC, unused_must_use)\n",
            "fn f() {}\n",
        );
        let (validated, _) = validate_source(source, function_target());

        assert_eq!(validated.lint_levels.len(), 1);
        assert_eq!(validated.diagnostics.len(), 4);
        assert_eq!(validated.diagnostics[0].code, Code::UnknownLint);
        assert_eq!(
            validated.diagnostics[0].data.get("candidates"),
            Some(&Value::Array(vec![Value::String(
                "UNUSED_VARIABLE".to_owned()
            )]))
        );
        assert!(
            validated.diagnostics[0].children()[0]
                .message
                .contains("UNUSED_VARIABLE")
        );
        assert_eq!(validated.diagnostics[1].code, Code::NotALint);
        assert!(
            validated.diagnostics[1].children()[0]
                .message
                .contains("fixed severity")
        );
        assert_eq!(validated.diagnostics[2].code, Code::NotALint);
        assert!(
            validated.diagnostics[2].children()[0]
                .message
                .contains("--deny-warnings")
        );
        assert_eq!(validated.diagnostics[3].code, Code::UnknownLint);
        assert!(validated.diagnostics[3].fixes().is_empty());
        assert_eq!(
            validated.diagnostics[3].data.get("candidates"),
            Some(&Value::Array(Vec::new()))
        );

        let expected_name_start = u32::try_from(
            source
                .find("MISSING_SAFETY_DOC")
                .expect("later-stage lint name"),
        )
        .expect("source offset fits");
        assert_eq!(
            validated.diagnostics[0]
                .primary()
                .expect("unknown lint has a primary")
                .span
                .range
                .start(),
            sai_vfs::TextSize::new(
                u32::try_from(source.find("UNUSED_VARIABLES").expect("unknown name"))
                    .expect("source offset fits")
            )
        );
        assert_eq!(
            validated.lint_levels[0].span.range.start(),
            sai_vfs::TextSize::new(expected_name_start)
        );
    }

    #[test]
    fn lowercase_known_lint_gets_a_machine_applicable_case_fix() {
        let source = "@allow(unused_variable)\nfn f() {}\n";
        let (validated, _) = validate_source(source, function_target());

        assert!(validated.lint_levels.is_empty());
        assert_eq!(validated.diagnostics.len(), 1);
        let diagnostic = &validated.diagnostics[0];
        assert_eq!(diagnostic.code, Code::UnknownLint);
        assert_eq!(diagnostic.fixes().len(), 1);
        assert_eq!(
            diagnostic.fixes()[0].applicability(),
            Applicability::MachineApplicable
        );
        assert_eq!(diagnostic.fixes()[0].edits().len(), 1);
        assert_eq!(diagnostic.fixes()[0].edits()[0].new_text, "UNUSED_VARIABLE");
        assert_eq!(
            diagnostic.fixes()[0].edits()[0].range.range,
            sai_vfs::TextRange::new(
                sai_vfs::TextSize::new(
                    u32::try_from(source.find("unused_variable").expect("lowercase name"))
                        .expect("source offset fits")
                ),
                sai_vfs::TextSize::new(
                    u32::try_from(source.find("unused_variable").expect("lowercase name") + 15)
                        .expect("source offset fits")
                )
            )
            .expect("ordered range")
        );
    }

    #[test]
    fn lint_name_candidates_use_the_specified_distance_limit() {
        let (validated, _) = validate_source("@allow(XXXSED_ITEM)\nfn f() {}\n", function_target());

        assert_eq!(validated.diagnostics.len(), 1);
        assert_eq!(validated.diagnostics[0].code, Code::UnknownLint);
        assert_eq!(
            validated.diagnostics[0].data.get("candidates"),
            Some(&Value::Array(Vec::new()))
        );
    }

    #[test]
    fn lint_level_conflicts_report_the_later_level_and_keep_the_first() {
        let source = concat!(
            "@warn(UNUSED_VARIABLE)\n",
            "@deny(UNUSED_VARIABLE, UNUSED_VARIABLE)\n",
            "fn f() {}\n",
        );
        let (validated, _) = validate_source(source, function_target());

        assert_eq!(validated.lint_levels.len(), 1);
        assert_eq!(validated.lint_levels[0].level, DiagnosticLintLevel::Warn);
        assert_eq!(validated.diagnostics.len(), 2);
        assert_eq!(validated.diagnostics[0].code, Code::ConflictingLintLevel);
        assert!(validated.diagnostics[0].message.contains("UNUSED_VARIABLE"));
        assert_eq!(
            validated.diagnostics[0]
                .primary()
                .expect("conflict has a primary")
                .span
                .range
                .start()
                .to_usize(),
            source.find("@deny").expect("later lint level") + "@deny(".len()
        );
        assert_eq!(validated.diagnostics[0].labels().len(), 1);
        let first_code_offset =
            u32::try_from(source.find("UNUSED_VARIABLE").expect("first lint level"))
                .expect("source offset fits");
        assert_eq!(
            validated.diagnostics[0].labels()[0]
                .location
                .span
                .range
                .start(),
            sai_vfs::TextSize::new(first_code_offset)
        );
        assert_eq!(
            validated.diagnostics[1].code,
            Code::DuplicateAttributeArgument
        );
    }

    #[test]
    fn attribute_argument_constraints_and_duplicates_are_diagnosed() {
        let (empty_reason, _) = validate_source("@must_use(\"\")\nfn f() {}\n", function_target());
        assert_eq!(empty_reason.diagnostics.len(), 1);
        assert_eq!(
            empty_reason.diagnostics[0].code,
            Code::InvalidAttributeArgument
        );
        assert!(empty_reason.must_use.is_none());

        let record_target = AttributeTarget {
            kind: TargetKind::Record,
            module_level: true,
            ..AttributeTarget::new(TargetKind::Record)
        };
        let (record_repr, _) = validate_source("@repr(U8)\ntype R {}\n", record_target);
        assert_eq!(record_repr.diagnostics.len(), 1);
        assert_eq!(
            record_repr.diagnostics[0].code,
            Code::InvalidAttributeArgument
        );

        let (duplicate, _) = validate_source("@repr(C)\n@repr(C)\ntype R {}\n", record_target);
        assert_eq!(duplicate.diagnostics.len(), 1);
        assert_eq!(duplicate.diagnostics[0].code, Code::DuplicateAttribute);
        assert_eq!(duplicate.diagnostics[0].labels().len(), 1);
        assert_eq!(duplicate.repr.map(|(repr, _)| repr), Some(ReprAttr::C));
    }

    #[test]
    fn deprecated_invalid_instead_parts_do_not_discard_the_message() {
        let (empty_path, _) = validate_source(
            "@deprecated(\"old\", instead = \"\")\nfn f() {}\n",
            function_target(),
        );
        assert_eq!(empty_path.diagnostics.len(), 1);
        assert_eq!(
            empty_path.diagnostics[0].code,
            Code::InvalidAttributeArgument
        );
        let primary = empty_path.diagnostics[0]
            .primary()
            .expect("primary location");
        let empty_literal_start = "@deprecated(\"old\", instead = \"\")"
            .find("\"\"")
            .expect("empty path literal");
        assert_eq!(primary.span.range.start().to_usize(), empty_literal_start);
        assert_eq!(primary.span.range.end().to_usize(), empty_literal_start + 2);
        let deprecation = empty_path.deprecated.expect("message remains applicable");
        assert_eq!(deprecation.message.as_deref(), Some("old"));
        assert_eq!(deprecation.instead, None);
        assert_eq!(deprecation.instead_span, None);

        let (raw_key, _) = validate_source(
            "@deprecated(r#instead = \"replacement\")\nfn f() {}\n",
            function_target(),
        );
        assert_eq!(raw_key.diagnostics.len(), 1);
        assert_eq!(raw_key.diagnostics[0].code, Code::InvalidAttributeArgument);
        assert!(raw_key.deprecated.is_none());
    }

    #[test]
    fn lexer_errors_on_attribute_arguments_do_not_cascade_into_validation() {
        let source = Arc::new(
            SourceFile::new(
                sai_vfs::FileId::new(44),
                "@must_use(\"\\q\")\nfn f() {}\n",
                false,
            )
            .expect("source"),
        );
        let (attrs, symbols, snapshot) = collect_raw_attrs(source);
        assert!(attrs[0].has_error);

        let validated = validate_attrs(&attrs, Some(function_target()), &symbols, &snapshot)
            .expect("validation");
        assert_eq!(validated, ValidatedAttrs::default());
    }

    #[test]
    fn conditional_inline_conflict_keeps_the_later_non_conflicting_attributes() {
        let (conflict, _) = validate_source("@inline\n@cold\nfn f() {}\n", function_target());
        assert_eq!(conflict.diagnostics.len(), 1);
        assert_eq!(conflict.diagnostics[0].code, Code::ConflictingAttributes);
        assert!(conflict.inline.is_some());
        assert!(conflict.cold.is_none());

        let (allowed_pair, _) =
            validate_source("@cold\n@inline(never)\nfn f() {}\n", function_target());
        assert!(allowed_pair.diagnostics.is_empty());
        assert_eq!(
            allowed_pair.inline.map(|(hint, _)| hint),
            Some(InlineHint::Never)
        );
        assert!(allowed_pair.cold.is_some());
    }

    #[test]
    fn target_predicates_validate_names_capabilities_and_duplicate_operands() {
        let (predicate, _) = validate_source(
            "@target(all(host, host, none(memory64, threads)))\nfn f() {}\n",
            function_target(),
        );
        assert_eq!(predicate.diagnostics.len(), 1);
        assert_eq!(
            predicate.diagnostics[0].code,
            Code::DuplicateAttributeArgument
        );
        assert_eq!(
            predicate.target.as_ref().map(|(predicate, _)| predicate),
            Some(&TargetPredicate::All(vec![
                TargetPredicate::Capability("host".to_owned()),
                TargetPredicate::Not(Box::new(TargetPredicate::Any(vec![
                    TargetPredicate::Capability("memory64".to_owned()),
                    TargetPredicate::Capability("threads".to_owned()),
                ]))),
            ]))
        );

        let (unknown_target, _) =
            validate_source("@target(\"not-a-target\")\nfn f() {}\n", function_target());
        assert_eq!(unknown_target.diagnostics.len(), 1);
        assert_eq!(unknown_target.diagnostics[0].code, Code::UnknownTarget);
        assert!(unknown_target.target.is_none());

        let (unknown_capability, _) =
            validate_source("@target(alloc)\nfn f() {}\n", function_target());
        assert_eq!(
            unknown_capability.diagnostics[0].code,
            Code::InvalidAttributeArgument
        );
        assert!(unknown_capability.target.is_none());
    }

    #[test]
    fn target_predicates_evaluate_target_capabilities_and_combinators() {
        let target = TargetCfg::new("wasm64-core", ["memory64", "host"])
            .expect("capabilities match the target");

        assert!(TargetPredicate::Target("wasm64-core".to_owned()).matches(&target));
        assert!(!TargetPredicate::Target("wasm32-core".to_owned()).matches(&target));
        assert!(TargetPredicate::Capability("memory64".to_owned()).matches(&target));
        assert!(!TargetPredicate::Capability("threads".to_owned()).matches(&target));
        assert!(
            TargetPredicate::All(vec![
                TargetPredicate::Capability("host".to_owned()),
                TargetPredicate::Not(Box::new(TargetPredicate::Capability("threads".to_owned(),))),
            ])
            .matches(&target)
        );
        assert!(
            TargetPredicate::Any(vec![
                TargetPredicate::Target("wasm32-core".to_owned()),
                TargetPredicate::Capability("memory64".to_owned()),
            ])
            .matches(&target)
        );
        assert!(!TargetPredicate::Any(Vec::new()).matches(&target));
        assert!(TargetPredicate::All(Vec::new()).matches(&target));

        assert_eq!(
            TargetCfg::new("wasm32-core", ["memory64"]),
            Err(TargetCfgError::CapabilityUnavailable)
        );
        assert_eq!(
            TargetCfg::new("wasm64-core", [] as [&str; 0]),
            Err(TargetCfgError::MissingRequiredCapability)
        );
        assert_eq!(
            TargetCfg::new("wasm32-core", ["threads"]),
            Err(TargetCfgError::CapabilityUnavailable)
        );
        assert_eq!(
            TargetCfg::new("wasm32-linux", [] as [&str; 0]),
            Err(TargetCfgError::UnknownTarget)
        );
        assert_eq!(
            TargetCfg::new("wasm32-core", ["alloc"]),
            Err(TargetCfgError::UnknownCapability)
        );
        assert_eq!(
            TargetCfg::new("wasm32-core", ["process"]),
            Err(TargetCfgError::CapabilityUnavailable)
        );
    }

    #[test]
    fn target_cfg_pointer_widths_follow_the_target_table() {
        for (name, capabilities, expected_bits) in [
            ("wasm32-core", &["host"][..], 32),
            ("wasm32-wasi", &[][..], 32),
            ("wasm64-core", &["memory64", "host"][..], 64),
            ("wasm64-wasi", &["memory64"][..], 64),
            ("x86_64-linux", &["native_ffi", "process"][..], 64),
            ("aarch64-linux", &["native_ffi", "process"][..], 64),
            ("aarch64-macos", &["native_ffi", "process"][..], 64),
            ("x86_64-windows", &["native_ffi", "process"][..], 64),
        ] {
            let target = TargetCfg::new(name.to_owned(), capabilities.iter().copied())
                .expect("target capabilities are valid");
            assert_eq!(target.pointer_width(), expected_bits, "{name}");
        }
    }

    #[test]
    fn target_duplicate_operands_compare_written_spelling() {
        let (raw_and_quoted, _) = validate_source(
            "@target(any(\"wasm32-core\", r#\"wasm32-core\"#))\nfn f() {}\n",
            function_target(),
        );
        assert!(raw_and_quoted.diagnostics.is_empty());
        assert_eq!(
            raw_and_quoted
                .target
                .as_ref()
                .map(|(predicate, _)| predicate),
            Some(&TargetPredicate::Any(vec![
                TargetPredicate::Target("wasm32-core".to_owned()),
                TargetPredicate::Target("wasm32-core".to_owned()),
            ]))
        );
    }

    #[test]
    fn unknown_target_kinds_suppress_attribute_checks_and_bom_offsets_are_original() {
        let raw_source = b"\xEF\xBB\xBF@must_use(\"\")\nfn f() {}\n";
        let file = sai_vfs::FileId::new(42);
        let source = Arc::new(SourceFile::decode(file, raw_source).expect("decoded source"));
        let (validated, _) = validate_source_file(source, function_target());
        let primary = validated.diagnostics[0]
            .primary()
            .expect("primary location");
        assert_eq!(primary.span.range.start().raw(), 13);

        let source = Arc::new(
            SourceFile::new(sai_vfs::FileId::new(43), "@inline\nfn f() {}\n", false)
                .expect("source"),
        );
        let (attrs, symbols, snapshot) = collect_raw_attrs(source);
        let no_target = validate_attrs(&attrs, None, &symbols, &snapshot).expect("validation");
        assert_eq!(no_target, ValidatedAttrs::default());
    }
}
