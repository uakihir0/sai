#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::collections::BTreeSet;

pub use sai_vfs::{FileId, Span, TextRange, TextSize, WorkspaceSnapshot, WorkspaceSnapshotId};

macro_rules! define_codes {
    ($($variant:ident => $name:literal),+ $(,)?) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub enum Code {
            $($variant),+
        }

        impl Code {
            /// Subset of the documented diagnostic registry implemented here.
            pub const ALL: &'static [Self] = &[$(Self::$variant),+];

            pub const fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $name),+
                }
            }

            pub fn from_name(name: &str) -> Option<Self> {
                Self::ALL
                    .iter()
                    .copied()
                    .find(|code| code.as_str() == name)
            }
        }
    };
}

define_codes! {
    InvalidUtf8 => "INVALID_UTF8",
    InvalidLineEnding => "INVALID_LINE_ENDING",
    ForbiddenCharacter => "FORBIDDEN_CHARACTER",
    UnterminatedComment => "UNTERMINATED_COMMENT",
    MisplacedDocComment => "MISPLACED_DOC_COMMENT",
    IdentNotNfc => "IDENT_NOT_NFC",
    NonAsciiIdentifier => "NON_ASCII_IDENTIFIER",
    NonAsciiPublicName => "NON_ASCII_PUBLIC_NAME",
    UnnecessaryRawIdent => "UNNECESSARY_RAW_IDENT",
    InvalidRawIdent => "INVALID_RAW_IDENT",
    ReservedWord => "RESERVED_WORD",
    NoNull => "NO_NULL",
    NoExceptions => "NO_EXCEPTIONS",
    MalformedNumber => "MALFORMED_NUMBER",
    LiteralOutOfRange => "LITERAL_OUT_OF_RANGE",
    InvalidEscape => "INVALID_ESCAPE",
    UnterminatedString => "UNTERMINATED_STRING",
    InconsistentIndent => "INCONSISTENT_INDENT",
    UnterminatedChar => "UNTERMINATED_CHAR",
    NoAsyncClosure => "NO_ASYNC_CLOSURE",
    InvalidCharLiteral => "INVALID_CHAR_LITERAL",
    MalformedRawString => "MALFORMED_RAW_STRING",
    MalformedMultilineString => "MALFORMED_MULTILINE_STRING",
    InvalidInterpolation => "INVALID_INTERPOLATION",
    InvalidFormatSpec => "INVALID_FORMAT_SPEC",
    MalformedPackagePath => "MALFORMED_PACKAGE_PATH",
    UnexpectedCharacter => "UNEXPECTED_CHARACTER",
    NoSemicolon => "NO_SEMICOLON",
    NoBangOperator => "NO_BANG_OPERATOR",
    NoDoubleColon => "NO_DOUBLE_COLON",
    UseAndOr => "USE_AND_OR",
    UnexpectedNewline => "UNEXPECTED_NEWLINE",
    ExpectedToken => "EXPECTED_TOKEN",
    ExpectedExpression => "EXPECTED_EXPRESSION",
    MissingInitializer => "MISSING_INITIALIZER",
    ItemInBlock => "ITEM_IN_BLOCK",
    BlockInCondition => "BLOCK_IN_CONDITION",
    ChainedComparison => "CHAINED_COMPARISON",
    ChainedRange => "CHAINED_RANGE",
    BaseArgNotLast => "BASE_ARG_NOT_LAST",
    InvalidTupleIndex => "INVALID_TUPLE_INDEX",
    FloatPattern => "FLOAT_PATTERN",
    MultipleRestPatterns => "MULTIPLE_REST_PATTERNS",
    MissingFnBody => "MISSING_FN_BODY",
    ImplementationLimitExceeded => "IMPLEMENTATION_LIMIT_EXCEEDED",
    ClauseOrder => "CLAUSE_ORDER",
    DuplicateClause => "DUPLICATE_CLAUSE",
    VisibilityOnTraitImplItem => "VISIBILITY_ON_TRAIT_IMPL_ITEM",
    InvalidAttributeTarget => "INVALID_ATTRIBUTE_TARGET",
    RedundantPubExport => "REDUNDANT_PUB_EXPORT",
    InvalidExportTarget => "INVALID_EXPORT_TARGET",
    ExplicitDiscriminantOnDataEnum => "EXPLICIT_DISCRIMINANT_ON_DATA_ENUM",
}

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

fn first_line_uses_crlf(bytes: &[u8]) -> bool {
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
    use sai_vfs::{SnapshotFile, SourceFile};
    use std::sync::Arc;

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
    fn diagnostic_codes_are_limited_to_the_implemented_registry() {
        assert_eq!(
            Code::from_name("UNEXPECTED_CHARACTER"),
            Some(Code::UnexpectedCharacter)
        );
        assert_eq!(Code::from_name("EXPECTED_TOKEN"), Some(Code::ExpectedToken));
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
        assert_eq!(Code::from_name("NOT_A_DIAGNOSTIC"), None);
        assert!(Code::ALL.iter().all(|code| code.as_str().is_ascii()));
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
}
