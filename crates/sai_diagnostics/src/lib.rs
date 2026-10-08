#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::collections::BTreeSet;

use sai_stable_hash::{StableHash, StableHasher};

mod lint_levels;
pub use lint_levels::{
    LintPolicies, LintResolution, LintResolutionContext, LintResolutionError, ManifestLintLevel,
    OverrideLintLevel, ScopedLintLevel, apply_deny_warnings, resolve_lint_occurrence,
};

pub use sai_vfs::{FileId, Span, TextRange, TextSize, WorkspaceSnapshot, WorkspaceSnapshotId};

include!(concat!(env!("OUT_DIR"), "/diagnostic_codes.rs"));

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LintLevel {
    Allow,
    Info,
    Warn,
    Deny,
}

/// The registered metadata needed to recognize and configure one lint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LintRegistryEntry {
    /// The exact, case-sensitive diagnostic code.
    pub code: &'static str,
    /// The level used when no source overrides this lint.
    pub default_level: LintLevel,
    /// The language stage that introduces this lint.
    pub stage: u8,
}

/// The result of checking whether a name can be assigned a lint level.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LintNameLookup {
    /// A lint with its registry metadata.
    Lint(&'static LintRegistryEntry),
    /// A registered hard diagnostic and its fixed severity.
    HardDiagnostic(Severity),
    /// A name absent from the complete v0.1 registry.
    Unknown,
}

/// Every v0.1 lint in declaration order from spec/15 §15.17.2.
pub const LINT_REGISTRY: &[LintRegistryEntry] = &[
    LintRegistryEntry {
        code: "NON_ASCII_IDENTIFIER",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "UNNECESSARY_RAW_IDENT",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "UNUSED_IMPORT",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "REDUNDANT_IMPORT",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "SHADOWS_PRELUDE",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "IRREFUTABLE_CONDITION_PATTERN",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "REDUNDANT_BINDING_MODE",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "REDUNDANT_UPDATE_BASE",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "STATEMENT_HAS_NO_EFFECT",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "UNREACHABLE_CODE",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "UNREACHABLE_PATTERN",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "REDUNDANT_PROVENANCE",
        default_level: LintLevel::Warn,
        stage: 2,
    },
    LintRegistryEntry {
        code: "UNUSED_UNSAFE",
        default_level: LintLevel::Warn,
        stage: 2,
    },
    LintRegistryEntry {
        code: "MISSING_SAFETY_DOC",
        default_level: LintLevel::Warn,
        stage: 2,
    },
    LintRegistryEntry {
        code: "DUPLICATE_EFFECT",
        default_level: LintLevel::Warn,
        stage: 3,
    },
    LintRegistryEntry {
        code: "UNUSED_EFFECT_DECLARATION",
        default_level: LintLevel::Warn,
        stage: 3,
    },
    LintRegistryEntry {
        code: "PUBLIC_EFFECTS_INFERRED",
        default_level: LintLevel::Warn,
        stage: 3,
    },
    LintRegistryEntry {
        code: "CONTRACT_RUNTIME_CHECK",
        default_level: LintLevel::Info,
        stage: 3,
    },
    LintRegistryEntry {
        code: "DUPLICATE_PACKAGE_LINE",
        default_level: LintLevel::Warn,
        stage: 3,
    },
    LintRegistryEntry {
        code: "AMBIENT_IMPORT",
        default_level: LintLevel::Warn,
        stage: 4,
    },
    LintRegistryEntry {
        code: "UNUSED_VARIABLE",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "UNNECESSARY_VAR",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "UNUSED_ITEM",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "UNUSED_LABEL",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "NAMING_CONVENTION",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "UNUSED_ATTRIBUTE",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "DEPRECATED_REFERENCE",
        default_level: LintLevel::Warn,
        stage: 0,
    },
    LintRegistryEntry {
        code: "MISSING_DOCS",
        default_level: LintLevel::Allow,
        stage: 0,
    },
];

// Sorted snapshot of Appendix A; the registry test checks it against the docs.
const DIAGNOSTIC_CODE_NAMES: &str = include_str!("diagnostic_code_names.txt");
const HARD_WARNING_CODES: &[&str] = &[
    "MISPLACED_DOC_COMMENT",
    "WIT_ITEM_SHADOWED",
    "CACHE_CORRUPT",
    "CONFUSABLE_PACKAGE_NAME",
    "DUPLICATE_LIST_ITEM",
    "INVALID_LICENSE_EXPRESSION",
    "LOCK_NOT_CANONICAL",
    "SUBMODULE_IGNORED",
    "SYMLINK_IGNORED",
    "TAG_MISSING",
    "UNUSED_OVERRIDE",
    "VERSION_REQ_SPANS_LINES",
    "DEPRECATED_DIAGNOSTIC_CODE",
];
const HARD_INFO_CODES: &[&str] = &[
    "EXPORT_FN_NOT_APPLIED",
    "EXPORT_WORLD_NOT_APPLIED",
    "FOOTPRINT_EXCEEDS_POLICY",
    "FOOTPRINT_NOT_GRANTED",
    "ENTRY_POINT_NOT_APPLIED",
    "LARGE_STACK_FRAME",
    "NOT_PRESENT_IN_BUILD",
    "SYMBOL_INDEX_DISCARDED",
];

