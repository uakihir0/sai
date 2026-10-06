use crate::item_tree::AttrName;

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

const TRAIT_FN_WITH_BODY: &[TargetRule] = &[
    TargetRule::new(TargetKind::Fn, ANY),
    TargetRule::new(TargetKind::TraitFn, TargetCondition::HasBody),
    TargetRule::new(TargetKind::ImplFn, ANY),
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
        targets: TRAIT_FN_WITH_BODY,
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
        targets: TRAIT_FN_WITH_BODY,
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

#[cfg(test)]
mod tests {
    use super::*;
    use sai_syntax::ATTRIBUTE_NAMES;

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
}