/// Classifies a name using the complete v0.1 diagnostic-code namespace.
///
/// This registry includes codes for stages that this workspace has not
/// implemented yet, so configuring a later-stage lint remains valid.
pub fn lookup_lint_name(name: &str) -> LintNameLookup {
    if let Some(lint) = LINT_REGISTRY.iter().find(|lint| lint.code == name) {
        return LintNameLookup::Lint(lint);
    }

    if !DIAGNOSTIC_CODE_NAMES
        .split_ascii_whitespace()
        .any(|registered| registered == name)
    {
        return LintNameLookup::Unknown;
    }

    let severity = if HARD_WARNING_CODES.contains(&name) {
        Severity::Warning
    } else if HARD_INFO_CODES.contains(&name) {
        Severity::Info
    } else {
        Severity::Error
    };
    LintNameLookup::HardDiagnostic(severity)
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum LevelSource {
    Default,
    Attribute,
    Manifest,
    Policy,
    Cli,
    DenyWarnings,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LevelInfo {
    pub value: LintLevel,
    pub source: LevelSource,
    pub location: Option<Location>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceRef {
    pub path: String,
    pub package: Option<String>,
    pub snapshot: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Location {
    pub file: SourceRef,
    pub span: Span,
}

impl Location {
    /// Constructs a location from a span over decoded source text.
    ///
    /// The stored span is converted to byte offsets in the exact source bytes,
    /// including any BOM and malformed UTF-8 bytes.
    pub fn from_decoded_source(
        file: SourceRef,
        decoded_span: Span,
        source: &sai_vfs::SourceFile,
    ) -> Option<Self> {
        if decoded_span.file != source.id() {
            return None;
        }
        Some(Self {
            file,
            span: Span::new(
                decoded_span.file,
                source.to_original_range(decoded_span.range)?,
            ),
        })
    }

    /// Constructs a location whose span already uses original-file byte offsets.
    pub fn new(path: impl Into<String>, span: Span) -> Self {
        Self {
            file: SourceRef {
                path: path.into(),
                package: None,
                snapshot: None,
            },
            span,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Label {
    pub location: Location,
    pub message: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ChildKind {
    Note,
    Help,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Child {
    pub kind: ChildKind,
    pub message: String,
    pub location: Option<Location>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Applicability {
    MachineApplicable,
    MaybeIncorrect,
    HasPlaceholders,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum NodeGroup {
    File,
    Item,
    Statement,
    Expression,
    Pattern,
    Type,
    Name,
    Error,
    Token,
}

impl NodeGroup {
    const fn as_str(self) -> &'static str {
        match self {
            Self::File => "file",
            Self::Item => "item",
            Self::Statement => "stmt",
            Self::Expression => "expr",
            Self::Pattern => "pat",
            Self::Type => "type",
            Self::Name => "name",
            Self::Error => "error",
            Self::Token => "tok",
        }
    }

    fn from_str(group: &str) -> Option<Self> {
        match group {
            "file" => Some(Self::File),
            "item" => Some(Self::Item),
            "stmt" => Some(Self::Statement),
            "expr" => Some(Self::Expression),
            "pat" => Some(Self::Pattern),
            "type" => Some(Self::Type),
            "name" => Some(Self::Name),
            "error" => Some(Self::Error),
            "tok" => Some(Self::Token),
            _ => None,
        }
    }
}

/// A CST node reference in the stable `group:HEX` representation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct NodeId {
    group: NodeGroup,
    index: u64,
}

impl NodeId {
    pub const fn new(group: NodeGroup, index: u64) -> Self {
        Self { group, index }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        let (group, hex) = raw.split_once(':')?;
        if !(4..=16).contains(&hex.len())
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'A'..=b'F').contains(&byte))
        {
            return None;
        }

        Some(Self {
            group: NodeGroup::from_str(group)?,
            index: u64::from_str_radix(hex, 16).ok()?,
        })
    }

    pub const fn group(self) -> NodeGroup {
        self.group
    }

    pub const fn index(self) -> u64 {
        self.index
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}:{:04X}", self.group.as_str(), self.index)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FixOperationName {
    ReplaceNode,
    InsertBefore,
    InsertAfter,
    RemoveNode,
    ReplaceField,
    InsertBorrow,
    SetEffects,
    ReorderClauses,
    RemoveToken,
    RenameSymbol,
    MoveSymbol,
    CreateFunction,
    DeleteFunction,
    ChangeType,
    AddParameter,
    RemoveParameter,
    ExtractFunction,
    InlineFunction,
    AddTest,
    ImplementTrait,
    MergeDeriveAttributes,
}

impl FixOperationName {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReplaceNode => "replace_node",
            Self::InsertBefore => "insert_before",
            Self::InsertAfter => "insert_after",
            Self::RemoveNode => "remove_node",
            Self::ReplaceField => "replace_field",
            Self::InsertBorrow => "insert_borrow",
            Self::SetEffects => "set_effects",
            Self::ReorderClauses => "reorder_clauses",
            Self::RemoveToken => "remove_token",
            Self::RenameSymbol => "rename_symbol",
            Self::MoveSymbol => "move_symbol",
            Self::CreateFunction => "create_function",
            Self::DeleteFunction => "delete_function",
            Self::ChangeType => "change_type",
            Self::AddParameter => "add_parameter",
            Self::RemoveParameter => "remove_parameter",
            Self::ExtractFunction => "extract_function",
            Self::InlineFunction => "inline_function",
            Self::AddTest => "add_test",
            Self::ImplementTrait => "implement_trait",
            Self::MergeDeriveAttributes => "merge_derive_attributes",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixOperationValidationError {
    MissingParameter,
    UnknownParameter,
    InvalidParameterType,
    InvalidParameterValue,
    CanonicalEffectsRequired,
}

#[derive(Clone, Copy)]
enum ArgumentKind {
    Text,
    UniqueTextList,
    Bool,
    Visibility,
    TextOrInteger,
    TextOrBool,
}

#[derive(Clone, Copy)]
struct ArgumentSpec {
    name: &'static str,
    kind: ArgumentKind,
    required: bool,
}

const fn required(name: &'static str, kind: ArgumentKind) -> ArgumentSpec {
    ArgumentSpec {
        name,
        kind,
        required: true,
    }
}

const fn optional(name: &'static str, kind: ArgumentKind) -> ArgumentSpec {
    ArgumentSpec {
        name,
        kind,
        required: false,
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FixOperation {
    name: FixOperationName,
    node: NodeId,
    args: ValueObject,
}

impl FixOperation {
    pub fn new(
        name: FixOperationName,
        node: NodeId,
        args: ValueObject,
    ) -> Result<Self, FixOperationValidationError> {
        Self::new_inner(name, node, args, None)
    }

    /// Constructs an operation using effect identities resolved by the semantic layer.
    ///
    /// This proof is required when `set_effects` contains interface or user effects:
    /// their canonical ordering and duplicate detection depend on resolved identity.
    pub fn new_with_canonical_effects(
        name: FixOperationName,
        node: NodeId,
        args: ValueObject,
        effects: &CanonicalEffectSet,
    ) -> Result<Self, FixOperationValidationError> {
        if name != FixOperationName::SetEffects {
            return Err(FixOperationValidationError::InvalidParameterValue);
        }
        Self::new_inner(name, node, args, Some(effects))
    }

    fn new_inner(
        name: FixOperationName,
        node: NodeId,
        args: ValueObject,
        canonical_effects: Option<&CanonicalEffectSet>,
    ) -> Result<Self, FixOperationValidationError> {
        let schema = argument_schema(name);
        for (name, value) in &args {
            let Some(spec) = schema.iter().find(|spec| spec.name == name) else {
                return Err(FixOperationValidationError::UnknownParameter);
            };
            if !spec.kind.accepts(value) {
                return Err(if spec.kind.has_duplicate_texts(value) {
                    FixOperationValidationError::InvalidParameterValue
                } else {
                    FixOperationValidationError::InvalidParameterType
                });
            }
        }
        for spec in &schema {
            if spec.required && !args.contains_key(spec.name) {
                return Err(FixOperationValidationError::MissingParameter);
            }
        }

        if name == FixOperationName::SetEffects {
            let Value::Array(values) = args
                .get("effects")
                .expect("required set_effects parameter was checked")
            else {
                unreachable!("effect argument kind was checked")
            };
            let names = values
                .iter()
                .map(|value| match value {
                    Value::String(name) => name.as_str(),
                    _ => unreachable!("effect list element type was checked"),
                })
                .collect::<Vec<_>>();
            if let Some(canonical_effects) = canonical_effects {
                if names
                    .iter()
                    .copied()
                    .ne(canonical_effects.spellings().map(String::as_str))
                {
                    return Err(FixOperationValidationError::InvalidParameterValue);
                }
            } else if names.iter().any(|name| builtin_effect_rank(name).is_none()) {
                return Err(FixOperationValidationError::CanonicalEffectsRequired);
            } else if names.windows(2).any(|pair| {
                let [left, right] = pair else {
                    return false;
                };
                builtin_effect_rank(left) >= builtin_effect_rank(right)
            }) {
                return Err(FixOperationValidationError::InvalidParameterValue);
            }
        }

        Ok(Self { name, node, args })
    }

    pub const fn name(&self) -> FixOperationName {
        self.name
    }

    pub const fn node(&self) -> NodeId {
        self.node
    }

    pub fn args(&self) -> &ValueObject {
        &self.args
    }
}

impl ArgumentKind {
    fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Text => matches!(value, Value::String(_)),
            Self::UniqueTextList => matches!(value, Value::Array(values)
                if values.iter().all(|value| matches!(value, Value::String(_)))
                    && !self.has_duplicate_texts(value)),
            Self::Bool => matches!(value, Value::Bool(_)),
            Self::Visibility => {
                matches!(value, Value::String(value) if matches!(value.as_str(), "package" | "pub" | "export"))
            }
            Self::TextOrInteger => {
                matches!(value, Value::Integer(_))
                    || matches!(value, Value::String(value) if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()))
            }
            Self::TextOrBool => {
                matches!(value, Value::Bool(_))
                    || matches!(value, Value::String(value) if matches!(value.as_str(), "true" | "false"))
            }
        }
    }

    fn has_duplicate_texts(self, value: &Value) -> bool {
        if !matches!(self, Self::UniqueTextList) {
            return false;
        }

        let Value::Array(values) = value else {
            return false;
        };
        let mut seen = BTreeSet::new();
        values
            .iter()
            .filter_map(|value| match value {
                Value::String(text) => Some(text),
                _ => None,
            })
            .any(|text| !seen.insert(text))
    }
}

fn builtin_effect_rank(effect: &str) -> Option<u8> {
    Some(match effect {
        "alloc" => 0,
        "fs.read" => 1,
        "fs.write" => 2,
        "net.connect" => 3,
        "net.http" => 4,
        "clock.read" => 5,
        "random.read" => 6,
        "stdio.read" => 7,
        "stdio.write" => 8,
        "env.read" => 9,
        "process.exit" => 10,
        "process.spawn" => 11,
        "thread.spawn" => 12,
        "host" => 13,
        "ffi" => 14,
        _ => return None,
    })
}

/// A resolved effect atom supplied by the semantic layer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResolvedEffect {
    Builtin(String),
    Interface { identity: String, spelling: String },
    User { identity: String, spelling: String },
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum EffectIdentity {
    Builtin(String),
    Interface(String),
    User(String),
}

/// Effect names in the canonical order required for generated tool output.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CanonicalEffectSet {
    effects: Vec<ResolvedEffect>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CanonicalEffectSetError {
    UnknownBuiltin,
    EmptyIdentityOrSpelling,
    DuplicateIdentity,
    DuplicateSpelling,
}

impl CanonicalEffectSet {
    pub fn from_resolved(
        effects: impl IntoIterator<Item = ResolvedEffect>,
    ) -> Result<Self, CanonicalEffectSetError> {
        let mut effects = effects.into_iter().collect::<Vec<_>>();
        let mut identities = BTreeSet::new();
        let mut spellings = BTreeSet::new();
        for effect in &effects {
            let (identity, spelling) = match effect {
                ResolvedEffect::Builtin(name) => {
                    if builtin_effect_rank(name).is_none() {
                        return Err(CanonicalEffectSetError::UnknownBuiltin);
                    }
                    (EffectIdentity::Builtin(name.clone()), name)
                }
                ResolvedEffect::Interface { identity, spelling } => {
                    if identity.is_empty() || spelling.is_empty() {
                        return Err(CanonicalEffectSetError::EmptyIdentityOrSpelling);
                    }
                    (EffectIdentity::Interface(identity.clone()), spelling)
                }
                ResolvedEffect::User { identity, spelling } => {
                    if identity.is_empty() || spelling.is_empty() {
                        return Err(CanonicalEffectSetError::EmptyIdentityOrSpelling);
                    }
                    (EffectIdentity::User(identity.clone()), spelling)
                }
            };
            if !identities.insert(identity) {
                return Err(CanonicalEffectSetError::DuplicateIdentity);
            }
            if !spellings.insert(spelling) {
                return Err(CanonicalEffectSetError::DuplicateSpelling);
            }
        }

        effects.sort_by(|left, right| match (left, right) {
            (ResolvedEffect::Builtin(left), ResolvedEffect::Builtin(right)) => {
                builtin_effect_rank(left).cmp(&builtin_effect_rank(right))
            }
            (ResolvedEffect::Builtin(_), _) => std::cmp::Ordering::Less,
            (_, ResolvedEffect::Builtin(_)) => std::cmp::Ordering::Greater,
            (
                ResolvedEffect::Interface { identity: left, .. },
                ResolvedEffect::Interface {
                    identity: right, ..
                },
            ) => left.as_bytes().cmp(right.as_bytes()),
            (ResolvedEffect::Interface { .. }, ResolvedEffect::User { .. }) => {
                std::cmp::Ordering::Less
            }
            (ResolvedEffect::User { .. }, ResolvedEffect::Interface { .. }) => {
                std::cmp::Ordering::Greater
            }
            (
                ResolvedEffect::User {
                    identity: left_identity,
                    spelling: left_spelling,
                },
                ResolvedEffect::User {
                    identity: right_identity,
                    spelling: right_spelling,
                },
            ) => left_spelling
                .as_bytes()
                .cmp(right_spelling.as_bytes())
                .then_with(|| left_identity.as_bytes().cmp(right_identity.as_bytes())),
        });

        Ok(Self { effects })
    }

    pub fn spellings(&self) -> impl Iterator<Item = &String> {
        self.effects.iter().map(|effect| match effect {
            ResolvedEffect::Builtin(spelling)
            | ResolvedEffect::Interface { spelling, .. }
            | ResolvedEffect::User { spelling, .. } => spelling,
        })
    }
}

fn argument_schema(name: FixOperationName) -> Vec<ArgumentSpec> {
    use ArgumentKind::{Bool, Text, TextOrBool, TextOrInteger, UniqueTextList, Visibility};
    use FixOperationName as Op;

    match name {
        Op::ReplaceNode | Op::InsertBefore | Op::InsertAfter => vec![required("text", Text)],
        Op::RemoveNode | Op::ReorderClauses | Op::RemoveToken | Op::DeleteFunction => vec![],
        Op::ReplaceField => vec![required("field", Text)],
        Op::InsertBorrow => vec![required("mutable", Bool)],
        Op::SetEffects => vec![required("effects", UniqueTextList)],
        Op::RenameSymbol => vec![required("name", Text)],
        Op::MoveSymbol => vec![required("module", Text)],
        Op::CreateFunction => vec![
            required("name", Text),
            optional("parameters", Text),
            optional("returns", Text),
            optional("effects", Text),
            optional("visibility", Visibility),
            optional("body", Text),
        ],
        Op::ChangeType => vec![required("type", Text), optional("parameter", Text)],
        Op::AddParameter => vec![
            required("name", Text),
            required("type", Text),
            optional("position", TextOrInteger),
            optional("argument", Text),
        ],
        Op::RemoveParameter => vec![required("name", Text)],
        Op::ExtractFunction => vec![
            required("name", Text),
            required("from", Text),
            optional("to", Text),
        ],
        Op::InlineFunction => vec![optional("call", Text), optional("keep", TextOrBool)],
        Op::AddTest => vec![required("name", Text), optional("body", Text)],
        Op::ImplementTrait => vec![
            required("trait", Text),
            optional("types", Text),
            optional("derive", TextOrBool),
        ],
        Op::MergeDeriveAttributes => vec![required("into", Text)],
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FixValidationError {
    NoEdits,
    OperationPlanningFailed,
    PlaceholderApplicabilityMismatch,
    PlaceholderOutOfBounds,
    EditsNotOrderedOrOverlapping,
    InvalidNewTextLineEnding,
    MissingPrimaryLocation,
    MissingWorkspaceSnapshot,
    SnapshotMismatch,
    InvalidSourceRange,
    OperationAndEditsDisagree,
}

/// A failure to derive edits for a structured operation on the current snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperationPlanError {
    UnsupportedOperation,
    TargetNotFound,
    NotApplicable,
    FormattingFailed,
}

/// Produces the text edits that exactly implement an operation on a snapshot.
///
/// Implementations belong to the syntax-aware operation planner. They must
/// apply the operation to the CST, format its enclosing format unit, and return
/// the minimal resulting edits. Callers cannot supply an independent edit list
/// to `Fix::new`.
pub trait FixOperationPlanner {
    /// Applies the operation to the source snapshot and returns each changed file's
    /// exact resulting bytes.
    fn apply_operation(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<BTreeMap<FileId, Vec<u8>>, OperationPlanError>;

    /// Produces the minimal text edits for the same operation and snapshot.
    fn plan_edits(
        &self,
        operation: &FixOperation,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<TextEdit>, OperationPlanError>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Fix {
    title: String,
    applicability: Applicability,
    operation: FixOperation,
    edits: Vec<TextEdit>,
    placeholders: Vec<Placeholder>,
    primary: Location,
    workspace_snapshot: WorkspaceSnapshotId,
}

impl Fix {
    pub fn new(
        title: impl Into<String>,
        applicability: Applicability,
        operation: FixOperation,
        placeholders: Vec<Placeholder>,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
        planner: &impl FixOperationPlanner,
    ) -> Result<Self, FixValidationError> {
        validate_location(snapshot, primary)?;
        let edits = planner
            .plan_edits(&operation, primary, snapshot)
            .map_err(|_| FixValidationError::OperationPlanningFailed)?;
        if edits.is_empty() {
            return Err(FixValidationError::NoEdits);
        }
        if (applicability == Applicability::HasPlaceholders) != !placeholders.is_empty() {
            return Err(FixValidationError::PlaceholderApplicabilityMismatch);
        }
        for placeholder in &placeholders {
            let Some(edit) = edits.get(placeholder.edit) else {
                return Err(FixValidationError::PlaceholderOutOfBounds);
            };
            if placeholder.start > placeholder.end
                || placeholder.end > edit.new_text.len()
                || !edit.new_text.is_char_boundary(placeholder.start)
                || !edit.new_text.is_char_boundary(placeholder.end)
            {
                return Err(FixValidationError::PlaceholderOutOfBounds);
            }
        }
        if edits.iter().any(|edit| edit.new_text.contains('\r')) {
            return Err(FixValidationError::InvalidNewTextLineEnding);
        }

        let ordered = edits.windows(2).all(|pair| {
            let [left, right] = pair else {
                return true;
            };
            if left.file.path > right.file.path {
                return false;
            }
            if left.file.path < right.file.path {
                return true;
            }
            let left_range = left.range.range;
            let right_range = right.range.range;
            left_range.start() <= right_range.start()
                && left_range.end() <= right_range.start()
                && !(left_range.is_empty() && right_range.is_empty())
        });
        if !ordered {
            return Err(FixValidationError::EditsNotOrderedOrOverlapping);
        }

        for edit in &edits {
            validate_location(
                snapshot,
                &Location {
                    file: edit.file.clone(),
                    span: edit.range,
                },
            )?;
        }
        let operation_result = planner
            .apply_operation(&operation, primary, snapshot)
            .map_err(|_| FixValidationError::OperationPlanningFailed)?;
        if apply_text_edits(snapshot, &edits)? != operation_result {
            return Err(FixValidationError::OperationAndEditsDisagree);
        }

        let fix = Self {
            title: title.into(),
            applicability,
            operation,
            edits,
            placeholders,
            primary: primary.clone(),
            workspace_snapshot: snapshot.id(),
        };
        fix.validate_primary(primary, snapshot.id())?;
        Ok(fix)
    }

    fn validate_primary(
        &self,
        primary: &Location,
        snapshot: WorkspaceSnapshotId,
    ) -> Result<(), FixValidationError> {
        if self.primary != *primary || self.workspace_snapshot != snapshot {
            return Err(FixValidationError::SnapshotMismatch);
        }

        let mut files_by_path = BTreeMap::from([(
            primary.file.path.clone(),
            (primary.file.clone(), primary.span.file),
        )]);
        let mut paths_by_file = BTreeMap::from([(primary.span.file, primary.file.path.clone())]);
        for edit in &self.edits {
            let path = edit.file.path.clone();
            let file_id = edit.range.file;
            if let Some((source_ref, known_id)) = files_by_path.get(&path) {
                if source_ref != &edit.file || *known_id != file_id {
                    return Err(FixValidationError::SnapshotMismatch);
                }
            } else {
                files_by_path.insert(path.clone(), (edit.file.clone(), file_id));
            }
            if let Some(known_path) = paths_by_file.get(&file_id) {
                if known_path != &path {
                    return Err(FixValidationError::SnapshotMismatch);
                }
            } else {
                paths_by_file.insert(file_id, path);
            }
            if edit.file.path == primary.file.path && edit.range.file != primary.span.file {
                return Err(FixValidationError::SnapshotMismatch);
            }
        }

        Ok(())
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    pub const fn applicability(&self) -> Applicability {
        self.applicability
    }

    pub fn operation(&self) -> &FixOperation {
        &self.operation
    }

    pub fn edits(&self) -> &[TextEdit] {
        &self.edits
    }

    pub fn placeholders(&self) -> &[Placeholder] {
        &self.placeholders
    }
}

fn apply_text_edits(
    snapshot: &WorkspaceSnapshot,
    edits: &[TextEdit],
) -> Result<BTreeMap<FileId, Vec<u8>>, FixValidationError> {
    let mut edits_by_file = BTreeMap::<FileId, Vec<&TextEdit>>::new();
    for edit in edits {
        edits_by_file.entry(edit.range.file).or_default().push(edit);
    }

    let mut result = BTreeMap::new();
    for (file_id, file_edits) in edits_by_file {
        let file = snapshot
            .file(file_id)
            .ok_or(FixValidationError::SnapshotMismatch)?;
        let original = file.source().raw_bytes();
        let crlf = first_line_uses_crlf(original);
        let mut bytes = original.to_vec();
        for edit in file_edits.into_iter().rev() {
            let start = edit.range.range.start().to_usize();
            let end = edit.range.range.end().to_usize();
            let replacement = if crlf {
                edit.new_text.replace('\n', "\r\n").into_bytes()
            } else {
                edit.new_text.as_bytes().to_vec()
            };
            bytes.splice(start..end, replacement);
        }
        result.insert(file_id, bytes);
    }
    Ok(result)
}

/// Returns whether edit text should use CR LF line endings for this source file.
///
/// Fix edits store normalized LF text; application follows the first line
/// terminator in the original file, matching [`apply_text_edits`].
pub fn first_line_uses_crlf(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .position(|byte| matches!(byte, b'\r' | b'\n'))
        .is_some_and(|index| bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n'))
}

fn validate_location(
    snapshot: &WorkspaceSnapshot,
    location: &Location,
) -> Result<(), FixValidationError> {
    let Some(file) = snapshot.file(location.span.file) else {
        return Err(FixValidationError::SnapshotMismatch);
    };
    if !file.matches_reference(
        &location.file.path,
        location.file.package.as_deref(),
        location.file.snapshot,
    ) {
        return Err(FixValidationError::SnapshotMismatch);
    }

    validate_span(snapshot, location.span)
}

fn validate_span(snapshot: &WorkspaceSnapshot, span: Span) -> Result<(), FixValidationError> {
    let Some(file) = snapshot.file(span.file) else {
        return Err(FixValidationError::SnapshotMismatch);
    };
    let source = file.source();
    let raw = source.raw_bytes();
    let start = span.range.start();
    let end = span.range.end();
    if end.to_usize() > raw.len()
        || !source.is_original_char_boundary(start)
        || !source.is_original_char_boundary(end)
        || splits_crlf(raw, start.to_usize())
        || splits_crlf(raw, end.to_usize())
    {
        return Err(FixValidationError::InvalidSourceRange);
    }
    Ok(())
}

fn splits_crlf(bytes: &[u8], offset: usize) -> bool {
    offset > 0 && offset < bytes.len() && bytes[offset - 1] == b'\r' && bytes[offset] == b'\n'
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TextEdit {
    pub file: SourceRef,
    pub range: Span,
    pub new_text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Placeholder {
    pub edit: usize,
    pub start: usize,
    pub end: usize,
    pub label: String,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Relation {
    Calls,
    Instantiates,
    Awaits,
    Covers,
    Imports,
    Declares,
    Requires,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChainStep {
    pub name: String,
    pub symbol: Option<String>,
    pub location: Option<Location>,
    pub relation: Relation,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum LoanKind {
    Shared,
    Mutable,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum AccessKind {
    Read,
    Write,
    Move,
    Borrow,
    BorrowMut,
    StorageDead,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum MoveKind {
    Assign,
    Argument,
    Return,
    Capture,
    Pattern,
}

/// Typed related facts; renderers and tools need not parse explanatory prose.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Related {
    EffectChain {
        effect: String,
        steps: Vec<ChainStep>,
    },
    CapabilityChain {
        capability: String,
        effect: String,
        steps: Vec<ChainStep>,
        policy: Option<Location>,
    },
    BorrowConflict {
        place: String,
        loan: Location,
        loan_kind: LoanKind,
        access: Location,
        access_kind: AccessKind,
        later_use: Option<Location>,
    },
    UseAfterMove {
        place: String,
        moved_at: Location,
        move_kind: MoveKind,
        used_at: Location,
        in_loop: bool,
    },
    RequirementChain {
        steps: Vec<ChainStep>,
    },
    DeclaredHere {
        location: Location,
        what: String,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(u128),
    String(String),
    Array(Vec<Value>),
    Object(ValueObject),
}

/// Ordered JSON object data, kept deterministic without a serialization dependency.
pub type ValueObject = std::collections::BTreeMap<String, Value>;

/// A fact that may explain or be poisoned by another diagnostic.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum PoisonFact {
    SyntaxNode(Span),
    ErrorType(String),
    UnresolvedName { item: String, spelling: String },
    Signature(String),
    Body(String),
    Package(String),
}

/// The set of poison facts on which a diagnostic depends.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CauseKey(BTreeSet<PoisonFact>);

impl CauseKey {
    pub fn new(facts: impl IntoIterator<Item = PoisonFact>) -> Self {
        Self(facts.into_iter().collect())
    }

    pub fn facts(&self) -> &BTreeSet<PoisonFact> {
        &self.0
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Diagnostic {
    pub code: Code,
    pub severity: Severity,
    level: Option<LevelInfo>,
    pub message: String,
    primary: Option<Location>,
    pub primary_label: String,
    labels: Vec<Label>,
    children: Vec<Child>,
    fixes: Vec<Fix>,
    workspace_snapshot: Option<WorkspaceSnapshot>,
    related: Vec<Related>,
    pub data: ValueObject,
    pub node: Option<String>,
    pub symbol: Option<String>,
    pub package: Option<String>,
    cause: CauseKey,
}

/// Maximum nesting depth accepted when converting diagnostic data into a
/// snapshot-independent query template.
pub const DIAGNOSTIC_TEMPLATE_MAX_VALUE_DEPTH: usize = 128;

impl Diagnostic {
    pub fn new(
        code: Code,
        severity: Severity,
        message: impl Into<String>,
        primary: Option<Location>,
    ) -> Self {
        Self {
            code,
            severity,
            level: None,
            message: message.into(),
            primary,
            primary_label: String::new(),
            labels: Vec::new(),
            children: Vec::new(),
            fixes: Vec::new(),
            workspace_snapshot: None,
            related: Vec::new(),
            data: ValueObject::new(),
            node: None,
            symbol: None,
            package: None,
            cause: CauseKey::default(),
        }
    }

    pub fn with_cause(mut self, cause: CauseKey) -> Result<Self, FixValidationError> {
        if let Some(snapshot) = &self.workspace_snapshot {
            validate_cause(snapshot, &cause)?;
        }
        self.cause = cause;
        Ok(self)
    }

    pub fn primary(&self) -> Option<&Location> {
        self.primary.as_ref()
    }

    pub fn fixes(&self) -> &[Fix] {
        &self.fixes
    }

    pub fn level(&self) -> Option<&LevelInfo> {
        self.level.as_ref()
    }

    pub fn labels(&self) -> &[Label] {
        &self.labels
    }

    pub fn children(&self) -> &[Child] {
        &self.children
    }

    pub fn related(&self) -> &[Related] {
        &self.related
    }

    pub fn set_level(&mut self, level: Option<LevelInfo>) -> Result<(), FixValidationError> {
        if let (Some(snapshot), Some(level)) = (&self.workspace_snapshot, &level) {
            if let Some(location) = &level.location {
                validate_location(snapshot, location)?;
            }
        }
        self.level = level;
        Ok(())
    }

    pub fn push_label(&mut self, label: Label) -> Result<(), FixValidationError> {
        if let Some(snapshot) = &self.workspace_snapshot {
            validate_location(snapshot, &label.location)?;
        }
        self.labels.push(label);
        Ok(())
    }

    pub fn push_child(&mut self, child: Child) -> Result<(), FixValidationError> {
        if let (Some(snapshot), Some(location)) = (&self.workspace_snapshot, &child.location) {
            validate_location(snapshot, location)?;
        }
        self.children.push(child);
        Ok(())
    }

    pub fn push_related(&mut self, related: Related) -> Result<(), FixValidationError> {
        if let Some(snapshot) = &self.workspace_snapshot {
            validate_related(snapshot, &related)?;
        }
        self.related.push(related);
        Ok(())
    }

    pub fn with_workspace_snapshot(
        mut self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Self, FixValidationError> {
        if self
            .workspace_snapshot
            .as_ref()
            .is_some_and(|current| current.id() != snapshot.id())
        {
            return Err(FixValidationError::SnapshotMismatch);
        }
        if let Some(primary) = &self.primary {
            validate_location(snapshot, primary)?;
        }
        if let Some(level) = &self.level {
            if let Some(location) = &level.location {
                validate_location(snapshot, location)?;
            }
        }
        for label in &self.labels {
            validate_location(snapshot, &label.location)?;
        }
        for child in &self.children {
            if let Some(location) = &child.location {
                validate_location(snapshot, location)?;
            }
        }
        for related in &self.related {
            validate_related(snapshot, related)?;
        }
        validate_cause(snapshot, &self.cause)?;
        self.workspace_snapshot = Some(snapshot.clone());
        Ok(self)
    }

    pub fn with_fix(mut self, fix: Fix) -> Result<Self, FixValidationError> {
        self.push_fix(fix)?;
        Ok(self)
    }

    pub fn push_fix(&mut self, fix: Fix) -> Result<(), FixValidationError> {
        let primary = self
            .primary
            .as_ref()
            .ok_or(FixValidationError::MissingPrimaryLocation)?;
        let snapshot = self
            .workspace_snapshot
            .as_ref()
            .ok_or(FixValidationError::MissingWorkspaceSnapshot)?
            .id();
        fix.validate_primary(primary, snapshot)?;
        self.fixes.push(fix);
        Ok(())
    }

    pub fn cause(&self) -> &CauseKey {
        &self.cause
    }

    /// Converts a source diagnostic into a snapshot-independent query value.
    ///
    /// Locations are retained as byte ranges, while their `FileId` and
    /// `SourceRef` are removed. The template can later be rebound to the
    /// requesting snapshot with [`DiagnosticTemplate::bind`]. Diagnostics
    /// carrying resolved levels, related locations, fixes, a workspace
    /// snapshot, or poison facts are rejected because those values need
    /// additional query or snapshot context. Diagnostic data deeper than
    /// [`DIAGNOSTIC_TEMPLATE_MAX_VALUE_DEPTH`] is also rejected.
    pub fn to_template(&self) -> Result<DiagnosticTemplate, DiagnosticTemplateError> {
        if self.level.is_some()
            || !self.related.is_empty()
            || !self.fixes.is_empty()
            || self.workspace_snapshot.is_some()
            || !self.cause.facts().is_empty()
        {
            return Err(DiagnosticTemplateError::ContextualState);
        }
        if !value_object_within_depth(&self.data, DIAGNOSTIC_TEMPLATE_MAX_VALUE_DEPTH) {
            return Err(DiagnosticTemplateError::ValueTooDeep);
        }

        let mut expected_source = None;
        let primary_range = template_range(self.primary.as_ref(), &mut expected_source)?;
        let mut labels = Vec::with_capacity(self.labels.len());
        for label in &self.labels {
            let range = template_range(Some(&label.location), &mut expected_source)?
                .ok_or(DiagnosticTemplateError::MissingLocation)?;
            labels.push(TemplateLabel {
                range,
                message: label.message.clone(),
            });
        }
        let mut children = Vec::with_capacity(self.children.len());
        for child in &self.children {
            children.push(TemplateChild {
                kind: child.kind,
                message: child.message.clone(),
                range: template_range(child.location.as_ref(), &mut expected_source)?,
            });
        }

        Ok(DiagnosticTemplate {
            code: self.code,
            severity: self.severity,
            message: self.message.clone(),
            primary_range,
            primary_label: self.primary_label.clone(),
            labels,
            children,
            data: self.data.clone(),
            node: self.node.clone(),
            symbol: self.symbol.clone(),
            package: self.package.clone(),
        })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DiagnosticTemplateError {
    ContextualState,
    MixedSource,
    MissingLocation,
    ValueTooDeep,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TemplateLabel {
    range: TextRange,
    message: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct TemplateChild {
    kind: ChildKind,
    message: String,
    range: Option<TextRange>,
}

/// A source diagnostic value without snapshot-specific file identity.
///
/// All stored locations are byte ranges in one source file. Binding supplies
/// the current file ID and source reference without changing the query value.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiagnosticTemplate {
    code: Code,
    severity: Severity,
    message: String,
    primary_range: Option<TextRange>,
    primary_label: String,
    labels: Vec<TemplateLabel>,
    children: Vec<TemplateChild>,
    data: ValueObject,
    node: Option<String>,
    symbol: Option<String>,
    package: Option<String>,
}

impl DiagnosticTemplate {
    /// Recreates this diagnostic with the requesting snapshot's file identity.
    pub fn bind(&self, file_id: FileId, source_ref: SourceRef) -> Diagnostic {
        let location = |range: TextRange| Location {
            file: source_ref.clone(),
            span: Span::new(file_id, range),
        };
        let mut diagnostic = Diagnostic::new(
            self.code,
            self.severity,
            self.message.clone(),
            self.primary_range.map(location),
        );
        diagnostic.primary_label.clone_from(&self.primary_label);
        diagnostic.labels = self
            .labels
            .iter()
            .map(|label| Label {
                location: location(label.range),
                message: label.message.clone(),
            })
            .collect();
        diagnostic.children = self
            .children
            .iter()
            .map(|child| Child {
                kind: child.kind,
                message: child.message.clone(),
                location: child.range.map(location),
            })
            .collect();
        diagnostic.data.clone_from(&self.data);
        diagnostic.node.clone_from(&self.node);
        diagnostic.symbol.clone_from(&self.symbol);
        diagnostic.package.clone_from(&self.package);
        diagnostic
    }
}

impl StableHash for DiagnosticTemplate {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD0);
        self.code.as_str().stable_hash(hasher);
        self.severity.stable_hash(hasher);
        self.message.stable_hash(hasher);
        stable_hash_range(self.primary_range, hasher);
        self.primary_label.stable_hash(hasher);
        hasher.write_len(u64::try_from(self.labels.len()).unwrap_or(u64::MAX));
        for label in &self.labels {
            hasher.write_tag(0xD1);
            stable_hash_range(Some(label.range), hasher);
            label.message.stable_hash(hasher);
        }
        hasher.write_len(u64::try_from(self.children.len()).unwrap_or(u64::MAX));
        for child in &self.children {
            hasher.write_tag(0xD2);
            hasher.write_tag(match child.kind {
                ChildKind::Note => 0,
                ChildKind::Help => 1,
            });
            child.message.stable_hash(hasher);
            stable_hash_range(child.range, hasher);
        }
        stable_hash_value_object(&self.data, hasher);
        self.node.stable_hash(hasher);
        self.symbol.stable_hash(hasher);
        self.package.stable_hash(hasher);
    }
}

fn template_range(
    location: Option<&Location>,
    expected_source: &mut Option<(SourceRef, FileId)>,
) -> Result<Option<TextRange>, DiagnosticTemplateError> {
    let Some(location) = location else {
        return Ok(None);
    };
    let source = (location.file.clone(), location.span.file);
    if let Some(expected) = expected_source {
        if *expected != source {
            return Err(DiagnosticTemplateError::MixedSource);
        }
    } else {
        *expected_source = Some(source);
    }
    Ok(Some(location.span.range))
}

fn stable_hash_range(range: Option<TextRange>, hasher: &mut StableHasher) {
    hasher.write_tag(0xD3);
    match range {
        None => hasher.write_tag(0),
        Some(range) => {
            hasher.write_tag(1);
            range.start().raw().stable_hash(hasher);
            range.end().raw().stable_hash(hasher);
        }
    }
}

fn stable_hash_value_object(value: &ValueObject, hasher: &mut StableHasher) {
    enum Entry<'a> {
        Value(&'a Value),
        Key(&'a str),
    }

    hasher.write_tag(0xD4);
    hasher.write_len(u64::try_from(value.len()).unwrap_or(u64::MAX));
    let mut stack = Vec::with_capacity(value.len().saturating_mul(2));
    for (key, value) in value.iter().rev() {
        stack.push(Entry::Value(value));
        stack.push(Entry::Key(key));
    }
    while let Some(entry) = stack.pop() {
        match entry {
            Entry::Key(key) => key.stable_hash(hasher),
            Entry::Value(value) => match value {
                Value::Null => hasher.write_tag(0),
                Value::Bool(value) => {
                    hasher.write_tag(1);
                    value.stable_hash(hasher);
                }
                Value::Integer(value) => {
                    hasher.write_tag(2);
                    value.stable_hash(hasher);
                }
                Value::String(value) => {
                    hasher.write_tag(3);
                    value.stable_hash(hasher);
                }
                Value::Array(values) => {
                    hasher.write_tag(4);
                    hasher.write_len(u64::try_from(values.len()).unwrap_or(u64::MAX));
                    stack.extend(values.iter().rev().map(Entry::Value));
                }
                Value::Object(values) => {
                    hasher.write_tag(5);
                    hasher.write_len(u64::try_from(values.len()).unwrap_or(u64::MAX));
                    for (key, value) in values.iter().rev() {
                        stack.push(Entry::Value(value));
                        stack.push(Entry::Key(key));
                    }
                }
            },
        }
    }
}

fn value_object_within_depth(value: &ValueObject, max_depth: usize) -> bool {
    let mut stack = value
        .values()
        .map(|value| (value, 1_usize))
        .collect::<Vec<_>>();
    while let Some((value, depth)) = stack.pop() {
        if depth > max_depth {
            return false;
        }
        match value {
            Value::Array(values) => {
                stack.extend(values.iter().map(|value| (value, depth.saturating_add(1))));
            }
            Value::Object(values) => {
                stack.extend(
                    values
                        .values()
                        .map(|value| (value, depth.saturating_add(1))),
                );
            }
            Value::Null | Value::Bool(_) | Value::Integer(_) | Value::String(_) => {}
        }
    }
    true
}

impl StableHash for Severity {
    fn stable_hash(&self, hasher: &mut StableHasher) {
        hasher.write_tag(0xD5);
        hasher.write_tag(match self {
            Self::Error => 0,
            Self::Warning => 1,
            Self::Info => 2,
        });
    }
}

/// Converts a tracked source-text status into its source-level diagnostic.
///
/// The caller supplies the canonical source reference for this file. A
/// `Missing` status represents a listed file for which the driver supplied no
/// contents, so it is reported as a read failure with the reason `"not found"`.
/// Error locations point at byte zero in the empty source retained by the VFS.
pub fn source_text_status_diagnostic(
    source_text: &sai_vfs::SourceText,
    source_ref: SourceRef,
) -> Option<Diagnostic> {
    let (code, message, data) = match source_text.status() {
        sai_vfs::SourceTextStatus::Ready => return None,
        sai_vfs::SourceTextStatus::Missing => {
            let reason = "not found";
            let mut data = ValueObject::new();
            data.insert("path".to_owned(), Value::String(source_ref.path.clone()));
            data.insert("reason".to_owned(), Value::String(reason.to_owned()));
            (
                Code::SourceReadFailed,
                format!("could not read source file: {reason}"),
                data,
            )
        }
        sai_vfs::SourceTextStatus::ReadError { io_error_kind } => {
            let reason = normalize_io_error_reason(io_error_kind);
            let mut data = ValueObject::new();
            data.insert("path".to_owned(), Value::String(source_ref.path.clone()));
            data.insert("reason".to_owned(), Value::String(reason.clone()));
            (
                Code::SourceReadFailed,
                format!("could not read source file: {reason}"),
                data,
            )
        }
        sai_vfs::SourceTextStatus::TooLarge { byte_len } => {
            let mut data = ValueObject::new();
            data.insert(
                "limit".to_owned(),
                Value::String("size of a source file".to_owned()),
            );
            data.insert("value".to_owned(), Value::Integer(*byte_len as u128));
            (
                Code::ImplementationLimitExceeded,
                format!(
                    "source file is {byte_len} bytes, exceeding the {} MiB limit",
                    sai_vfs::MAX_SOURCE_BYTES / (1024 * 1024)
                ),
                data,
            )
        }
    };

    let package = source_ref.package.clone();
    let primary = Location {
        file: source_ref,
        span: Span::new(
            source_text.source().id(),
            TextRange::empty(TextSize::new(0)),
        ),
    };
    let mut diagnostic = Diagnostic::new(code, Severity::Error, message, Some(primary));
    diagnostic.data = data;
    diagnostic.package = package;
    Some(diagnostic)
}

const MAX_IO_ERROR_REASON_CHARS: usize = 64;
const MAX_IO_ERROR_REASON_INPUT_CHARS: usize = 256;

fn normalize_io_error_reason(reason: &str) -> String {
    let chars = reason
        .chars()
        .take_while(|character| *character != '\r' && *character != '\n')
        .take(MAX_IO_ERROR_REASON_INPUT_CHARS)
        .collect::<Vec<_>>();
    let mut normalized = String::with_capacity(chars.len());

    for (index, character) in chars.iter().copied().enumerate() {
        if character.is_whitespace() || character.is_control() {
            if !normalized.is_empty() && !normalized.ends_with(' ') {
                normalized.push(' ');
            }
            continue;
        }

        let previous = index.checked_sub(1).and_then(|index| chars.get(index));
        let next = chars.get(index + 1);
        let camel_case_boundary = character.is_uppercase()
            && (previous.is_some_and(|previous| previous.is_lowercase() || previous.is_numeric())
                || (previous.is_some_and(|previous| previous.is_uppercase())
                    && next.is_some_and(|next| next.is_lowercase())));
        if camel_case_boundary && !normalized.is_empty() && !normalized.ends_with(' ') {
            normalized.push(' ');
        }
        normalized.extend(character.to_lowercase());
    }

    let normalized = normalized.trim().trim_end_matches('.');
    if normalized.is_empty() {
        return "unknown error".to_owned();
    }

    if normalized.chars().nth(MAX_IO_ERROR_REASON_CHARS).is_some() {
        let mut truncated = normalized
            .chars()
            .take(MAX_IO_ERROR_REASON_CHARS - 1)
            .collect::<String>();
        truncated.push('…');
        return truncated;
    }

    normalized.to_owned()
}

fn validate_cause(
    snapshot: &WorkspaceSnapshot,
    cause: &CauseKey,
) -> Result<(), FixValidationError> {
    for fact in cause.facts() {
        if let PoisonFact::SyntaxNode(span) = fact {
            validate_span(snapshot, *span)?;
        }
    }
    Ok(())
}

fn validate_related(
    snapshot: &WorkspaceSnapshot,
    related: &Related,
) -> Result<(), FixValidationError> {
    fn validate_steps(
        snapshot: &WorkspaceSnapshot,
        steps: &[ChainStep],
    ) -> Result<(), FixValidationError> {
        for step in steps {
            if let Some(location) = &step.location {
                validate_location(snapshot, location)?;
            }
        }
        Ok(())
    }

    match related {
        Related::EffectChain { steps, .. } | Related::RequirementChain { steps } => {
            validate_steps(snapshot, steps)
        }
        Related::CapabilityChain { steps, policy, .. } => {
            validate_steps(snapshot, steps)?;
            if let Some(location) = policy {
                validate_location(snapshot, location)?;
            }
            Ok(())
        }
        Related::BorrowConflict {
            loan,
            access,
            later_use,
            ..
        } => {
            validate_location(snapshot, loan)?;
            validate_location(snapshot, access)?;
            if let Some(location) = later_use {
                validate_location(snapshot, location)?;
            }
            Ok(())
        }
        Related::UseAfterMove {
            moved_at, used_at, ..
        } => {
            validate_location(snapshot, moved_at)?;
            validate_location(snapshot, used_at)
        }
        Related::DeclaredHere { location, .. } => validate_location(snapshot, location),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sai_query::{Durability, QueryDatabase};
    use sai_stable_hash::{StableHasher, fingerprint};
    use sai_vfs::{FileKey, SnapshotFile, SourceFile, SourceText, SourceTextStatus, VfsInputs};
    use std::sync::Arc;

    fn source_text_with_status(file_id: u32, status: SourceTextStatus) -> SourceText {
        let vfs = VfsInputs::new().expect("VFS query handles are valid");
        let mut database = QueryDatabase::new();
        let key = FileKey::new("sai:core", "dep:sai:core@1.0.0/src/lib.sai")
            .expect("built-in source key is valid");
        match status {
            SourceTextStatus::Ready => {
                vfs.set_file_contents(
                    &mut database,
                    key.clone(),
                    Arc::<[u8]>::from([]),
                    Durability::Low,
                )
                .expect("source input can be set");
            }
            SourceTextStatus::Missing => {}
            SourceTextStatus::ReadError { io_error_kind } => {
                vfs.set_file_read_error(&mut database, key.clone(), io_error_kind, Durability::Low)
                    .expect("source read error can be set");
            }
            SourceTextStatus::TooLarge { byte_len } => {
                vfs.set_file_contents(
                    &mut database,
                    key.clone(),
                    Arc::<[u8]>::from(vec![b'x'; byte_len]),
                    Durability::Low,
                )
                .expect("oversized source input can be set");
            }
        }
        vfs.source_text(&mut database, key, FileId::new(file_id))
            .expect("source text query succeeds")
    }

    fn replace_node_operation() -> FixOperation {
        FixOperation::new(
            FixOperationName::ReplaceNode,
            NodeId::new(NodeGroup::Expression, 0x82A1),
            ValueObject::from([("text".to_owned(), Value::String("replacement".to_owned()))]),
        )
        .expect("valid replace_node operation")
    }

    fn test_location(snapshot: Option<u64>) -> Location {
        Location {
            file: SourceRef {
                path: "src/lib.sai".to_owned(),
                package: None,
                snapshot,
            },
            span: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(0), TextSize::new(1)).expect("ordered range"),
            ),
        }
    }

    #[test]
    fn ready_source_text_has_no_status_diagnostic() {
        let source = source_text_with_status(7, SourceTextStatus::Ready);
        let source_ref = SourceRef {
            path: "src/lib.sai".to_owned(),
            package: Some("sai:demo@0.1.0".to_owned()),
            snapshot: Some(4),
        };

        assert!(source_text_status_diagnostic(&source, source_ref).is_none());
    }

    #[test]
    fn missing_source_text_becomes_a_not_found_read_diagnostic() {
        let source = source_text_with_status(7, SourceTextStatus::Missing);
        let source_ref = SourceRef {
            path: "dep:sai:core@1.0.0/src/lib.sai".to_owned(),
            package: Some("sai:core@1.0.0".to_owned()),
            snapshot: None,
        };

        let diagnostic = source_text_status_diagnostic(&source, source_ref.clone())
            .expect("missing source produces a diagnostic");

        assert_eq!(diagnostic.code, Code::SourceReadFailed);
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.message, "could not read source file: not found");
        assert_eq!(
            diagnostic.data,
            ValueObject::from([
                ("path".to_owned(), Value::String(source_ref.path.clone())),
                ("reason".to_owned(), Value::String("not found".to_owned())),
            ])
        );
        assert_eq!(diagnostic.package.as_deref(), source_ref.package.as_deref());
        let primary = diagnostic
            .primary()
            .expect("read error has a source location");
        assert_eq!(primary.file, source_ref);
        assert_eq!(primary.span.file, FileId::new(7));
        assert_eq!(primary.span.range, TextRange::empty(TextSize::new(0)));
    }

    #[test]
    fn read_error_preserves_the_io_reason_and_source_reference() {
        let source = source_text_with_status(
            11,
            SourceTextStatus::ReadError {
                io_error_kind: "permission denied".to_owned(),
            },
        );
        let source_ref = SourceRef {
            path: "src/private.sai".to_owned(),
            package: None,
            snapshot: Some(9),
        };

        let diagnostic = source_text_status_diagnostic(&source, source_ref.clone())
            .expect("read error produces a diagnostic");

        assert_eq!(diagnostic.code, Code::SourceReadFailed);
        assert_eq!(
            diagnostic.message,
            "could not read source file: permission denied"
        );
        assert_eq!(
            diagnostic.data,
            ValueObject::from([
                ("path".to_owned(), Value::String(source_ref.path.clone())),
                (
                    "reason".to_owned(),
                    Value::String("permission denied".to_owned())
                ),
            ])
        );
        assert_eq!(
            diagnostic
                .primary()
                .expect("read error has a location")
                .file,
            source_ref
        );
    }

    #[test]
    fn read_error_reason_is_lowercase_and_single_line() {
        let source = source_text_with_status(
            12,
            SourceTextStatus::ReadError {
                io_error_kind: "PermissionDenied.\nadditional details".to_owned(),
            },
        );
        let source_ref = SourceRef {
            path: "src/private.sai".to_owned(),
            package: None,
            snapshot: None,
        };

        let diagnostic = source_text_status_diagnostic(&source, source_ref)
            .expect("read error produces a diagnostic");

        assert_eq!(
            diagnostic.message,
            "could not read source file: permission denied"
        );
        assert!(!diagnostic.message.ends_with('.'));
        assert_eq!(
            diagnostic.data.get("reason"),
            Some(&Value::String("permission denied".to_owned()))
        );
    }

    #[test]
    fn read_error_reason_is_bounded_with_a_truncation_marker() {
        let source = source_text_with_status(
            14,
            SourceTextStatus::ReadError {
                io_error_kind: format!("PermissionDenied{}", "x".repeat(120)),
            },
        );

        let diagnostic = source_text_status_diagnostic(
            &source,
            SourceRef {
                path: "src/private.sai".to_owned(),
                package: None,
                snapshot: None,
            },
        )
        .expect("read error produces a diagnostic");
        let reason = match diagnostic.data.get("reason") {
            Some(Value::String(reason)) => reason,
            other => panic!("reason is a string, got {other:?}"),
        };

        assert_eq!(reason.chars().count(), MAX_IO_ERROR_REASON_CHARS);
        assert!(reason.ends_with('…'));
        assert_eq!(
            diagnostic.message,
            format!("could not read source file: {reason}")
        );
        assert!(!diagnostic.message.ends_with('.'));
        assert!(diagnostic.message.chars().count() <= 100);
    }

    #[test]
    fn oversized_source_text_reports_the_limit_and_exact_byte_length() {
        let byte_len = sai_vfs::MAX_SOURCE_BYTES + 1;
        let source = source_text_with_status(13, SourceTextStatus::TooLarge { byte_len });
        let source_ref = SourceRef {
            path: "src/large.sai".to_owned(),
            package: Some("sai:demo@0.1.0".to_owned()),
            snapshot: None,
        };

        let diagnostic = source_text_status_diagnostic(&source, source_ref.clone())
            .expect("oversized source produces a diagnostic");

        assert_eq!(diagnostic.code, Code::ImplementationLimitExceeded);
        assert_eq!(
            diagnostic.message,
            format!(
                "source file is {byte_len} bytes, exceeding the {} MiB limit",
                sai_vfs::MAX_SOURCE_BYTES / (1024 * 1024)
            )
        );
        assert_eq!(
            diagnostic.data,
            ValueObject::from([
                (
                    "limit".to_owned(),
                    Value::String("size of a source file".to_owned())
                ),
                ("value".to_owned(), Value::Integer(byte_len as u128)),
            ])
        );
        assert_eq!(
            diagnostic
                .primary()
                .expect("size limit has a source location")
                .file,
            source_ref
        );
    }

    #[test]
    fn locations_convert_decoded_spans_to_original_file_bytes() {
        let source = Arc::new(
            SourceFile::decode(FileId::new(1), b"\xEF\xBB\xBF\xFFa").expect("source file"),
        );
        let decoded = Span::new(
            FileId::new(1),
            TextRange::new(TextSize::new(3), TextSize::new(4)).expect("ordered range"),
        );
        let location = Location::from_decoded_source(
            SourceRef {
                path: "src/lib.sai".to_owned(),
                package: None,
                snapshot: None,
            },
            decoded,
            &source,
        )
        .expect("decoded span maps to raw bytes");

        assert_eq!(
            location.span.range,
            TextRange::new(TextSize::new(4), TextSize::new(5)).expect("ordered range")
        );

        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source, "src/lib.sai", None, None)])
                .expect("single-file snapshot");
        let edit = TextEdit {
            file: location.file.clone(),
            range: location.span,
            new_text: "replacement".to_owned(),
        };
        assert!(
            test_fix(
                "replace first visible character",
                Applicability::MachineApplicable,
                replace_node_operation(),
                vec![edit],
                vec![],
                &location,
                &snapshot,
            )
            .is_ok()
        );
    }

    #[test]
    fn locations_reject_decoded_spans_inside_crlf_pairs() {
        let source = SourceFile::new(FileId::new(1), "a\r\n", false).expect("source file");
        let decoded = Span::new(FileId::new(1), TextRange::empty(TextSize::new(2)));

        assert!(
            Location::from_decoded_source(
                SourceRef {
                    path: "src/lib.sai".to_owned(),
                    package: None,
                    snapshot: None,
                },
                decoded,
                &source,
            )
            .is_none()
        );
    }

    fn snapshot_for(primary: &Location, edits: &[TextEdit]) -> WorkspaceSnapshot {
        let mut files = BTreeMap::<FileId, (SourceRef, usize)>::new();
        let mut include = |file: &SourceRef, span: Span| {
            let entry = files.entry(span.file).or_insert_with(|| (file.clone(), 0));
            entry.1 = entry.1.max(span.range.end().to_usize());
        };
        include(&primary.file, primary.span);
        for edit in edits {
            include(&edit.file, edit.range);
        }
        drop(include);

        WorkspaceSnapshot::new(files.into_iter().map(|(id, (reference, length))| {
            let source =
                Arc::new(SourceFile::new(id, "x".repeat(length), false).expect("small source"));
            SnapshotFile::new(
                source,
                reference.path,
                reference.package,
                reference.snapshot,
            )
        }))
        .expect("test files form a workspace snapshot")
    }

    struct StaticFixPlanner {
        edits: Vec<TextEdit>,
        operation_result: Option<BTreeMap<FileId, Vec<u8>>>,
    }

    impl FixOperationPlanner for StaticFixPlanner {
        fn apply_operation(
            &self,
            _operation: &FixOperation,
            _primary: &Location,
            snapshot: &WorkspaceSnapshot,
        ) -> Result<BTreeMap<FileId, Vec<u8>>, OperationPlanError> {
            self.operation_result.clone().map_or_else(
                || {
                    apply_text_edits(snapshot, &self.edits)
                        .map_err(|_| OperationPlanError::NotApplicable)
                },
                Ok,
            )
        }

        fn plan_edits(
            &self,
            _operation: &FixOperation,
            _primary: &Location,
            _snapshot: &WorkspaceSnapshot,
        ) -> Result<Vec<TextEdit>, OperationPlanError> {
            Ok(self.edits.clone())
        }
    }

    fn test_fix(
        title: impl Into<String>,
        applicability: Applicability,
        operation: FixOperation,
        edits: Vec<TextEdit>,
        placeholders: Vec<Placeholder>,
        primary: &Location,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Fix, FixValidationError> {
        Fix::new(
            title,
            applicability,
            operation,
            placeholders,
            primary,
            snapshot,
            &StaticFixPlanner {
                edits,
                operation_result: None,
            },
        )
    }

    #[test]
    fn fixes_reject_when_operation_result_and_text_edits_disagree() {
        let primary = test_location(None);
        let edit = TextEdit {
            file: primary.file.clone(),
            range: primary.span,
            new_text: "replacement".to_owned(),
        };
        let snapshot = snapshot_for(&primary, std::slice::from_ref(&edit));
        let planner = StaticFixPlanner {
            edits: vec![edit],
            operation_result: Some(BTreeMap::from([(
                FileId::new(1),
                b"operation result".to_vec(),
            )])),
        };

        assert_eq!(
            Fix::new(
                "replace node",
                Applicability::MachineApplicable,
                replace_node_operation(),
                vec![],
                &primary,
                &snapshot,
                &planner,
            ),
            Err(FixValidationError::OperationAndEditsDisagree)
        );
    }

    #[test]
    fn applying_fix_edits_preserves_the_files_first_line_ending_style() {
        let source = Arc::new(SourceFile::decode(FileId::new(1), b"old\r\nline").expect("source"));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::clone(&source),
            "src/lib.sai",
            None,
            None,
        )])
        .expect("single-file snapshot");
        let edit = TextEdit {
            file: SourceRef {
                path: "src/lib.sai".to_owned(),
                package: None,
                snapshot: None,
            },
            range: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(0), TextSize::new(3)).expect("ordered range"),
            ),
            new_text: "new\ntext".to_owned(),
        };

        assert_eq!(
            apply_text_edits(&snapshot, &[edit]).expect("valid edit"),
            BTreeMap::from([(FileId::new(1), b"new\r\ntext\r\nline".to_vec())])
        );
    }

    #[test]
    fn diagnostic_defaults_to_a_hard_error_without_optional_metadata() {
        let diagnostic = Diagnostic::new(
            Code::UnexpectedCharacter,
            Severity::Error,
            "unexpected character",
            None,
        );

        assert_eq!(diagnostic.code.as_str(), "UNEXPECTED_CHARACTER");
        assert_eq!(diagnostic.severity, Severity::Error);
        assert_eq!(diagnostic.level(), None);
        assert!(diagnostic.labels().is_empty());
        assert!(diagnostic.data.is_empty());
        assert!(diagnostic.cause().facts().is_empty());
    }

    #[test]
    fn diagnostic_codes_cover_the_complete_registry() {
        assert_eq!(
            Code::from_name("UNEXPECTED_CHARACTER"),
            Some(Code::UnexpectedCharacter)
        );
        assert_eq!(Code::from_name("EXPECTED_TOKEN"), Some(Code::ExpectedToken));
        assert_eq!(
            Code::from_name("CONFLICTING_LINT_LEVEL"),
            Some(Code::ConflictingLintLevel)
        );
        assert_eq!(
            Code::from_name("VISIBILITY_ON_TRAIT_IMPL_ITEM"),
            Some(Code::VisibilityOnTraitImplItem)
        );
        assert_eq!(
            Code::from_name("NO_ASYNC_CLOSURE"),
            Some(Code::NoAsyncClosure)
        );
        assert_eq!(
            Code::from_name("INVALID_EXPORT_TARGET"),
            Some(Code::InvalidExportTarget)
        );
        assert_eq!(
            Code::from_name("EXPLICIT_DISCRIMINANT_ON_DATA_ENUM"),
            Some(Code::ExplicitDiscriminantOnDataEnum)
        );
        assert_eq!(
            Code::from_name("FEATURE_NOT_IMPLEMENTED"),
            Some(Code::FeatureNotImplemented)
        );
        assert_eq!(Code::from_name("UNKNOWN_DERIVE"), Some(Code::UnknownDerive));
        assert_eq!(
            Code::from_name("GENERIC_INTERFACE_ITEM"),
            Some(Code::GenericInterfaceItem)
        );
        assert_eq!(
            Code::from_name("INTERFACE_FN_CLAUSE"),
            Some(Code::InterfaceFnClause)
        );
        assert_eq!(
            Code::from_name("EFFECTS_ON_INTERFACE_FN"),
            Some(Code::EffectsOnInterfaceFn)
        );
        assert_eq!(
            Code::from_name("INTERFACE_FN_BODY"),
            Some(Code::InterfaceFnBody)
        );
        assert_eq!(
            Code::from_name("WORLD_IMPORT_NEEDS_INTERFACE"),
            Some(Code::WorldImportNeedsInterface)
        );
        assert_eq!(Code::from_name("UNKNOWN_ABI"), Some(Code::UnknownAbi));
        assert_eq!(
            Code::from_name("INVALID_EXTERN_BLOCK"),
            Some(Code::InvalidExternBlock)
        );
        assert_eq!(
            Code::from_name("UNEXPECTED_FN_BODY"),
            Some(Code::UnexpectedFnBody)
        );
        assert_eq!(
            Code::from_name("DUPLICATE_BINDING"),
            Some(Code::DuplicateBinding)
        );
        assert_eq!(
            Code::from_name("OR_PATTERN_BINDING_MISMATCH"),
            Some(Code::OrPatternBindingMismatch)
        );
        assert!(Code::from_name("AMBIGUOUS_ASSOCIATED_TYPE").is_some());
        assert!(Code::from_name("WIT_PACKAGE_MISMATCH").is_some());
        assert_eq!(Code::from_name("NOT_A_DIAGNOSTIC"), None);
        assert_eq!(Code::from_name("UNKNOWN_LINT"), Some(Code::UnknownLint));
        assert_eq!(Code::from_name("NOT_A_LINT"), Some(Code::NotALint));

        let names = DIAGNOSTIC_CODE_NAMES
            .split_ascii_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(Code::ALL.len(), names.len());
        let mut code_names = Code::ALL
            .iter()
            .map(|code| code.as_str())
            .collect::<Vec<_>>();
        code_names.sort_unstable();
        assert_eq!(code_names, names);
        for name in names {
            let code = Code::from_name(name).expect("registered code");
            assert_eq!(code.as_str(), name);
            assert!(code.as_str().is_ascii());
        }

        let legacy_names = include_str!("diagnostic_code_legacy_order.txt")
            .split_ascii_whitespace()
            .collect::<Vec<_>>();
        assert_eq!(
            Code::ALL
                .iter()
                .take(legacy_names.len())
                .map(|code| code.as_str())
                .collect::<Vec<_>>(),
            legacy_names
        );
        for pair in legacy_names.windows(2) {
            assert!(
                Code::from_name(pair[0]).expect("registered legacy code")
                    < Code::from_name(pair[1]).expect("registered legacy code")
            );
        }
    }

    #[test]
    fn lint_name_lookup_uses_the_complete_v01_registry() {
        let registered = DIAGNOSTIC_CODE_NAMES
            .split_ascii_whitespace()
            .collect::<Vec<_>>();
        let unique = registered.iter().copied().collect::<BTreeSet<_>>();
        assert_eq!(registered.len(), 493);
        assert_eq!(unique.len(), registered.len());
        assert!(registered.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(LINT_REGISTRY.len(), 28);
        assert!(matches!(
            lookup_lint_name("INVALID_DESCRIPTION"),
            LintNameLookup::HardDiagnostic(_)
        ));
        assert!(
            registered
                .iter()
                .all(|name| !matches!(lookup_lint_name(name), LintNameLookup::Unknown))
        );

        for lint in LINT_REGISTRY {
            assert!(registered.contains(&lint.code));
            assert!(matches!(
                lookup_lint_name(lint.code),
                LintNameLookup::Lint(found) if found == lint
            ));
        }

        assert_eq!(
            lookup_lint_name("MISSING_SAFETY_DOC"),
            LintNameLookup::Lint(&LintRegistryEntry {
                code: "MISSING_SAFETY_DOC",
                default_level: LintLevel::Warn,
                stage: 2,
            })
        );
        assert_eq!(
            lookup_lint_name("CONTRACT_RUNTIME_CHECK"),
            LintNameLookup::Lint(&LintRegistryEntry {
                code: "CONTRACT_RUNTIME_CHECK",
                default_level: LintLevel::Info,
                stage: 3,
            })
        );
        assert_eq!(
            lookup_lint_name("MISSING_DOCS"),
            LintNameLookup::Lint(&LintRegistryEntry {
                code: "MISSING_DOCS",
                default_level: LintLevel::Allow,
                stage: 0,
            })
        );
        assert_eq!(
            lookup_lint_name("UNKNOWN_FIELD"),
            LintNameLookup::HardDiagnostic(Severity::Error)
        );
        assert_eq!(
            lookup_lint_name("MISPLACED_DOC_COMMENT"),
            LintNameLookup::HardDiagnostic(Severity::Warning)
        );
        assert_eq!(
            lookup_lint_name("LARGE_STACK_FRAME"),
            LintNameLookup::HardDiagnostic(Severity::Info)
        );
        assert_eq!(
            lookup_lint_name("UNUSED_VARIABLES"),
            LintNameLookup::Unknown
        );
        assert_eq!(lookup_lint_name("unused_variable"), LintNameLookup::Unknown);
        assert_eq!(
            lookup_lint_name("UNUSED_MUST_USE"),
            LintNameLookup::HardDiagnostic(Severity::Error)
        );
        assert!(
            Code::ALL.iter().all(|code| {
                !matches!(lookup_lint_name(code.as_str()), LintNameLookup::Unknown)
            })
        );
    }

    #[test]
    fn checked_in_registry_snapshot_matches_the_spec_tables() {
        let docs = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/spec");
        let appendix_path = docs.join("appendix-a-diagnostic-codes.md");
        let Ok(appendix) = std::fs::read_to_string(appendix_path) else {
            // Packaged crates do not contain the workspace's specification sources.
            return;
        };

        let mut documented_codes = appendix
            .lines()
            .filter_map(|line| {
                let row = line.strip_prefix("| `")?;
                let (code, _) = row.split_once('`')?;
                Some(code)
            })
            .collect::<Vec<_>>();
        documented_codes.sort_unstable();
        assert_eq!(
            DIAGNOSTIC_CODE_NAMES
                .split_ascii_whitespace()
                .collect::<Vec<_>>(),
            documented_codes
        );

        let mut documented_warnings = Vec::new();
        let mut documented_infos = Vec::new();
        for line in appendix.lines() {
            let columns = line.split('|').map(str::trim).collect::<Vec<_>>();
            if columns.len() < 4 {
                continue;
            }
            let Some(code) = columns[1]
                .strip_prefix('`')
                .and_then(|code| code.strip_suffix('`'))
            else {
                continue;
            };
            match columns[2] {
                "warning" => documented_warnings.push(code),
                "info" => documented_infos.push(code),
                _ => {}
            }
        }
        documented_warnings.sort_unstable();
        documented_infos.sort_unstable();
        let mut implemented_warnings = HARD_WARNING_CODES.to_vec();
        implemented_warnings.sort_unstable();
        let mut implemented_infos = HARD_INFO_CODES.to_vec();
        implemented_infos.sort_unstable();
        assert_eq!(implemented_warnings, documented_warnings);
        assert_eq!(implemented_infos, documented_infos);

        let spec = std::fs::read_to_string(docs.join("15-attributes-and-lints.md"))
            .expect("workspace lint specification");
        let lint_table = spec
            .split("### 15.17.2 The lints of v0.1")
            .nth(1)
            .and_then(|section| section.split("### 15.17.3").next())
            .expect("v0.1 lint registry table");
        let documented_lints = lint_table
            .lines()
            .filter_map(|line| {
                let columns = line.split('|').map(str::trim).collect::<Vec<_>>();
                if columns.len() < 6 {
                    return None;
                }
                let code = columns[1].strip_prefix('`')?.strip_suffix('`')?;
                let level = match columns[2].trim_matches('`') {
                    "allow" => LintLevel::Allow,
                    "info" => LintLevel::Info,
                    "warn" => LintLevel::Warn,
                    "deny" => LintLevel::Deny,
                    _ => return None,
                };
                let stage = columns[4].parse::<u8>().ok()?;
                Some((code, level, stage))
            })
            .collect::<Vec<_>>();
        let implemented_lints = LINT_REGISTRY
            .iter()
            .map(|lint| (lint.code, lint.default_level, lint.stage))
            .collect::<Vec<_>>();
        assert_eq!(implemented_lints, documented_lints);
    }

    #[test]
    fn cause_keys_deduplicate_and_order_poison_facts() {
        let body = PoisonFact::Body("pkg:module.fn".to_owned());
        let signature = PoisonFact::Signature("pkg:module.fn".to_owned());
        let cause = CauseKey::new([body.clone(), signature.clone(), body.clone()]);
        let expected = BTreeSet::from([body, signature]);

        assert_eq!(cause.facts(), &expected);
    }

    #[test]
    fn related_diagnostics_keep_machine_readable_borrow_facts() {
        let location = Location::new(
            "src/lib.sai",
            Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(2), TextSize::new(3)).expect("ordered range"),
            ),
        );
        let related = Related::BorrowConflict {
            place: "value.field".to_owned(),
            loan: location.clone(),
            loan_kind: LoanKind::Mutable,
            access: location.clone(),
            access_kind: AccessKind::Write,
            later_use: None,
        };

        assert!(matches!(
            related,
            Related::BorrowConflict {
                loan_kind: LoanKind::Mutable,
                access_kind: AccessKind::Write,
                later_use: None,
                ..
            }
        ));
    }

    #[test]
    fn node_ids_validate_groups_hex_and_canonicalize_indices() {
        let parsed = NodeId::parse("expr:82A1").expect("valid expression node ID");

        assert_eq!(parsed.group(), NodeGroup::Expression);
        assert_eq!(parsed.index(), 0x82A1);
        assert_eq!(parsed.to_string(), "expr:82A1");
        assert_eq!(NodeId::parse("unknown:0000"), None);
        assert_eq!(NodeId::parse("expr:82a1"), None);
        assert_eq!(NodeId::parse("expr:0"), None);
    }

    #[test]
    fn fixes_reject_inconsistent_placeholder_applicability() {
        let operation = replace_node_operation();
        let primary = test_location(None);
        let edit = TextEdit {
            file: SourceRef {
                path: "src/lib.sai".to_owned(),
                package: None,
                snapshot: None,
            },
            range: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(0), TextSize::new(1)).expect("ordered range"),
            ),
            new_text: "replacement".to_owned(),
        };
        let placeholder = Placeholder {
            edit: 0,
            start: 0,
            end: 4,
            label: "name".to_owned(),
        };
        let snapshot = snapshot_for(&primary, std::slice::from_ref(&edit));

        assert_eq!(
            test_fix(
                "replace node",
                Applicability::HasPlaceholders,
                operation.clone(),
                vec![edit.clone()],
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::PlaceholderApplicabilityMismatch)
        );
        assert_eq!(
            test_fix(
                "replace node",
                Applicability::MaybeIncorrect,
                operation,
                vec![edit],
                vec![placeholder],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::PlaceholderApplicabilityMismatch)
        );
    }

    #[test]
    fn fixes_reject_placeholder_ranges_inside_utf8_sequences() {
        let operation = replace_node_operation();
        let primary = test_location(None);
        let edit = TextEdit {
            file: SourceRef {
                path: "src/lib.sai".to_owned(),
                package: None,
                snapshot: None,
            },
            range: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(0), TextSize::new(1)).expect("ordered range"),
            ),
            new_text: "a🦀b".to_owned(),
        };
        let snapshot = snapshot_for(&primary, std::slice::from_ref(&edit));

        assert_eq!(
            test_fix(
                "replace node",
                Applicability::HasPlaceholders,
                operation,
                vec![edit],
                vec![Placeholder {
                    edit: 0,
                    start: 2,
                    end: 4,
                    label: "value".to_owned(),
                }],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::PlaceholderOutOfBounds)
        );
    }

    #[test]
    fn fixes_require_sorted_non_overlapping_edits() {
        let operation = replace_node_operation();
        let primary = test_location(None);
        let file = SourceRef {
            path: "src/lib.sai".to_owned(),
            package: None,
            snapshot: None,
        };
        let edit = |start, end| TextEdit {
            file: file.clone(),
            range: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(start), TextSize::new(end)).expect("ordered range"),
            ),
            new_text: String::new(),
        };
        let edits = vec![edit(4, 8), edit(2, 3)];
        let snapshot = snapshot_for(&primary, &edits);

        assert_eq!(
            test_fix(
                "remove text",
                Applicability::MachineApplicable,
                operation,
                edits,
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::EditsNotOrderedOrOverlapping)
        );
    }

    #[test]
    fn fix_operations_validate_required_allowed_and_typed_arguments() {
        let node = NodeId::new(NodeGroup::Expression, 0x82A1);

        assert_eq!(
            FixOperation::new(FixOperationName::ReplaceField, node, ValueObject::new(),),
            Err(FixOperationValidationError::MissingParameter)
        );
        assert_eq!(
            FixOperation::new(
                FixOperationName::ReplaceField,
                node,
                ValueObject::from([("text".to_owned(), Value::String("name".to_owned()))]),
            ),
            Err(FixOperationValidationError::UnknownParameter)
        );
        assert_eq!(
            FixOperation::new(
                FixOperationName::ReplaceField,
                node,
                ValueObject::from([("field".to_owned(), Value::Integer(1))]),
            ),
            Err(FixOperationValidationError::InvalidParameterType)
        );
        assert_eq!(
            FixOperation::new(
                FixOperationName::ReplaceField,
                node,
                ValueObject::from([("field".to_owned(), Value::String("name".to_owned()))]),
            )
            .expect("valid replace_field operation")
            .name(),
            FixOperationName::ReplaceField
        );
    }

    #[test]
    fn set_effects_rejects_duplicate_effects() {
        let node = NodeId::new(NodeGroup::Item, 0x82A1);
        let args = ValueObject::from([(
            "effects".to_owned(),
            Value::Array(vec![
                Value::String("fs.read".to_owned()),
                Value::String("alloc".to_owned()),
                Value::String("alloc".to_owned()),
            ]),
        )]);

        assert_eq!(
            FixOperation::new(FixOperationName::SetEffects, node, args),
            Err(FixOperationValidationError::InvalidParameterValue)
        );
    }

    #[test]
    fn set_effects_requires_canonical_builtin_order() {
        let node = NodeId::new(NodeGroup::Item, 0x82A1);
        let effects = |names: &[&str]| {
            ValueObject::from([(
                "effects".to_owned(),
                Value::Array(
                    names
                        .iter()
                        .map(|name| Value::String((*name).to_owned()))
                        .collect(),
                ),
            )])
        };

        assert_eq!(
            FixOperation::new(
                FixOperationName::SetEffects,
                node,
                effects(&["fs.read", "alloc"]),
            ),
            Err(FixOperationValidationError::InvalidParameterValue)
        );
        assert!(
            FixOperation::new(
                FixOperationName::SetEffects,
                node,
                effects(&["alloc", "fs.read"]),
            )
            .is_ok()
        );
    }

    #[test]
    fn set_effects_requires_resolved_order_for_non_builtin_effects() {
        let node = NodeId::new(NodeGroup::Item, 0x82A1);
        let effects = |names: &[&str]| {
            ValueObject::from([(
                "effects".to_owned(),
                Value::Array(
                    names
                        .iter()
                        .map(|name| Value::String((*name).to_owned()))
                        .collect(),
                ),
            )])
        };

        assert_eq!(
            FixOperation::new(
                FixOperationName::SetEffects,
                node,
                effects(&["pkg::zeta", "pkg::alpha"]),
            ),
            Err(FixOperationValidationError::CanonicalEffectsRequired)
        );

        let canonical = CanonicalEffectSet::from_resolved([
            ResolvedEffect::User {
                identity: "symbol://pkg#alpha".to_owned(),
                spelling: "pkg::alpha".to_owned(),
            },
            ResolvedEffect::User {
                identity: "symbol://pkg#zeta".to_owned(),
                spelling: "pkg::zeta".to_owned(),
            },
        ])
        .expect("distinct resolved effects");
        assert!(
            FixOperation::new_with_canonical_effects(
                FixOperationName::SetEffects,
                node,
                effects(&["pkg::alpha", "pkg::zeta"]),
                &canonical,
            )
            .is_ok()
        );
        assert_eq!(
            FixOperation::new_with_canonical_effects(
                FixOperationName::SetEffects,
                node,
                effects(&["pkg::zeta", "pkg::alpha"]),
                &canonical,
            ),
            Err(FixOperationValidationError::InvalidParameterValue)
        );
    }

    #[test]
    fn canonical_effect_sets_sort_by_kind_identity_and_spelling() {
        let effects = CanonicalEffectSet::from_resolved([
            ResolvedEffect::User {
                identity: "symbol://pkg#zeta".to_owned(),
                spelling: "zeta".to_owned(),
            },
            ResolvedEffect::Interface {
                identity: "wit://z".to_owned(),
                spelling: "alpha-interface".to_owned(),
            },
            ResolvedEffect::Builtin("fs.read".to_owned()),
            ResolvedEffect::Interface {
                identity: "wit://a".to_owned(),
                spelling: "zeta-interface".to_owned(),
            },
            ResolvedEffect::User {
                identity: "symbol://pkg#alpha".to_owned(),
                spelling: "alpha".to_owned(),
            },
            ResolvedEffect::Builtin("alloc".to_owned()),
        ])
        .expect("unique effects");

        assert_eq!(
            effects.spellings().map(String::as_str).collect::<Vec<_>>(),
            [
                "alloc",
                "fs.read",
                "zeta-interface",
                "alpha-interface",
                "alpha",
                "zeta",
            ]
        );
    }

    #[test]
    fn canonical_effect_sets_reject_aliases_with_duplicate_identity() {
        assert_eq!(
            CanonicalEffectSet::from_resolved([
                ResolvedEffect::User {
                    identity: "symbol://pkg#effect".to_owned(),
                    spelling: "effect".to_owned(),
                },
                ResolvedEffect::User {
                    identity: "symbol://pkg#effect".to_owned(),
                    spelling: "alias".to_owned(),
                },
            ]),
            Err(CanonicalEffectSetError::DuplicateIdentity)
        );
    }

    #[test]
    fn canonical_effect_sets_reject_ambiguous_printed_spellings() {
        assert_eq!(
            CanonicalEffectSet::from_resolved([
                ResolvedEffect::User {
                    identity: "symbol://pkg#first".to_owned(),
                    spelling: "same".to_owned(),
                },
                ResolvedEffect::User {
                    identity: "symbol://pkg#second".to_owned(),
                    spelling: "same".to_owned(),
                },
            ]),
            Err(CanonicalEffectSetError::DuplicateSpelling)
        );
    }

    #[test]
    fn fixes_reject_primary_snapshot_mismatches() {
        let primary = test_location(Some(8));
        let edit = TextEdit {
            file: SourceRef {
                path: "src/lib.sai".to_owned(),
                package: None,
                snapshot: Some(7),
            },
            range: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(0), TextSize::new(1)).expect("ordered range"),
            ),
            new_text: "replacement".to_owned(),
        };
        let snapshot = snapshot_for(&primary, std::slice::from_ref(&edit));

        assert_eq!(
            test_fix(
                "replace node",
                Applicability::MachineApplicable,
                replace_node_operation(),
                vec![edit],
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::SnapshotMismatch)
        );
    }

    #[test]
    fn fixes_reject_a_secondary_edit_from_an_older_workspace_snapshot() {
        let primary = Location {
            file: SourceRef {
                path: "src/a.sai".to_owned(),
                package: Some("sai:demo@0.1.0".to_owned()),
                snapshot: Some(8),
            },
            span: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(0), TextSize::new(1)).expect("ordered range"),
            ),
        };
        let stale_edit = TextEdit {
            file: SourceRef {
                path: "src/b.sai".to_owned(),
                package: Some("sai:demo@0.1.0".to_owned()),
                snapshot: Some(4),
            },
            range: Span::new(
                FileId::new(2),
                TextRange::new(TextSize::new(0), TextSize::new(1)).expect("ordered range"),
            ),
            new_text: "replacement".to_owned(),
        };
        let snapshot = WorkspaceSnapshot::new([
            SnapshotFile::new(
                Arc::new(SourceFile::new(FileId::new(1), "a", false).expect("source")),
                "src/a.sai",
                Some("sai:demo@0.1.0".to_owned()),
                Some(8),
            ),
            SnapshotFile::new(
                Arc::new(SourceFile::new(FileId::new(2), "b", false).expect("source")),
                "src/b.sai",
                Some("sai:demo@0.1.0".to_owned()),
                Some(5),
            ),
        ])
        .expect("consistent current workspace snapshot");

        assert_eq!(
            test_fix(
                "edit another file",
                Applicability::MachineApplicable,
                replace_node_operation(),
                vec![stale_edit],
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::SnapshotMismatch)
        );
    }

    #[test]
    fn diagnostics_reject_fixes_created_from_another_workspace_snapshot() {
        let primary = test_location(Some(8));
        let make_snapshot = |source: &str| {
            WorkspaceSnapshot::new([SnapshotFile::new(
                Arc::new(SourceFile::new(FileId::new(1), source, false).expect("source")),
                "src/lib.sai",
                None,
                Some(8),
            )])
            .expect("single-file snapshot")
        };
        let fix_snapshot = make_snapshot("x");
        let diagnostic_snapshot = make_snapshot("y");
        let edit = TextEdit {
            file: primary.file.clone(),
            range: primary.span,
            new_text: "replacement".to_owned(),
        };
        let fix = test_fix(
            "replace node",
            Applicability::MachineApplicable,
            replace_node_operation(),
            vec![edit],
            vec![],
            &primary,
            &fix_snapshot,
        )
        .expect("fix matches its workspace snapshot");
        let diagnostic = Diagnostic::new(
            Code::UnexpectedCharacter,
            Severity::Error,
            "unexpected character",
            Some(primary),
        )
        .with_workspace_snapshot(&diagnostic_snapshot)
        .expect("diagnostic matches its workspace snapshot");

        assert_eq!(
            diagnostic.with_fix(fix),
            Err(FixValidationError::SnapshotMismatch)
        );
    }

    #[test]
    fn fixes_reject_ranges_inside_utf8_characters_and_crlf_pairs() {
        let source = Arc::new(SourceFile::new(FileId::new(1), "é\r\nx", false).expect("source"));
        let snapshot =
            WorkspaceSnapshot::new([SnapshotFile::new(source, "src/lib.sai", None, None)])
                .expect("single-file snapshot");
        let primary = Location::new(
            "src/lib.sai",
            Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(0), TextSize::new(2)).expect("ordered range"),
            ),
        );
        let split_crlf = TextEdit {
            file: primary.file.clone(),
            range: Span::new(FileId::new(1), TextRange::empty(TextSize::new(3))),
            new_text: String::new(),
        };

        assert_eq!(
            test_fix(
                "split CR LF",
                Applicability::MachineApplicable,
                replace_node_operation(),
                vec![split_crlf],
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::InvalidSourceRange)
        );

        let split_utf8 = TextEdit {
            file: primary.file.clone(),
            range: Span::new(FileId::new(1), TextRange::empty(TextSize::new(1))),
            new_text: String::new(),
        };
        assert_eq!(
            test_fix(
                "split UTF 8",
                Applicability::MachineApplicable,
                replace_node_operation(),
                vec![split_utf8],
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::InvalidSourceRange)
        );
    }

    #[test]
    fn diagnostics_require_snapshot_binding_before_accepting_fixes() {
        let primary = test_location(None);
        let edit = TextEdit {
            file: primary.file.clone(),
            range: primary.span,
            new_text: "replacement".to_owned(),
        };
        let snapshot = snapshot_for(&primary, std::slice::from_ref(&edit));
        let fix = test_fix(
            "replace node",
            Applicability::MachineApplicable,
            replace_node_operation(),
            vec![edit],
            vec![],
            &primary,
            &snapshot,
        )
        .expect("fix matches its snapshot");
        let diagnostic = Diagnostic::new(
            Code::UnexpectedCharacter,
            Severity::Error,
            "unexpected character",
            Some(primary),
        );

        assert_eq!(
            diagnostic.with_fix(fix),
            Err(FixValidationError::MissingWorkspaceSnapshot)
        );
    }

    #[test]
    fn diagnostics_cannot_be_rebound_after_accepting_snapshot_fixes() {
        let primary = test_location(None);
        let make_snapshot = || {
            WorkspaceSnapshot::new([SnapshotFile::new(
                Arc::new(SourceFile::new(FileId::new(1), "x", false).expect("source")),
                "src/lib.sai",
                None,
                None,
            )])
            .expect("single-file snapshot")
        };
        let first_snapshot = make_snapshot();
        let second_snapshot = make_snapshot();
        let edit = TextEdit {
            file: primary.file.clone(),
            range: primary.span,
            new_text: "replacement".to_owned(),
        };
        let fix = test_fix(
            "replace node",
            Applicability::MachineApplicable,
            replace_node_operation(),
            vec![edit],
            vec![],
            &primary,
            &first_snapshot,
        )
        .expect("fix matches first snapshot");
        let diagnostic = Diagnostic::new(
            Code::UnexpectedCharacter,
            Severity::Error,
            "unexpected character",
            Some(primary),
        )
        .with_workspace_snapshot(&first_snapshot)
        .expect("diagnostic matches first snapshot")
        .with_fix(fix)
        .expect("diagnostic accepts matching fix");

        assert_eq!(
            diagnostic.with_workspace_snapshot(&second_snapshot),
            Err(FixValidationError::SnapshotMismatch)
        );
    }

    #[test]
    fn diagnostic_secondary_locations_must_belong_to_its_snapshot() {
        let primary = test_location(None);
        let other = Location::new(
            "src/other.sai",
            Span::new(
                FileId::new(2),
                TextRange::new(TextSize::new(0), TextSize::new(1)).expect("ordered range"),
            ),
        );
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            Arc::new(SourceFile::new(FileId::new(1), "x", false).expect("source")),
            primary.file.path.clone(),
            None,
            None,
        )])
        .expect("single-file snapshot");
        let mut diagnostic = Diagnostic::new(
            Code::UnexpectedCharacter,
            Severity::Error,
            "unexpected character",
            Some(primary.clone()),
        );
        diagnostic
            .push_label(Label {
                location: other.clone(),
                message: "from another document".to_owned(),
            })
            .expect("unbound diagnostics may collect locations");

        assert_eq!(
            diagnostic.with_workspace_snapshot(&snapshot),
            Err(FixValidationError::SnapshotMismatch)
        );

        let mut bound = Diagnostic::new(
            Code::UnexpectedCharacter,
            Severity::Error,
            "unexpected character",
            Some(primary),
        )
        .with_workspace_snapshot(&snapshot)
        .expect("primary belongs to snapshot");
        assert_eq!(
            bound.push_related(Related::DeclaredHere {
                location: other,
                what: "declaration".to_owned(),
            }),
            Err(FixValidationError::SnapshotMismatch)
        );
    }

    #[test]
    fn diagnostics_validate_fixes_against_their_primary_location() {
        let primary = test_location(Some(8));
        let edit = TextEdit {
            file: primary.file.clone(),
            range: primary.span,
            new_text: "replacement".to_owned(),
        };
        let snapshot = snapshot_for(&primary, std::slice::from_ref(&edit));
        let fix = test_fix(
            "replace node",
            Applicability::MachineApplicable,
            replace_node_operation(),
            vec![edit],
            vec![],
            &primary,
            &snapshot,
        )
        .expect("fix matches primary snapshot");
        let diagnostic = Diagnostic::new(
            Code::UnexpectedCharacter,
            Severity::Error,
            "unexpected character",
            Some(primary),
        )
        .with_workspace_snapshot(&snapshot)
        .expect("diagnostic belongs to the same snapshot")
        .with_fix(fix)
        .expect("diagnostic accepts matching fix");

        assert_eq!(diagnostic.fixes().len(), 1);
    }

    #[test]
    fn fixes_cannot_be_attached_to_a_different_primary_snapshot() {
        let primary = test_location(Some(8));
        let edit = TextEdit {
            file: SourceRef {
                path: "src/other.sai".to_owned(),
                package: None,
                snapshot: Some(3),
            },
            range: Span::new(
                FileId::new(2),
                TextRange::new(TextSize::new(4), TextSize::new(5)).expect("ordered range"),
            ),
            new_text: "replacement".to_owned(),
        };
        let snapshot = snapshot_for(&primary, std::slice::from_ref(&edit));
        let fix = test_fix(
            "replace node",
            Applicability::MachineApplicable,
            replace_node_operation(),
            vec![edit],
            vec![],
            &primary,
            &snapshot,
        )
        .expect("secondary-file edit is distinct from the primary file");
        let different_primary = Location {
            file: SourceRef {
                path: "src/other.sai".to_owned(),
                package: None,
                snapshot: Some(3),
            },
            span: Span::new(
                FileId::new(2),
                TextRange::new(TextSize::new(4), TextSize::new(5)).expect("ordered range"),
            ),
        };
        let diagnostic = Diagnostic::new(
            Code::UnexpectedCharacter,
            Severity::Error,
            "unexpected character",
            Some(different_primary),
        )
        .with_workspace_snapshot(&snapshot)
        .expect("other primary file is in the snapshot");

        assert_eq!(
            diagnostic.with_fix(fix),
            Err(FixValidationError::SnapshotMismatch)
        );
    }

    #[test]
    fn a_secondary_path_cannot_reuse_the_primary_file_id() {
        let primary = test_location(None);
        let edit = TextEdit {
            file: SourceRef {
                path: "src/other.sai".to_owned(),
                package: None,
                snapshot: None,
            },
            range: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(4), TextSize::new(5)).expect("ordered range"),
            ),
            new_text: "replacement".to_owned(),
        };
        let snapshot = snapshot_for(&primary, std::slice::from_ref(&edit));

        assert_eq!(
            test_fix(
                "replace node",
                Applicability::MachineApplicable,
                replace_node_operation(),
                vec![edit],
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::SnapshotMismatch)
        );
    }

    #[test]
    fn distinct_secondary_paths_cannot_share_a_file_id() {
        let primary = test_location(None);
        let edit = |path: &str| TextEdit {
            file: SourceRef {
                path: path.to_owned(),
                package: None,
                snapshot: None,
            },
            range: Span::new(
                FileId::new(2),
                TextRange::new(TextSize::new(4), TextSize::new(5)).expect("ordered range"),
            ),
            new_text: "replacement".to_owned(),
        };
        let edits = vec![edit("src/first.sai"), edit("src/second.sai")];
        let snapshot = snapshot_for(&primary, &edits);

        assert_eq!(
            test_fix(
                "replace two nodes",
                Applicability::MachineApplicable,
                replace_node_operation(),
                edits,
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::SnapshotMismatch)
        );
    }

    #[test]
    fn fixes_allow_a_replacement_after_an_insertion_at_the_same_offset() {
        let primary = test_location(None);
        let edit = |start, end, new_text: &str| TextEdit {
            file: primary.file.clone(),
            range: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(start), TextSize::new(end)).expect("ordered range"),
            ),
            new_text: new_text.to_owned(),
        };
        let edits = vec![edit(4, 4, "prefix"), edit(4, 7, "replacement")];
        let snapshot = snapshot_for(&primary, &edits);

        assert!(
            test_fix(
                "insert before replacement",
                Applicability::MachineApplicable,
                replace_node_operation(),
                edits.clone(),
                vec![],
                &primary,
                &snapshot,
            )
            .is_ok()
        );
        assert_eq!(
            test_fix(
                "duplicate insertions",
                Applicability::MachineApplicable,
                replace_node_operation(),
                vec![edit(4, 4, "a"), edit(4, 4, "b")],
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::EditsNotOrderedOrOverlapping)
        );
    }

    #[test]
    fn fixes_reject_non_lf_edit_text() {
        let primary = test_location(None);
        let edit = TextEdit {
            file: SourceRef {
                path: "src/lib.sai".to_owned(),
                package: None,
                snapshot: None,
            },
            range: Span::new(
                FileId::new(1),
                TextRange::new(TextSize::new(0), TextSize::new(1)).expect("ordered range"),
            ),
            new_text: "a\r\nb".to_owned(),
        };
        let snapshot = snapshot_for(&primary, std::slice::from_ref(&edit));

        assert_eq!(
            test_fix(
                "replace node",
                Applicability::MachineApplicable,
                replace_node_operation(),
                vec![edit],
                vec![],
                &primary,
                &snapshot,
            ),
            Err(FixValidationError::InvalidNewTextLineEnding)
        );
    }

    #[test]
    fn diagnostic_data_objects_have_a_deterministic_key_order() {
        let mut data = ValueObject::new();
        data.insert("z".to_owned(), Value::Integer(2));
        data.insert("a".to_owned(), Value::Integer(1));

        assert_eq!(
            data.keys().map(String::as_str).collect::<Vec<_>>(),
            ["a", "z"]
        );
    }

    #[test]
    fn diagnostic_templates_ignore_snapshot_identity_and_rebind_all_locations() {
        fn template(file_id: u32, path: &str, snapshot: u64) -> DiagnosticTemplate {
            let file = SourceRef {
                path: path.to_owned(),
                package: Some("sai:test@0.1.0".to_owned()),
                snapshot: Some(snapshot),
            };
            let range =
                TextRange::new(TextSize::new(2), TextSize::new(5)).expect("valid source range");
            let mut diagnostic = Diagnostic::new(
                Code::ExpectedToken,
                Severity::Error,
                "expected `)`",
                Some(Location {
                    file: file.clone(),
                    span: Span::new(FileId::new(file_id), range),
                }),
            );
            diagnostic.primary_label = "close this list".to_owned();
            diagnostic.labels.push(Label {
                location: Location {
                    file: file.clone(),
                    span: Span::new(
                        FileId::new(file_id),
                        TextRange::new(TextSize::new(0), TextSize::new(1))
                            .expect("valid label range"),
                    ),
                },
                message: "opening delimiter".to_owned(),
            });
            diagnostic.children.push(Child {
                kind: ChildKind::Note,
                message: "parser recovered here".to_owned(),
                location: Some(Location {
                    file,
                    span: Span::new(
                        FileId::new(file_id),
                        TextRange::new(TextSize::new(5), TextSize::new(5))
                            .expect("valid empty range"),
                    ),
                }),
            });
            diagnostic.data.insert(
                "expected".to_owned(),
                Value::Array(vec![Value::String(")".to_owned()), Value::Null]),
            );
            diagnostic.to_template().expect("diagnostic is query-safe")
        }

        let first = template(3, "src/main.sai", 1);
        let second = template(19, "dep:acme:lib@2.4.1/src/main.sai", 99);
        assert_eq!(first, second);
        assert_eq!(fingerprint(&first), fingerprint(&second));
        let mut changed_message = first.clone();
        changed_message.message.push('.');
        assert_ne!(fingerprint(&first), fingerprint(&changed_message));
        let mut changed_data = first.clone();
        changed_data.data.insert(
            "expected".to_owned(),
            Value::Array(vec![Value::String("different".to_owned())]),
        );
        assert_ne!(fingerprint(&first), fingerprint(&changed_data));

        let rebound_file = SourceRef {
            path: "dep:acme:lib@2.4.1/src/main.sai".to_owned(),
            package: Some("acme:lib@2.4.1".to_owned()),
            snapshot: Some(100),
        };
        let rebound = first.bind(FileId::new(27), rebound_file.clone());
        assert_eq!(rebound.primary().expect("primary").file, rebound_file);
        assert_eq!(
            rebound.primary().expect("primary").span.file,
            FileId::new(27)
        );
        assert_eq!(rebound.labels()[0].location.span.file, FileId::new(27));
        assert_eq!(
            rebound.children()[0]
                .location
                .as_ref()
                .expect("child location")
                .span
                .file,
            FileId::new(27)
        );
        assert_eq!(
            rebound.data.get("expected"),
            Some(&Value::Array(vec![
                Value::String(")".to_owned()),
                Value::Null
            ]))
        );

        let validated_file = SourceRef {
            path: "src/main.sai".to_owned(),
            package: None,
            snapshot: Some(101),
        };
        let validated_source =
            Arc::new(SourceFile::new(FileId::new(28), "abcdef", false).expect("valid source"));
        let snapshot = WorkspaceSnapshot::new([SnapshotFile::new(
            validated_source,
            validated_file.path.clone(),
            validated_file.package.clone(),
            validated_file.snapshot,
        )])
        .expect("valid snapshot");
        first
            .bind(FileId::new(28), validated_file)
            .with_workspace_snapshot(&snapshot)
            .expect("template locations bind to the current snapshot");
    }

    #[test]
    fn diagnostic_templates_reject_mixed_files_and_snapshot_bound_state() {
        let first_file = SourceRef {
            path: "src/main.sai".to_owned(),
            package: None,
            snapshot: Some(1),
        };
        let second_file = SourceRef {
            path: "src/other.sai".to_owned(),
            package: None,
            snapshot: Some(1),
        };
        let span = TextRange::new(TextSize::new(0), TextSize::new(1)).expect("valid range");
        let mut diagnostic = Diagnostic::new(
            Code::ExpectedToken,
            Severity::Error,
            "expected token",
            Some(Location {
                file: first_file.clone(),
                span: Span::new(FileId::new(1), span),
            }),
        );
        diagnostic.labels.push(Label {
            location: Location {
                file: second_file,
                span: Span::new(FileId::new(2), span),
            },
            message: "other file".to_owned(),
        });
        assert_eq!(
            diagnostic.to_template(),
            Err(DiagnosticTemplateError::MixedSource)
        );

        let mut diagnostic =
            Diagnostic::new(Code::ExpectedToken, Severity::Error, "expected", None);
        diagnostic
            .set_level(Some(LevelInfo {
                value: LintLevel::Deny,
                source: LevelSource::Attribute,
                location: None,
            }))
            .expect("unbound diagnostic accepts a level");
        assert_eq!(
            diagnostic.to_template(),
            Err(DiagnosticTemplateError::ContextualState)
        );
    }

    #[test]
    fn diagnostic_template_rejects_deep_data_and_hashes_it_iteratively() {
        let mut accepted_value = Value::Null;
        for _ in 1..DIAGNOSTIC_TEMPLATE_MAX_VALUE_DEPTH {
            accepted_value = Value::Array(vec![accepted_value]);
        }
        let mut accepted =
            Diagnostic::new(Code::ExpectedToken, Severity::Error, "expected token", None);
        accepted.data.insert("nested".to_owned(), accepted_value);
        assert!(accepted.to_template().is_ok());

        let mut nested = Value::Null;
        for _ in 0..20_000 {
            nested = Value::Array(vec![nested]);
        }
        let mut diagnostic =
            Diagnostic::new(Code::ExpectedToken, Severity::Error, "expected token", None);
        diagnostic.data.insert("nested".to_owned(), nested);
        assert_eq!(
            diagnostic.to_template(),
            Err(DiagnosticTemplateError::ValueTooDeep)
        );
        let mut hasher = StableHasher::new();
        stable_hash_value_object(&diagnostic.data, &mut hasher);
        assert_ne!(hasher.finish(), StableHasher::new().finish());
        // Value uses recursive drop; keep this over-limit stress fixture alive
        // until process exit after checking the template boundary and hasher.
        std::mem::forget(diagnostic);
    }
}
