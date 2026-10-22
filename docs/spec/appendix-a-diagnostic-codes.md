# Appendix A. Diagnostic Codes

**Status:** Normative (generated from the diagnostics tables of the owner documents)
**Stage:** 0–7
**Related:** [01](01-introduction.md) §1.4, [15](15-attributes-and-lints.md) §15.17, [impl/11](../impl/11-diagnostics.md)

This appendix lists every diagnostic code of SAI v0.1 once. Each code has exactly one owner document, which defines when the code is reported. Other documents that report the same code refer to the owner. The severity is the one defined by the owner; "(lint)" marks the codes of the lint registry ([15](15-attributes-and-lints.md) §15.17.2), whose level can be changed. Every other code is hard: its severity is fixed.

Section references in a summary are to the owner document. When this appendix and an owner document disagree, the owner document decides, and the disagreement is a defect of this appendix.

## A.1 Rules

1. A code is an identifier in `UPPER_SNAKE_CASE`. Its meaning, severity and class never change within a major language version ([impl/11](../impl/11-diagnostics.md) §11.12).
2. A code has exactly one owner. A document that reuses a code writes "defined in" and the owner in its diagnostics table.
3. Codes owned by implementation documents (`impl/*`) report facts about the toolchain itself, such as an unreadable source file or an unknown code given to `sai explain`. They are part of the conformance surface like every other code.
4. A new code is added to the diagnostics table of its owner and to this appendix. A new warning or info is also added to [15](15-attributes-and-lints.md) §15.17.2 or §15.17.3.

## A.2 Language codes

Codes owned by the language specification (`spec/`). 344 codes.

| Code | Severity | Owner | Summary |
|---|---|---|---|
| `ALIAS_CASE_MISMATCH` | error | [04](04-modules-and-names.md) | An item alias has a different case class from the item |
| `ALIAS_CYCLE` | error | [05](05-types.md) | A type alias that expands to itself |
| `AMBIENT_IMPORT` | warning (lint) | [08](08-effects-and-capabilities.md) | A `use` of a `wasi:` interface outside the `sai` namespace; error under `policy { deny ambient_imports }` (§8.5.3); named by [decisions.md](../design/decisions.md) D-CAP-02 |
| `AMBIGUOUS_ASSOCIATED_TYPE` | error | [05](05-types.md) | `T.A` where several bound traits of `T` have an associated type `A` |
| `AMBIGUOUS_IMPL_REGION` | error | [07](07-ownership-and-borrowing.md) | A region position in an associated type or trait argument of an impl whose target carries several regions (§7.7.5, N-B-12) |
| `AMBIGUOUS_METHOD` | error | [05](05-types.md) | Methods or associated functions of two or more traits match at the same step |
| `ARENA_REQUIRES_COPY` | error | [07](07-ownership-and-borrowing.md) | A non-Copy value is allocated in an arena. It is reported by the type checker (§7.15.2, D-OWN-07) |
| `ARRAY_LENGTH_MISMATCH` | error | [06](06-expressions-and-statements.md) | An array literal's length differs from the expected `Array<T, N>` |
| `ASSIGN_TO_IMMUTABLE` | error | [07](07-ownership-and-borrowing.md) | Assignment to a place that is not mutable (§7.2.4) |
| `ASSIGN_WHILE_BORROWED` | error | [07](07-ownership-and-borrowing.md) | A place is assigned (or its old value dropped) while it is borrowed (§7.9.6) |
| `ASSOC_TYPE_BINDING_OUTSIDE_BOUND` | error | [05](05-types.md) | `A = T` in a trait reference that is not a bound |
| `ASSOC_TYPE_IN_INHERENT_IMPL` | error | [05](05-types.md) | An associated type in an inherent impl |
| `ASYNC_EXPORT_UNSUPPORTED` | error | [11](11-async-and-concurrency.md) | An `async fn` exported as a core export, or through an interface on a target without the Component Model async ABI (§11.10.4) |
| `ASYNC_FN_IN_TRAIT` | error | [11](11-async-and-concurrency.md) | An `async fn` declared in a trait or implemented in a trait impl (§11.2.6) |
| `ASYNC_MAIN` | error | [11](11-async-and-concurrency.md) | The entry point `main` is declared `async` (§11.2.6) |
| `ASYNC_RECURSION` | error | [11](11-async-and-concurrency.md) | The future type of an async function contains itself without a `Box` (§11.2.4) |
| `AT_PATTERN_BINDING_CONFLICT` | error | [06](06-expressions-and-statements.md) | `x @ P` combines binding modes that conflict (§6.21.4) |
| `AWAIT_OUTSIDE_ASYNC` | error | [11](11-async-and-concurrency.md) | `.await` or `for await` where the innermost body is not an `async fn` or `async` block (§11.3.1) |
| `BASE_ARG_NOT_LAST` | error | [03](03-grammar.md) | A `..base` argument that is not the last argument (§3.13.2) |
| `BLOCK_IN_CONDITION` | error | [03](03-grammar.md) | An expression in condition position starts with `{` (§3.13.5) |
| `BORROWS_CONTRACT_VIOLATED` | error | [07](07-ownership-and-borrowing.md) | The returned value borrows from a source that the elision rules or the `borrows` clause do not name (§7.7.4, §7.9.7) |
| `BORROWS_INVALID_PROVENANCE` | error | [07](07-ownership-and-borrowing.md) | A `borrows` entry does not name a parameter, a valid accessible field path, or a source that carries a region (§7.7.3) |
| `BORROWS_WITHOUT_OUTPUT_REGION` | error | [07](07-ownership-and-borrowing.md) | A `borrows` clause on a function whose return type has no region (§7.7.3) |
| `BORROW_ESCAPES_ARENA` | error | [07](07-ownership-and-borrowing.md) | A value that borrows an arena is live after its `region` block ends (§7.15) |
| `BORROW_ESCAPES_SIGNATURE` | error | [07](07-ownership-and-borrowing.md) | The body needs an outlives relation between input regions, or a type-parameter bound, that the signature does not imply (§7.9.7, §7.6.5) |
| `BORROW_MUT_OF_IMMUTABLE` | error | [07](07-ownership-and-borrowing.md) | `borrow mut` (explicit, receiver, capture, or closure call) of a place that is not mutable (§7.2.4, §7.5.1, D-OWN-02) |
| `BOUND_EFFECTS_EXCEEDED` | error | [08](08-effects-and-capabilities.md) | Latent effects of an instantiation exceed a bound's cap (§8.8.3) |
| `BREAK_OUTSIDE_LOOP` | error | [06](06-expressions-and-statements.md) | `break` with no enclosing loop in the current body |
| `CANNOT_DEREFERENCE` | error | [06](06-expressions-and-statements.md) | `*e` on a type that is not `borrow T`, `borrow mut T`, `Box<T>`, `Ptr<T>` or `PtrMut<T>`; a raw-pointer dereference outside `unsafe` is `UNSAFE_OPERATION_OUTSIDE_UNSAFE` ([12](12-unsafe-and-ffi.md)) |
| `CAPABILITY_ALLOW_DENY_CONFLICT` | error | [08](08-effects-and-capabilities.md) | The same capability name is in both `allow` and `deny` (§8.12.5) |
| `CAPABILITY_DENIED` | error | [08](08-effects-and-capabilities.md) | A required capability overlaps the checked policy's `deny` (§8.12.6) |
| `CAPABILITY_NOT_ALLOWED` | error | [08](08-effects-and-capabilities.md) | A required capability is neither covered nor implied by the checked policy's `allow` (§8.12.5, §8.12.6) |
| `CHAINED_COMPARISON` | error | [03](03-grammar.md) | Chained comparison operators (§3.13.1) |
| `CHAINED_RANGE` | error | [03](03-grammar.md) | Chained range operators, such as `a..b..c` (§3.13.1) |
| `CHILD_TASK_BORROWS_LOCAL` | error | [11](11-async-and-concurrency.md) | A future passed to `Scope.spawn`, or the body of `scope`, borrows something that does not outlive the scope (§11.7.3) |
| `CLAUSE_ORDER` | error | [03](03-grammar.md) | Function signature clauses out of order (§3.5) |
| `COMPONENT_TARGET_MISMATCH` | error | [13](13-components-and-wit.md) | A component build for a core target of a package that defines `main` (§13.6.2), or `sai component build` for a native target (§13.6.1) |
| `CONFLICTING_ATTRIBUTES` | error | [15](15-attributes-and-lints.md) | `@inline` with `@cold`, or `@flags` with `@derive`, on one target (§15.2.4 step 7) |
| `CONFLICTING_BORROW` | error | [07](07-ownership-and-borrowing.md) | A borrow (including a receiver, a two-phase activation, or a capture) conflicts with a live loan (§7.9.6) |
| `CONFLICTING_WASM_IMPORT` | error | [12](12-unsafe-and-ffi.md) | Two imports with the same module and field name and different lowered signatures, as computed by [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.10.3 (§12.7.6) |
| `CONST_ARG_NOT_SIMPLE` | error | [05](05-types.md) | A const argument that is not a literal or a const parameter (D-TYPE-07) |
| `CONST_CYCLE` | error | [06](06-expressions-and-statements.md) | A const depends on itself |
| `CONST_EVAL_PANIC` | error | [06](06-expressions-and-statements.md) | Evaluating a const initializer would panic (overflow, division by zero, shift overflow) |
| `CONST_INVARIANT_NOT_PROVEN` | error | [09](09-contracts-and-refinements.md) | A construction of a type with an invariant in a const initializer that the SFC does not prove (§9.4.5) |
| `CONST_PATTERN_TYPE` | error | [06](06-expressions-and-statements.md) | A const used as a pattern has a type other than an integer, `Bool`, `Char`, `Byte` or `Str` |
| `CONTINUE_OUTSIDE_LOOP` | error | [06](06-expressions-and-statements.md) | `continue` with no enclosing loop in the current body |
| `CONTRACT_REQUIRES_ON_IMPL` | error | [09](09-contracts-and-refinements.md) | A `requires` clause on a trait impl method (§9.5.1) |
| `CONTRACT_RUNTIME_CHECK` | info (lint) | [09](09-contracts-and-refinements.md) | An obligation that is not proven and is checked at runtime; named by D-CONTRACT-01 (§9.7.1) |
| `COPY_AND_DROP` | error | [07](07-ownership-and-borrowing.md) | A type is both Copy and `Drop` (§7.13.6) |
| `DEPRECATED_REFERENCE` | warning (lint) | [15](15-attributes-and-lints.md) | A reference to a deprecated declaration outside a deprecated context (§15.6.4) |
| `DERIVE_FIELD_MISSING_IMPL` | error | [05](05-types.md) | A field, payload or base type that does not implement the derived trait |
| `DERIVE_NOT_SUPPORTED` | error | [05](05-types.md) | A derivable trait that cannot be derived for this kind of declaration (§5.12.4) |
| `DISCRIMINANT_OUT_OF_RANGE` | error | [05](05-types.md) | An enum discriminant outside the discriminant type |
| `DOES_NOT_LIVE_LONG_ENOUGH` | error | [07](07-ownership-and-borrowing.md) | A local is dropped or its storage ends while a loan of it is live (§7.9.6) |
| `DROP_HAS_EFFECTS` | error | [08](08-effects-and-capabilities.md) | A `Drop.drop` impl has a non-empty declared or inferred set, or makes a call that leaves effects latent in its type parameters (§8.6.3) |
| `DROP_IMPL_INVALID_TYPE` | error | [05](05-types.md) | `impl Drop` for a type that is not a local record, enum, distinct type or resource |
| `DROP_IMPL_NOT_GENERIC_ENOUGH` | error | [05](05-types.md) | A `Drop` impl that is not exactly as generic as its type (N-A-34) |
| `DUPLICATE_ATTRIBUTE` | error | [15](15-attributes-and-lints.md) | The same attribute twice on one target, other than the level attributes (§15.2.4 step 5) |
| `DUPLICATE_ATTRIBUTE_ARGUMENT` | error | [15](15-attributes-and-lints.md) | The same derive name, the same lint name at the same level, or the same `@target` operand twice (§15.2.4 step 6, §15.15.3) |
| `DUPLICATE_BINDING` | error | [04](04-modules-and-names.md) | Two bindings of one name in one scope: module scope, block, parameter list, generic parameter list, pattern, interface (§4.4.3, D-MOD-03, N-A-8) |
| `DUPLICATE_CLAUSE` | error | [03](03-grammar.md) | A second `where`, `borrows`, or `effects` clause on one function (§3.5) |
| `DUPLICATE_DISCRIMINANT` | error | [05](05-types.md) | Two variants with the same discriminant |
| `DUPLICATE_EFFECT` | warning (lint) | [08](08-effects-and-capabilities.md) | The same atom appears twice in one effect set (§8.2.3) |
| `DUPLICATE_EXPORT_NAME` | error | [15](15-attributes-and-lints.md) | Two `export fn` items of the root package of a core-module build with the same export name, or an export name `memory` (§15.7) |
| `DUPLICATE_FIELD` | error | [05](05-types.md) | Two fields with the same name in a record or record variant |
| `DUPLICATE_INHERENT_ITEM` | error | [05](05-types.md) | Two inherent items with the same name across the inherent impls of one type |
| `DUPLICATE_INTERFACE_IMPL` | error | [13](13-components-and-wit.md) | A second `impl` of the same interface in one package (§13.3.1) |
| `DUPLICATE_INVARIANT` | error | [09](09-contracts-and-refinements.md) | A second `invariant` declaration for the same type (§9.4.1) |
| `DUPLICATE_PANIC_HOOK` | error | [10](10-errors-and-panics.md) | A second `@panic_hook` declaration in one build, reported at every declaration after the first in file-path and source order (§10.6.1) |
| `DUPLICATE_TEST_NAME` | error | [14](14-testing.md) | Two tests or properties of one module with the same name after `@target` filtering, reported at the later one with a note at the first (§14.2.3) |
| `DUPLICATE_TRAIT_ITEM` | error | [05](05-types.md) | Two items with the same name in one trait |
| `DUPLICATE_VARIANT` | error | [05](05-types.md) | Two variants with the same name |
| `EFFECTFUL_PREDICATE` | error | [08](08-effects-and-capabilities.md) | A refinement, contract or invariant predicate has effects (§8.11) |
| `EFFECTS_ON_INTERFACE_FN` | error | [08](08-effects-and-capabilities.md) | A function of an `interface` declaration has an `effects` clause (§8.6.1) |
| `EFFECT_COVERS_CYCLE` | error | [08](08-effects-and-capabilities.md) | The `covers` relation of user effects has a cycle (§8.4.1) |
| `EFFECT_NAME_RESERVED` | error | [08](08-effects-and-capabilities.md) | A user effect is named after a built-in root (§8.4.1) |
| `EFFECT_NOT_DECLARED` | error | [08](08-effects-and-capabilities.md) | A function performs an effect its clause does not permit (§8.9.1) |
| `EFFECT_NOT_VISIBLE` | error | [08](08-effects-and-capabilities.md) | A user effect is named where it is not visible (§8.2.2, §8.4.2) |
| `EMPTY_RANGE_PATTERN` | error | [06](06-expressions-and-statements.md) | A range pattern that matches no value |
| `ENSURES_USES_MOVED_PARAMETER` | error | [09](09-contracts-and-refinements.md) | An `ensures` clause mentions a by-value parameter that may be moved at a return point (§9.3.4) |
| `EXPECTED_EXPRESSION` | error | [03](03-grammar.md) | An expression is required but missing |
| `EXPECTED_PATTERN` | error | [06](06-expressions-and-statements.md) | A path in a pattern that is not a constructor, variant or const |
| `EXPECTED_TOKEN` | error | [03](03-grammar.md) | An expected token is missing (the data includes the set of expected tokens) |
| `EXPECTED_VALUE_FOUND_TYPE` | error | [06](06-expressions-and-statements.md) | A type, module or trait used as a value |
| `EXPLICIT_DISCRIMINANT_ON_DATA_ENUM` | error | [03](03-grammar.md) | Explicit discriminant on an enum with payloads (§3.6) |
| `EXPLICIT_DROP_CALL` | error | [07](07-ownership-and-borrowing.md) | `Drop.drop` is called explicitly (§7.13.6) |
| `EXPORT_FN_NON_SCALAR` | error | [12](12-unsafe-and-ffi.md) | A parameter or result type of an `extern "wasm"` function, or of an `export fn` exported from a core module, that is not a boundary scalar (§12.7.4, §12.8) |
| `EXPORT_FN_NOT_APPLIED` | info | [13](13-components-and-wit.md) | A component build does not export the root package's `export fn` items (§13.6.4) |
| `EXPORT_WORLD_NOT_APPLIED` | info | [13](13-components-and-wit.md) | `sai build` for a core or native target ignores the `export world` (§13.6.1, §13.6.2) |
| `EXTERN_EFFECTS_MISMATCH` | error | [08](08-effects-and-capabilities.md) | An `extern "wasm"` function is given an effect set other than `{ host }`, or an `extern "C"` function one other than `{ ffi }` (§8.6.1) |
| `EXTERN_ENSURES_REQUIRES_UNSAFE` | error | [12](12-unsafe-and-ffi.md) | An `extern "wasm"` function with an `ensures` clause in a block without `unsafe` (§12.7.3) |
| `FEATURE_NOT_IMPLEMENTED` | error | [01](01-introduction.md) | A staged feature that this compiler does not implement is used; data: `stage`, `feature` (README §4; formally defined in §1.4.5) |
| `FLAGS_PATTERN` | error | [13](13-components-and-wit.md) | A variant pattern on a flags type (§13.9.4) |
| `FLOAT_PATTERN` | error | [03](03-grammar.md) | A floating-point literal in a pattern (§3.14) |
| `FN_POINTER_EFFECTS_MISMATCH` | error | [08](08-effects-and-capabilities.md) | A function's effects, including the latent effects of its type arguments, are not permitted by the target function pointer type (§8.6.4) |
| `FN_POINTER_REGION_MISMATCH` | error | [07](07-ownership-and-borrowing.md) | A function item is coerced to a function pointer type whose elided regions differ from the item's (§7.7.6) |
| `FORBIDDEN_CHARACTER` | error | [02](02-lexical-structure.md) | Forbidden control character, bidirectional control character, or invisible whitespace |
| `FORMAT_NOT_IMPLEMENTED` | error | [06](06-expressions-and-statements.md) | An f-string interpolation whose type does not implement `Format` |
| `FORMAT_SPEC_ARGUMENT` | error | [16](16-core-library.md) | A format specifier with a missing or unexpected argument, or with an argument outside `0..=65535` (§16.14.3) |
| `FORMAT_SPEC_NOT_APPLICABLE` | error | [16](16-core-library.md) | A known format specifier applied to a type that does not implement the marker trait it requires (`FormatInt`, `FormatFloat` or `FormatPad`); data: `spec`, `type`, `required_trait` (§16.14.3) |
| `FUNCTIONAL_UPDATE_OUTSIDE_RECORD` | error | [06](06-expressions-and-statements.md) | `..e` as an argument of a call that is not a record construction |
| `GENERIC_ARG_KIND_MISMATCH` | error | [05](05-types.md) | A type argument for a const parameter, or the reverse |
| `GENERIC_INTERFACE_ITEM` | error | [13](13-components-and-wit.md) | Generic parameters or a `where` clause on an interface item or interface impl function (§13.2.2, §13.3.1) |
| `GENERIC_REFINED_TYPE` | error | [05](05-types.md) | A refined type, or a distinct type with a refinement clause, with generic parameters (03 §3.6) |
| `GUARD_MOVE_OR_MUTATION` | error | [06](06-expressions-and-statements.md) | A match guard moves, assigns or mutably borrows a binding or the scrutinee |
| `IDENT_NOT_NFC` | error | [02](02-lexical-structure.md) | Identifier not in NFC |
| `IF_WITHOUT_ELSE` | error | [06](06-expressions-and-statements.md) | An `if` without `else` used where a non-`()` value is required |
| `IMPLEMENTATION_LIMIT_EXCEEDED` | error | [01](01-introduction.md) | The program exceeds an implementation limit of this compiler; data: `limit`, `value` (§1.5.4) |
| `IMPLICIT_MEMBER_NEEDS_TYPE` | error | [06](06-expressions-and-statements.md) | `.V` without a known expected type (named in [03](03-grammar.md) §3.13.4) |
| `IMPLICIT_MEMBER_NOT_ENUM` | error | [06](06-expressions-and-statements.md) | `.V` whose expected type is not an enum |
| `IMPL_SIGNATURE_REGION_MISMATCH` | error | [07](07-ownership-and-borrowing.md) | An impl method's elided signature differs from the trait method's in its regions (§7.7.5, N-B-13) |
| `IMPL_TARGET_NOT_INTERFACE` | error | [13](13-components-and-wit.md) | The path of an interface impl does not resolve to an interface (§13.3.1) |
| `IMPL_UNSAFETY_MISMATCH` | error | [12](12-unsafe-and-ffi.md) | An impl of an unsafe trait without `unsafe`, or `unsafe impl` of a safe trait, an inherent impl or an interface impl (§12.2.3) |
| `INACTIVE_DEPENDENCY` | error | [04](04-modules-and-names.md) | A `use` of a dependency whose `only` list does not contain the build target (§4.3.2 step 1.3, [package/01](../package/01-package-contract.md) §1.10.5 rule 4) |
| `INCONSISTENT_INDENT` | error | [02](02-lexical-structure.md) | Multiline string line does not match the base indentation |
| `INFER_IN_SIGNATURE` | error | [03](03-grammar.md) | `_` type in a signature (§3.15) |
| `INFINITE_SIZE_TYPE` | error | [05](05-types.md) | A nominal type that contains itself by value (§5.6.7) |
| `INHERENT_IMPL_FOREIGN_TYPE` | error | [05](05-types.md) | An inherent impl for a type not declared as a nominal type in the current package, other than the core-library exception of §5.9.2 rule 1 (N-A-33) |
| `INSTANTIATION_DEPTH_EXCEEDED` | error | [05](05-types.md) | Monomorphization nests a type argument deeper than 64 (N-A-30) |
| `INTERFACE_FN_BODY` | error | [13](13-components-and-wit.md) | An interface function, interface resource member or world function has a body; reported by the parser instead of `UNEXPECTED_FN_BODY`, which is only for `extern` blocks (§13.2.2, §13.4.1) |
| `INTERFACE_FN_CLAUSE` | error | [13](13-components-and-wit.md) | `borrows`, `requires`, `ensures` or `unsafe` on an interface function or resource member (§13.2.2) |
| `INTERFACE_IMPL_EXTRA_ITEM` | error | [13](13-components-and-wit.md) | An interface impl contains an item the interface does not declare (§13.3.1) |
| `INTERFACE_IMPL_MISSING_ITEM` | error | [13](13-components-and-wit.md) | An interface impl or bound resource lacks a function, member or resource binding (§13.3.1, §13.3.3) |
| `INTERFACE_IMPL_SIGNATURE_MISMATCH` | error | [13](13-components-and-wit.md) | An implementing function does not conform to the declared signature (§13.3.2, §13.4.1) |
| `INTERFACE_USE_CYCLE` | error | [13](13-components-and-wit.md) | Local interfaces use each other's types cyclically (§13.2.3) |
| `INTERNAL_COMPILER_ERROR` | error | [01](01-introduction.md) | Internal failure of the compiler, never caused by the program (§1.4.2 item 8). [impl/11](../impl/11-diagnostics.md) §11.4.6 and [impl/01](../impl/01-architecture.md) §1.10 describe only how it is reported |
| `INVALID_ASSIGN_TARGET` | error | [03](03-grammar.md) | The left-hand side of an assignment is not a place expression (§3.12) |
| `INVALID_ATTRIBUTE_ARGUMENT` | error | [15](15-attributes-and-lints.md) | An argument list that does not match the attribute's grammar or value range, an `instead` path that does not resolve to a valid replacement (§15.6.3), or a `@target` operand that is not a target name, a capability or a combinator (§15.11.1) |
| `INVALID_ATTRIBUTE_TARGET` | error | [15](15-attributes-and-lints.md) | An attribute on a declaration kind or position it does not allow (§15.2.1, §15.2.4 step 3, and each attribute's section) |
| `INVALID_BREAK_VALUE` | error | [06](06-expressions-and-statements.md) | `break` with a value targeting `while`, `while let` or `for` |
| `INVALID_CHAR_LITERAL` | error | [02](02-lexical-structure.md) | Empty character literal, raw TAB, or more than one scalar value between the quotes |
| `INVALID_CONST_PARAM_TYPE` | error | [05](05-types.md) | A const parameter whose type is not an integer type, `Bool` or `Char` |
| `INVALID_CONTRACT_EXPRESSION` | error | [09](09-contracts-and-refinements.md) | A predicate contains `return`, `break`, `continue`, `?`, `.await`, an assignment, `var`, `unsafe`, `region` or a closure (§9.1.2) |
| `INVALID_ESCAPE` | error | [02](02-lexical-structure.md) | Invalid escape sequence |
| `INVALID_EXPORT_TARGET` | error | [03](03-grammar.md) | `export` on something other than a function (§3.3) |
| `INVALID_EXTERN_BLOCK` | error | [12](12-unsafe-and-ffi.md) | An `extern "wasm"` block without `module`, or an `extern "C"` block without `unsafe` (§12.7.1, §12.9.1) |
| `INVALID_EXTERN_SIGNATURE` | error | [12](12-unsafe-and-ffi.md) | An extern function or an exported `export fn` that is `async` (extern only), generic, has a `self` parameter, or has a `where` or `borrows` clause; a type of an `extern "C"` function that is not an FFI type (§12.7.4, §12.8, §12.9.3) |
| `INVALID_FLAGS_ENUM` | error | [13](13-components-and-wit.md) | `@flags` on an enum with payloads, discriminants, more than 32 variants, or outside an interface (§13.9.4) |
| `INVALID_FORMAT_SPEC` | error | [02](02-lexical-structure.md) | Format specifier that does not match `FormatSpec` |
| `INVALID_INTERPOLATION` | error | [02](02-lexical-structure.md) | Line break, string literal, or empty interpolation, or unmatched `}`, in a format string |
| `INVALID_LINE_ENDING` | error | [02](02-lexical-structure.md) | Lone CR |
| `INVALID_LINK_NAME` | error | [12](12-unsafe-and-ffi.md) | The symbol name of an `extern "C"` function is not a C identifier (§12.9.1) |
| `INVALID_MODULE_NAME` | error | [04](04-modules-and-names.md) | A directory or file name in `<source>` is not a valid module segment, or is `lib` outside the root file |
| `INVALID_RAW_IDENT` | error | [02](02-lexical-structure.md) | `r#_` |
| `INVALID_RESOURCE_BINDING` | error | [13](13-components-and-wit.md) | `type R = C` where `C` is not a non-generic local resource, or `C` is bound twice (§13.3.3) |
| `INVALID_SELF_TYPE` | error | [03](03-grammar.md) | Invalid type for a self parameter (§3.5) |
| `INVALID_TEST_NAME` | error | [14](14-testing.md) | An empty test name, or one that contains a character of the general category `Cc` (§14.2.3) |
| `INVALID_TRAIT_WHERE_SUBJECT` | error | [05](05-types.md) | A `where` predicate on a trait whose subject is not `Self` or `Self.A` (N-A-28) |
| `INVALID_TUPLE_INDEX` | error | [06](06-expressions-and-statements.md) | A tuple index written with `_` or leading zeros |
| `INVALID_USE_TARGET` | error | [04](04-modules-and-names.md) | A WIT `PKG_PATH` names a package or a world instead of an interface |
| `INVALID_UTF8` | error | [02](02-lexical-structure.md) | Ill-formed UTF-8 |
| `INVALID_WORLD_ITEM` | error | [13](13-components-and-wit.md) | A world item names a world where an interface is required, or vice versa (§13.4.1) |
| `INVARIANT_FIELD_WRITE` | error | [09](09-contracts-and-refinements.md) | A field of a type with an invariant is written or mutably borrowed outside its defining module (§9.4.2) |
| `INVARIANT_ON_GENERIC_TYPE` | error | [09](09-contracts-and-refinements.md) | An `invariant` on a type with generic parameters (§9.4.1) |
| `INVARIANT_ON_INVALID_TYPE` | error | [09](09-contracts-and-refinements.md) | An `invariant` on a refined type, alias, primitive or interface type (§9.4.1) |
| `INVARIANT_OUTSIDE_DEFINING_MODULE` | error | [03](03-grammar.md) | Invariant placed outside the module that defines the type (§3.8) |
| `IRREFUTABLE_CONDITION_PATTERN` | warning (lint) | [06](06-expressions-and-statements.md) | `if let` or `while let` with an irrefutable pattern |
| `ITEM_IN_BLOCK` | error | [06](06-expressions-and-statements.md) | An item declaration inside a block |
| `LABEL_SHADOWED` | error | [06](06-expressions-and-statements.md) | A loop label that repeats an enclosing label in the same body |
| `LET_ELSE_IRREFUTABLE` | error | [06](06-expressions-and-statements.md) | `let`-`else` with an irrefutable pattern |
| `LET_ELSE_NOT_DIVERGING` | error | [06](06-expressions-and-statements.md) | The `else` block of `let`-`else` does not have type `Never` |
| `LITERAL_OUT_OF_RANGE` | error | [02](02-lexical-structure.md) | Literal out of range for its type |
| `MALFORMED_MULTILINE_STRING` | error | [02](02-lexical-structure.md) | Opening `"""` not followed by a line break |
| `MALFORMED_NUMBER` | error | [02](02-lexical-structure.md) | Malformed numeric literal |
| `MALFORMED_PACKAGE_PATH` | error | [02](02-lexical-structure.md) | `PKG_PATH` segment that violates §2.9 |
| `MALFORMED_RAW_STRING` | error | [02](02-lexical-structure.md) | Raw string with more than 255 `#` |
| `MANUAL_COPY_IMPL` | error | [05](05-types.md) | A hand-written `impl Copy` (N-A-39) |
| `MANUAL_FN_IMPL` | error | [05](05-types.md) | A hand-written impl of `Fn`, `FnMut` or `FnOnce` (N-A-31) |
| `MATCH_ON_BORROW` | error | [06](06-expressions-and-statements.md) | A non-trivial pattern on a scrutinee of type `borrow T` / `borrow mut T` (fix: `match *e`) |
| `METHOD_NOT_FOUND` | error | [05](05-types.md) | No method or associated function of that name for the type (§5.13.5) |
| `MISPLACED_DOC_COMMENT` | warning | [02](02-lexical-structure.md) | Doc comment in an invalid position |
| `MISSING_BORROW` | error | [07](07-ownership-and-borrowing.md) | An argument is passed without the `borrow` / `borrow mut` its parameter type requires. It is reported by the type checker (§7.5.2, D-OWN-02) |
| `MISSING_DOCS` | warning (lint) | [15](15-attributes-and-lints.md) | A declaration visible outside its package without a doc comment (§15.18.7) |
| `MISSING_EFFECTS_DECLARATION` | error | [08](08-effects-and-capabilities.md) | A function that must declare its effects omits a non-empty clause (§8.9.3); named by [decisions.md](../design/decisions.md) D-EFF-01 |
| `MISSING_FIELD` | error | [06](06-expressions-and-statements.md) | A record construction or record pattern misses fields |
| `MISSING_FN_BODY` | error | [03](03-grammar.md) | A function that needs a body has none (§3.5) |
| `MISSING_INITIALIZER` | error | [03](03-grammar.md) | `let` or `var` without `=` (§3.12) |
| `MISSING_ROOT_MODULE` | error | [04](04-modules-and-names.md) | `<source>` or `<source>/lib.sai` does not exist |
| `MISSING_SAFETY_DOC` | warning (lint) | [12](12-unsafe-and-ffi.md) | A `pub` unsafe function, method, trait method or trait without a `# Safety` doc section (§12.2.2, §12.2.3) |
| `MISSING_SUPERTRAIT_IMPL` | error | [05](05-types.md) | An impl (or derive) of a trait whose supertrait is not implemented for the type |
| `MISSING_TRAIT_ITEM` | error | [05](05-types.md) | A trait impl that lacks a required item |
| `MODULE_INTERFACE_CONFLICT` | error | [04](04-modules-and-names.md) | An interface and a module file have the same path |
| `MOVE_OUT_OF_BORROW` | error | [07](07-ownership-and-borrowing.md) | A non-Copy value would be moved out from behind a borrow or a raw pointer (§7.3.5, D-OWN-01, D-EXPR-04) |
| `MOVE_OUT_OF_BOX` | error | [07](07-ownership-and-borrowing.md) | A non-Copy value would be moved out of `*b` for a `Box` (§7.3.5, N-B-2) |
| `MOVE_OUT_OF_CAPTURE` | error | [07](07-ownership-and-borrowing.md) | A closure required to be `Fn` or `FnMut` moves out of a captured variable (§7.11.3) |
| `MOVE_OUT_OF_DROP_TYPE` | error | [07](07-ownership-and-borrowing.md) | A field would be moved out of a value whose type implements `Drop`, or out of a resource (§7.3.5, §7.14) |
| `MOVE_OUT_OF_INDEX` | error | [07](07-ownership-and-borrowing.md) | A non-Copy value would be moved out of a built-in index place `a[i]` (§7.3.5, N-B-4) |
| `MOVE_WHILE_BORROWED` | error | [07](07-ownership-and-borrowing.md) | A place is moved while it is borrowed (§7.9.6) |
| `MULTIPLE_REST_PATTERNS` | error | [03](03-grammar.md) | More than one `..` in a slice pattern (§3.14) |
| `NAMED_ARGUMENT_ON_FUNCTION` | error | [03](03-grammar.md) | Named argument in an ordinary function call (§3.13.2) |
| `NAMING_CONVENTION` | warning (lint) | [15](15-attributes-and-lints.md) | A declared name that does not follow the case convention of its kind (§15.18.5) |
| `NESTED_OLD` | error | [09](09-contracts-and-refinements.md) | `old(..)` inside another `old(..)` (§9.3.2) |
| `NON_ASCII_IDENTIFIER` | warning (lint) | [02](02-lexical-structure.md) | Non-ASCII identifier |
| `NON_ASCII_PUBLIC_NAME` | error | [02](02-lexical-structure.md) | Non-ASCII character in a public name or module name |
| `NON_EXHAUSTIVE_MATCH` | error | [06](06-expressions-and-statements.md) | A `match` that does not cover every value; lists witnesses |
| `NOT_A_LINT` | error | [15](15-attributes-and-lints.md) | A level attribute, a `lints` entry or a CLI lint flag names the code of a hard diagnostic (§15.15.4) |
| `NOT_A_TRAIT` | error | [05](05-types.md) | A bound or impl names something that is not a trait |
| `NOT_CALLABLE` | error | [06](06-expressions-and-statements.md) | A call of a value whose type is not callable |
| `NOT_CONST_EXPRESSION` | error | [06](06-expressions-and-statements.md) | A form not allowed in a const initializer |
| `NOT_INDEXABLE` | error | [06](06-expressions-and-statements.md) | `e[i]` with no built-in indexing and no `Index` / `IndexMut` impl |
| `NOT_ITERABLE` | error | [06](06-expressions-and-statements.md) | The `for` iterable does not implement `IntoIterator` |
| `NO_ASYNC_CLOSURE` | error | [11](11-async-and-concurrency.md) | `async` before a closure expression (§11.2.5) |
| `NO_AS_CAST` | error | [06](06-expressions-and-statements.md) | `as` used as a conversion operator |
| `NO_BANG_OPERATOR` | error | [02](02-lexical-structure.md) | Use of a lone `!` |
| `NO_COMPONENT_EXPORTS` | error | [13](13-components-and-wit.md) | A component build of a reactor with nothing to export (§13.5.3) |
| `NO_DOUBLE_COLON` | error | [02](02-lexical-structure.md) | Use of `::` |
| `NO_EXCEPTIONS` | error | [02](02-lexical-structure.md) | Use of `try` / `catch` / `throw` |
| `NO_NULL` | error | [02](02-lexical-structure.md) | Use of `null` / `nil` / `undefined` |
| `NO_SEMICOLON` | error | [02](02-lexical-structure.md) | Use of `;` |
| `OLD_OUTSIDE_ENSURES` | error | [09](09-contracts-and-refinements.md) | `old(..)` outside an `ensures` clause that does not resolve to a function (§9.3.2) |
| `OLD_REFERS_TO_RESULT` | error | [09](09-contracts-and-refinements.md) | The operand of `old(..)` mentions `result` (§9.3.2) |
| `OLD_REQUIRES_COPY` | error | [09](09-contracts-and-refinements.md) | The operand of `old(..)` has a type that is not `Copy` (§9.3.2) |
| `OPAQUE_RESOURCE_CONSTRUCTION` | error | [13](13-components-and-wit.md) | Record-construction syntax used with an imported resource type (§13.7.4) |
| `OPERATOR_NOT_SUPPORTED` | error | [06](06-expressions-and-statements.md) | An operator not defined for the operand types |
| `ORPHAN_IMPL` | error | [05](05-types.md) | A trait impl that violates the orphan rule (§5.10.1) |
| `OR_PATTERN_BINDING_MISMATCH` | error | [03](03-grammar.md) | Or-pattern alternatives bind different names, or one name with different types or binding modes (§3.14) |
| `OVERLAPPING_IMPLS` | error | [05](05-types.md) | Two impls, derived impls or compiler-provided impls whose headers unify (§5.10.2) |
| `PANIC_HOOK_OUTSIDE_ROOT` | error | [10](10-errors-and-panics.md) | A `@panic_hook` declared in a package that is not the root package of the build (§10.6.1) |
| `PANIC_HOOK_SIGNATURE_MISMATCH` | error | [10](10-errors-and-panics.md) | A `@panic_hook` function that does not have exactly two `USize` parameters and result `()`, is an `unsafe fn`, or has a `requires` or `ensures` clause (§10.6.1) |
| `PARTIAL_ASSIGN_TO_MOVED` | error | [07](07-ownership-and-borrowing.md) | A field of a value that is not definitely initialized is assigned (§7.3.6, N-B-3) |
| `PATTERN_ARITY_MISMATCH` | error | [06](06-expressions-and-statements.md) | Wrong number of subpatterns for a tuple, tuple variant or unit variant |
| `PRIVATE_EFFECT_IN_PUBLIC_SIGNATURE` | error | [08](08-effects-and-capabilities.md) | A public signature names a non-`pub` user effect (§8.4.2) |
| `PRIVATE_FIELD` | error | [04](04-modules-and-names.md) | A field (or `.value` of an opaque distinct type) is not visible here, or construction/pattern rules of §4.6.3 are violated |
| `PRIVATE_ITEM` | error | [04](04-modules-and-names.md) | An item, member, distinct constructor or pattern is not visible here |
| `PRIVATE_REEXPORT` | error | [04](04-modules-and-names.md) | A `pub use` lists an item that is not `pub` |
| `PRIVATE_TYPE_IN_PUBLIC_API` | error | [04](04-modules-and-names.md) | A public item's surface names a package-private type, trait or constant |
| `PROJECTION_IN_IMPL_HEADER` | error | [05](05-types.md) | A projection in the self type or trait arguments of an impl header |
| `PROJECTION_ON_CONCRETE_TYPE` | error | [05](05-types.md) | `τ.A` with a concrete `τ` (N-A-29) |
| `PROVIDED_INTERFACE_AT_BOUNDARY` | error | [13](13-components-and-wit.md) | A provided interface is exported by the export world, has a type in a boundary type of an export or of a remaining import, or, when provided within the build, is imported by a composed artifact (§13.5.2) |
| `PUBLIC_EFFECTS_INFERRED` | warning (lint) | [08](08-effects-and-capabilities.md) | A `pub` function of a pre-1.0 package has a non-empty inferred set (§8.9.3); named by [decisions.md](../design/decisions.md) D-EFF-01 |
| `PUB_REPR_ON_PRIVATE_DISTINCT` | error | [04](04-modules-and-names.md) | `distinct D = pub B` where `D` is not `pub` |
| `PUB_USE_WITHOUT_ITEMS` | error | [04](04-modules-and-names.md) | A `pub use` has no item list |
| `RECORD_CONSTRUCTOR_AS_VALUE` | error | [06](06-expressions-and-statements.md) | A record type or record variant used as a function value |
| `RECORD_FIELD_NEEDS_NAME` | error | [06](06-expressions-and-statements.md) | An unnamed, non-punning argument or subpattern in a record construction or pattern |
| `REDUNDANT_BINDING_MODE` | warning (lint) | [06](06-expressions-and-statements.md) | `borrow mut r` rest binding on a `SliceMut` |
| `REDUNDANT_IMPORT` | warning (lint) | [04](04-modules-and-names.md) | A `use` binding denotes what its name already denotes (default level in [15](15-attributes-and-lints.md) §15.17.2) |
| `REDUNDANT_PROVENANCE` | warning (lint) | [07](07-ownership-and-borrowing.md) | A `borrows` entry duplicates another, or is subsumed by a prefix (§7.7.3) |
| `REDUNDANT_PUB_EXPORT` | error | [03](03-grammar.md) | `pub export` (§3.3) |
| `REDUNDANT_UPDATE_BASE` | warning (lint) | [06](06-expressions-and-statements.md) | `..base` in a record construction that names every field |
| `REEXPORT_CYCLE` | error | [04](04-modules-and-names.md) | A chain of `pub use` re-exports returns to itself (D-MOD-04) |
| `REFINED_BASE_IS_REFINED` | error | [05](05-types.md) | The base of a refined type is itself refined (N-A-25) |
| `REFINED_PLACE_MUTATION` | error | [09](09-contracts-and-refinements.md) | A base method with a `borrow mut Self` receiver selected on a refined place (§9.2.2) |
| `REFINEMENT_NOT_PROVEN` | error | [09](09-contracts-and-refinements.md) | A B→R conversion, or a refined distinct construction `D(e)`, that the SFC does not prove (§9.2.3) |
| `REFUTABLE_PATTERN_IN_FOR` | error | [06](06-expressions-and-statements.md) | A refutable pattern in `for` |
| `REFUTABLE_PATTERN_IN_LET` | error | [06](06-expressions-and-statements.md) | A refutable pattern in `let` without `else`; lists witnesses |
| `RESERVED_TYPE_NAME` | error | [04](04-modules-and-names.md) | A declaration or import alias uses a primitive type name, `Int`, `UInt`, `Never`, `Send` or `Share` (§4.4.4) |
| `RESERVED_WORD` | error | [02](02-lexical-structure.md) | Use of a reserved word |
| `RESOURCE_CONTAINS_BORROW` | error | [07](07-ownership-and-borrowing.md) | A resource field type contains a borrow (§7.8.4, N-B-14) |
| `RESOURCE_WITH_BORROW` | error | [05](05-types.md) | A resource field whose type contains a view or borrow (N-A-27) |
| `RESULT_NAME_CONFLICT` | error | [09](09-contracts-and-refinements.md) | A parameter named `result` in a function with an `ensures` clause (§9.3.2) |
| `RESULT_OUTSIDE_ENSURES` | error | [09](09-contracts-and-refinements.md) | `result` outside an `ensures` clause that does not resolve to a binding (§9.3.2) |
| `RETURNS_LOCAL_BORROW` | error | [07](07-ownership-and-borrowing.md) | The returned value borrows a local, a temporary, or a by-value parameter (§7.7.4, §7.12.2) |
| `RETURN_OUTSIDE_FUNCTION` | error | [06](06-expressions-and-statements.md) | `return` in a const initializer; in a predicate the error is `INVALID_CONTRACT_EXPRESSION` (§6.16.5) |
| `SCOPE_REGION_ESCAPE` | error | [11](11-async-and-concurrency.md) | The output of a scope mentions the scope region, for example a `JoinHandle` or `borrow Scope` (§11.7.3) |
| `SEALED_TRAIT_IMPL` | error | [16](16-core-library.md) | An impl of a sealed trait (`Integer`, `Float`, `Step`, `SliceRange`, `FormatInt`, `FormatFloat`) outside `sai:core`; data: `trait` (§16.3.5) |
| `SELF_OUTSIDE_IMPL` | error | [05](05-types.md) | `Self` outside an impl or trait |
| `SHADOWS_GENERIC_PARAM` | error | [04](04-modules-and-names.md) | A method's generic parameter reuses a name of its impl's or trait's generic parameters |
| `SHADOWS_MODULE` | error | [04](04-modules-and-names.md) | A local has the name of a module or interface binding (D-MOD-01) |
| `SHADOWS_PRELUDE` | warning (lint) | [04](04-modules-and-names.md) | A module-level binding shadows a prelude name (default level in [15](15-attributes-and-lints.md) §15.17.2) |
| `SLICE_PATTERN_LENGTH_MISMATCH` | error | [06](06-expressions-and-statements.md) | A slice pattern that cannot match the length of an `Array<U, N>` |
| `STATEMENT_HAS_NO_EFFECT` | warning (lint) | [06](06-expressions-and-statements.md) | A discarded expression without side effects (§6.25.2) |
| `SUPERTRAIT_CYCLE` | error | [05](05-types.md) | A trait that is its own supertrait, directly or transitively |
| `SYMLINK_IN_SOURCE` | error | [04](04-modules-and-names.md) | A symbolic link inside `<source>` |
| `TARGET_CAPABILITY_UNAVAILABLE` | error | [08](08-effects-and-capabilities.md) | A required capability is not provided by the target (§8.12.6, §8.12.7) |
| `TEMPORARY_DROPPED_WHILE_BORROWED` | error | [07](07-ownership-and-borrowing.md) | A temporary is dropped at the end of its scope while a loan of it is live (§7.5.6, §7.9.6) |
| `TEST_DEPENDENCY_OUTSIDE_TEST` | error | [14](14-testing.md) | A reference to a binding of a test-only use outside a test or property body and property parameter types, or a `pub use` of a test dependency; checked in every build (§14.2.5) |
| `TEST_DEPENDENCY_UNAVAILABLE` | error | [14](14-testing.md) | A test-only use in a selected workspace member that is not the root, whose test dependencies are not resolved (§14.2.5 item 5) |
| `TEST_EFFECT_NOT_PERMITTED` | error | [14](14-testing.md) | A test or property whose entry set maps to a capability other than `alloc`, reported once per test and capability at the name, with the capability chain (§14.3.1) |
| `TRAIT_IMPL_EFFECTS_EXCEED_BOUND` | error | [08](08-effects-and-capabilities.md) | An impl method's effects are not permitted by the trait method's clause (§8.6.3) |
| `TRAIT_ITEM_SIGNATURE_MISMATCH` | error | [05](05-types.md) | A trait impl item whose signature does not match the trait exactly (N-A-32) |
| `TRAIT_NOT_IN_SCOPE` | error | [05](05-types.md) | The method exists only in a trait that is not in scope; the fix adds a `use` |
| `TRAIT_SOLVER_OVERFLOW` | error | [05](05-types.md) | Trait solving exceeded depth 128 (N-A-36) |
| `TRANSMUTE_SIZE_MISMATCH` | error | [12](12-unsafe-and-ffi.md) | The sizes of the source and target types of `transmute` differ or depend on a type parameter (§12.5.2) |
| `TRY_ERROR_TYPE_MISMATCH` | error | [10](10-errors-and-panics.md) | The error type of a `Result` operand of `?` differs from the error type of the enclosing body's return type (§10.3.2 item 4) |
| `TRY_INVALID_OPERAND` | error | [10](10-errors-and-panics.md) | The operand of `?` is not a `Result` or an `Option`, including a borrow of one and a future that is not awaited (§10.3.2 item 1) |
| `TRY_INVALID_RETURN_TYPE` | error | [10](10-errors-and-panics.md) | `?` in a body whose return type is neither a `Result` nor an `Option` (§10.3.2 item 3) |
| `TRY_OPTION_RESULT_MIX` | error | [10](10-errors-and-panics.md) | `?` applied to an `Option` in a body returning a `Result`, or to a `Result` in a body returning an `Option` (§10.3.2 item 3) |
| `TUPLE_INDEX_OUT_OF_RANGE` | error | [06](06-expressions-and-statements.md) | `e.N` with `N` not less than the tuple's arity |
| `TYPE_ANNOTATIONS_NEEDED` | error | [05](05-types.md) | A type that must be known (§5.16.4), or remains unknown at the end of the body (§5.16.5), or an obligation that stays ambiguous |
| `TYPE_MISMATCH` | error | [05](05-types.md) | Two types that must be equal, or a coercion that does not apply; names the expected and found types and the origin of the expectation |
| `UNCONSTRAINED_IMPL_PARAM` | error | [05](05-types.md) | An impl parameter that does not occur in the impl header |
| `UNDECLARED_LABEL` | error | [06](06-expressions-and-statements.md) | A `break` or `continue` label that names no enclosing loop |
| `UNEXPECTED_CHARACTER` | error | [02](02-lexical-structure.md) | Character that cannot start any token |
| `UNEXPECTED_FN_BODY` | error | [03](03-grammar.md) | A body on a function of an extern block (§3.5, §3.10) |
| `UNEXPECTED_NEWLINE` | error | [02](02-lexical-structure.md) | NEWLINE in a position the grammar does not allow |
| `UNKNOWN_ABI` | error | [03](03-grammar.md) | Unknown extern ABI (§3.10) |
| `UNKNOWN_ASSOC_TYPE` | error | [05](05-types.md) | An associated-type binding or projection naming no associated type of the trait |
| `UNKNOWN_ATTRIBUTE` | error | [03](03-grammar.md) | An attribute name outside the closed set of 15 §15.2.3 (§3.3) |
| `UNKNOWN_CAPABILITY` | error | [08](08-effects-and-capabilities.md) | A capability name denotes no known WIT package, interface or target capability (§8.12.1) |
| `UNKNOWN_DERIVE` | error | [05](05-types.md) | A name in `@derive` that is not a derivable trait |
| `UNKNOWN_EFFECT` | error | [08](08-effects-and-capabilities.md) | An `EffectRef` names no built-in effect, user effect, interface binding or world (§8.2.2) |
| `UNKNOWN_EXPORT_WORLD` | error | [13](13-components-and-wit.md) | `export world` names no world (§13.5.1) |
| `UNKNOWN_FIELD` | error | [06](06-expressions-and-statements.md) | A field name that the record, variant or tuple type does not have |
| `UNKNOWN_FORMAT_SPEC` | error | [16](16-core-library.md) | A format specifier name that is not one of `debug`, `hex`, `upper_hex`, `bin`, `oct`, `exp`, `prec`, `pad`, `left` and `zpad`; data: `name` (§16.14.3) |
| `UNKNOWN_LINT` | error | [15](15-attributes-and-lints.md) | A level attribute, a `lints` entry or a CLI lint flag names something that is not a diagnostic code (§15.15.4); the registry lookup is in [impl/11](../impl/11-diagnostics.md) §11.12.3 |
| `UNKNOWN_MODULE` | error | [04](04-modules-and-names.md) | No module or interface has the given path |
| `UNKNOWN_PACKAGE` | error | [04](04-modules-and-names.md) | The package of a `PKG_PATH` is neither the current package, `sai:core`, `sai:alloc`, a WASI package of the toolchain, nor a declared dependency |
| `UNKNOWN_TRAIT_ITEM` | error | [05](05-types.md) | A trait impl item that the trait does not declare |
| `UNKNOWN_VARIANT` | error | [05](05-types.md) | `E.V` naming no variant of `E`, or a UIDENT member of a non-enum type |
| `UNKNOWN_WIT_ITEM` | error | [13](13-components-and-wit.md) | A `use` path names no interface or world of a WIT package (§13.7.2) |
| `UNNECESSARY_RAW_IDENT` | warning (lint) | [02](02-lexical-structure.md) | Unnecessary `r#` |
| `UNNECESSARY_VAR` | warning (lint) | [15](15-attributes-and-lints.md) | A `var` binding that never needs to be mutable (§15.18.2) |
| `UNREACHABLE_CODE` | warning (lint) | [06](06-expressions-and-statements.md) | Code after a diverging expression |
| `UNREACHABLE_PATTERN` | warning (lint) | [06](06-expressions-and-statements.md) | A match arm or top-level alternative that can never match |
| `UNRESOLVED_IMPORT` | error | [04](04-modules-and-names.md) | An item of a `use` list is not in the target's member namespace |
| `UNRESOLVED_NAME` | error | [04](04-modules-and-names.md) | A name or path segment does not resolve |
| `UNSAFE_EXPORT` | error | [12](12-unsafe-and-ffi.md) | An `export unsafe fn` (§12.2.2, §12.8) |
| `UNSAFE_OPERATION_OUTSIDE_UNSAFE` | error | [12](12-unsafe-and-ffi.md) | An unsafe operation outside an unsafe context (§12.2.5, §12.3) |
| `UNSATISFIED_BOUND` | error | [05](05-types.md) | A trait obligation that no candidate satisfies (§5.11.2) |
| `UNTERMINATED_CHAR` | error | [02](02-lexical-structure.md) | Unterminated character or byte literal |
| `UNTERMINATED_COMMENT` | error | [02](02-lexical-structure.md) | Unterminated block comment |
| `UNTERMINATED_STRING` | error | [02](02-lexical-structure.md) | Unterminated string |
| `UNUSED_ALIAS_PARAM` | error | [05](05-types.md) | A type alias parameter that does not occur in the aliased type (N-A-26) |
| `UNUSED_ATTRIBUTE` | warning (lint) | [15](15-attributes-and-lints.md) | A valid attribute that has no effect (§15.18.6) |
| `UNUSED_EFFECT_DECLARATION` | warning (lint) | [08](08-effects-and-capabilities.md) | A private function declares an atom it does not need (§8.9.2) |
| `UNUSED_IMPORT` | warning (lint) | [04](04-modules-and-names.md) | A non-`pub` `use` binding is never referenced (default level in [15](15-attributes-and-lints.md) §15.17.2) |
| `UNUSED_ITEM` | warning (lint) | [15](15-attributes-and-lints.md) | A package-private item that is never referenced (§15.18.3) |
| `UNUSED_LABEL` | warning (lint) | [15](15-attributes-and-lints.md) | A loop label that no `break` or `continue` names (§15.18.4) |
| `UNUSED_MUST_USE` | error | [06](06-expressions-and-statements.md) | A discarded value of a must-use type, or of a must-use call ([15](15-attributes-and-lints.md) §15.5, D-EXPR-01) (§6.25.2) |
| `UNUSED_UNSAFE` | warning (lint) | [12](12-unsafe-and-ffi.md) | An `unsafe` block, `unsafe module` header or `unsafe extern "wasm"` marker with nothing to cover, an `unsafe` block already in an unsafe context, or an `unsafe fn` marker on an `extern "C"` item (§12.2.1, §12.2.4, §12.7.3, §12.9.1) |
| `UNUSED_VARIABLE` | warning (lint) | [15](15-attributes-and-lints.md) | A local binding or parameter that is never used (§15.18.1) |
| `USE_AFTER_MOVE` | error | [07](07-ownership-and-borrowing.md) | A place is used after it has been moved out, or may have been on some path (§7.4.4) |
| `USE_AND_OR` | error | [02](02-lexical-structure.md) | Use of `&&` / `||` |
| `USE_BINDING_IS_KEYWORD` | error | [04](04-modules-and-names.md) | The derived binding name of a `use` is a keyword or reserved word; an alias is required |
| `USE_OF_CURRENT_MODULE` | error | [04](04-modules-and-names.md) | A `use` names the module that contains it |
| `USE_WHILE_MUT_BORROWED` | error | [07](07-ownership-and-borrowing.md) | A place is read while it is mutably borrowed (§7.9.6) |
| `VISIBILITY_ON_TRAIT_IMPL_ITEM` | error | [03](03-grammar.md) | `pub` on an item of a trait or interface impl (§3.7) |
| `WHERE_WITHOUT_PARAM` | error | [05](05-types.md) | A `where` predicate whose subject mentions no generic parameter of the item |
| `WIT_EMPTY_TYPE` | error | [13](13-components-and-wit.md) | An interface record without fields, or an enum or flags without variants (§13.2.2) |
| `WIT_EXPLICIT_DISCRIMINANT` | error | [13](13-components-and-wit.md) | An explicit discriminant on a variant of an interface enum (§13.2.2) |
| `WIT_INCOMPATIBLE_TYPE` | error | [13](13-components-and-wit.md) | A boundary type, payload shape, receiver or `@repr` that has no WIT mapping (§13.2.2, §13.2.4, §13.9.3) |
| `WIT_ITEM_SHADOWED` | warning | [13](13-components-and-wit.md) | An item of imported WIT is not accessible from SAI because its SAI name collides with another item, such as a static function `new` next to a constructor (§13.7.4, §13.8.5) |
| `WIT_NAME_COLLISION` | error | [13](13-components-and-wit.md) | Two SAI items map to the same WIT name in one scope (§13.8.5) |
| `WIT_NAME_NOT_REPRESENTABLE` | error | [13](13-components-and-wit.md) | A SAI identifier that must cross the boundary has no WIT name (§13.8.4) |
| `WIT_PARSE_ERROR` | error | [13](13-components-and-wit.md) | A visible WIT package fails to parse or validate (§13.7.1) |
| `WIT_UNSUPPORTED_FEATURE` | error | [13](13-components-and-wit.md) | Use of a WIT item whose signature contains a construct not supported in v0.1, or, from Stage 5 on, an imported `async` function or a `future` or `stream` type on a target without the Component Model async ABI (§13.9.2) |
| `WORLD_DUPLICATE_ITEM` | error | [13](13-components-and-wit.md) | An elaborated world imports and exports the same interface, or repeats a name (§13.4.2, §13.5.3) |
| `WORLD_EXPORT_UNIMPLEMENTED` | error | [13](13-components-and-wit.md) | An exported interface or world function has no implementation (§13.4.1) |
| `WORLD_IMPORT_NEEDS_INTERFACE` | error | [03](03-grammar.md) | A world import or export that is not per interface (§3.9) |
| `WORLD_IMPORT_UNDECLARED` | error | [13](13-components-and-wit.md) | The component requires an interface that the export world does not import (§13.5.2) |
| `WORLD_INCLUDE_CYCLE` | error | [13](13-components-and-wit.md) | Worlds include each other cyclically (§13.4.2) |
| `WRONG_ARGUMENT_COUNT` | error | [06](06-expressions-and-statements.md) | A call with the wrong number of arguments |
| `WRONG_NAME_KIND` | error | [04](04-modules-and-names.md) | A name resolves to an entity of the wrong kind for its position |
| `WRONG_NUMBER_OF_GENERIC_ARGS` | error | [05](05-types.md) | A generic argument list with the wrong number of arguments (§5.2.2) |

## A.3 Package, lock file and distribution codes

Codes owned by the package documents (`package/`). 101 codes.

| Code | Severity | Owner | Summary |
|---|---|---|---|
| `ALLOCATOR_NOT_REQUIRED` | error | [package/01](../package/01-package-contract.md) | `allocator` names a package that the root does not require |
| `ALREADY_REQUIRED` | error | [package/02](../package/02-resolver.md) | `sai add` for an identity that the root already requires (§2.11) |
| `ARTIFACT_NOT_FOUND` | error | [package/01](../package/01-package-contract.md) | An artifact file that does not exist or is not a regular file |
| `ARTIFACT_ON_CORE_TARGET` | error | [package/01](../package/01-package-contract.md) | An artifact keyed by a target that is not a component target: a core target, or a native target ([targets](../targets/targets-and-profiles.md) §1.4 item 6, N-F-62) |
| `ARTIFACT_TARGET_NOT_DECLARED` | error | [package/01](../package/01-package-contract.md) | An artifact for a target missing from `targets` |
| `ARTIFACT_WIT_MISMATCH` | error | [package/04](../package/04-distribution-and-security.md) | An artifact does not conform to the package's `export world` (§4.5.2) |
| `BARE_VERSION_REQUIREMENT` | error | [package/01](../package/01-package-contract.md) | A version requirement without an operator |
| `BUILD_METADATA_NOT_ALLOWED` | error | [package/01](../package/01-package-contract.md) | `+build` metadata in a version |
| `CACHE_CORRUPT` | warning | [package/04](../package/04-distribution-and-security.md) | A cached entry does not match its checksum and was removed (§4.4.2) |
| `CAPABILITY_ESCALATION` | error | [package/02](../package/02-resolver.md) | A lock change adds capabilities to a node and was not approved (§2.12.3) |
| `CAPABILITY_NOT_GRANTED` | error | [package/01](../package/01-package-contract.md) | A capability required by a dependency or its dependencies is neither covered by the requirement's `grant` nor, for `wasi:io`, implied by it ([targets](../targets/targets-and-profiles.md) §3.3) (build time) |
| `CHECKSUM_MISMATCH` | error | [package/03](../package/03-lock-file.md) | Fetched content does not match the source, WIT, or artifact checksum of the lock (§3.7.3) |
| `COMPILER_VERSION_MISMATCH` | error | [package/01](../package/01-package-contract.md) | The toolchain's language version does not satisfy a package's `compiler` requirement |
| `CONFLICTING_LINT_LEVEL` | error | [package/01](../package/01-package-contract.md) | One lint given two different levels in `lints` |
| `CONFLICTING_PATH_SOURCES` | error | [package/02](../package/02-resolver.md) | Two different directories pin the same node (§2.7.1) |
| `CONFUSABLE_PACKAGE_NAME` | warning | [package/04](../package/04-distribution-and-security.md) | A new identity is confusable with another identity of the graph, the bundled packages, or the root (§4.11.1) |
| `CREDENTIALS_IN_URL` | error | [package/04](../package/04-distribution-and-security.md) | A Git URL contains credentials (§4.2.1) |
| `DEPENDENCY_CYCLE` | error | [package/02](../package/02-resolver.md) | Selected package versions form a cycle, or a dependency requires the root's own line (§2.4.4) |
| `DIVERGENT_SOURCES` | error | [package/02](../package/02-resolver.md) | Two sources provide different contracts for the same package version (§2.8.3) |
| `DUPLICATE_DECLARATION` | error | [package/01](../package/01-package-contract.md) | A declaration, profile block, profile item, or artifact target given twice |
| `DUPLICATE_LIST_ITEM` | warning | [package/01](../package/01-package-contract.md) | An element repeated inside one list |
| `DUPLICATE_OVERRIDE` | error | [package/01](../package/01-package-contract.md) | Two overrides of the same kind for one identity |
| `DUPLICATE_PACKAGE_LINE` | warning (lint) | [package/02](../package/02-resolver.md) | Several lines of one package in the graph (§2.9, D-PKG-02); an error under `policy { deny duplicate_lines }` |
| `DUPLICATE_REQUIREMENT` | error | [package/01](../package/01-package-contract.md) | An identity required twice across `requires` and `requires test` |
| `EMPTY_VERSION_REQUIREMENT` | error | [package/01](../package/01-package-contract.md) | A requirement that no version satisfies |
| `FETCH_FAILED` | error | [package/02](../package/02-resolver.md) | A network or protocol failure while fetching from a Git source (§2.6.2) |
| `FOOTPRINT_EXCEEDS_POLICY` | info | [package/04](../package/04-distribution-and-security.md) | A reviewed footprint contains capabilities that the package's or the root's policy does not allow (§4.7.1) |
| `FOOTPRINT_NOT_GRANTED` | info | [package/04](../package/04-distribution-and-security.md) | A reviewed footprint is not covered by the `grant` of an edge (§4.7.1) |
| `HYBRID_WIT_MISMATCH` | error | [package/04](../package/04-distribution-and-security.md) | The WIT generated from a hybrid package's source differs from its declared world (§4.5.3) |
| `IMPLICIT_PACKAGE_REQUIRED` | error | [package/01](../package/01-package-contract.md) | `sai:core` or `sai:alloc` required or overridden |
| `INSECURE_GIT_URL` | error | [package/04](../package/04-distribution-and-security.md) | A Git URL uses `http` or `git` (§4.2.1) |
| `INVALID_ARTIFACT` | error | [package/04](../package/04-distribution-and-security.md) | An artifact is not a valid Wasm component, or uses the wrong memory width (§4.5.2) |
| `INVALID_CAPABILITY_NAME` | error | [package/01](../package/01-package-contract.md) | A name that is not a capability name of D-CAP-01 |
| `INVALID_DESCRIPTION` | error | [package/01](../package/01-package-contract.md) | `description` contains a line break or exceeds 1024 UTF-8 bytes |
| `INVALID_LICENSE_EXPRESSION` | warning | [package/01](../package/01-package-contract.md) | `license` is not a valid SPDX expression |
| `INVALID_OVERRIDE` | error | [package/01](../package/01-package-contract.md) | `use artifact` on a package that is not hybrid |
| `INVALID_PACKAGE_FILE_PATH` | error | [package/03](../package/03-lock-file.md) | A file path in fetched content violates §3.4.3 |
| `INVALID_PACKAGE_NAME` | error | [package/01](../package/01-package-contract.md) | A package identity that is not `namespace:name` or has a segment longer than 64 bytes |
| `INVALID_PACKAGE_PATH` | error | [package/01](../package/01-package-contract.md) | A path in `sai.package` violates §1.6.1 |
| `INVALID_SIZE` | error | [package/01](../package/01-package-contract.md) | A unit other than `B`, `KiB`, `MiB`, a zero size, or an overflowing size |
| `INVALID_TAG` | error | [package/04](../package/04-distribution-and-security.md) | A release tag does not peel to a commit (§4.3.2) |
| `INVALID_VERSION` | error | [package/01](../package/01-package-contract.md) | Malformed version: leading zero, overflow, bad pre-release, too few or too many components |
| `INVALID_VERSION_REQUIREMENT` | error | [package/01](../package/01-package-contract.md) | Two lower or two upper bounds, `=` combined with another comparator, or a partial version with `=` or `<=` |
| `INVALID_WORLD_NAME` | error | [package/01](../package/01-package-contract.md) | An `export world` segment that is not a snake_case lower identifier, or a `PKG_PATH` without exactly one `/` part |
| `LOCK_CORRUPT` | error | [package/03](../package/03-lock-file.md) | The lock does not parse or violates §3.3.5; its recorded edges do not describe the locked content ([package/02 §2.10.2](../package/02-resolver.md)); a side of a merge conflict is invalid (§3.9.2) |
| `LOCK_MERGE_CONFLICT` | error | [package/03](../package/03-lock-file.md) | The lock contains conflict markers, or both sides of a merge lock different content for one version (§3.9) |
| `LOCK_MISSING` | error | [package/03](../package/03-lock-file.md) | `--locked` or `--frozen` and there is no `sai.lock` (§3.7.1, §3.8) |
| `LOCK_NOT_CANONICAL` | warning | [package/03](../package/03-lock-file.md) | The lock is valid but not in canonical form (§3.2.3, §3.7.2) |
| `LOCK_OUT_OF_DATE` | error | [package/03](../package/03-lock-file.md) | `--locked` or `--frozen` and the lock does not satisfy Mode A or would change; names the failed condition (§3.8.1) |
| `MISSING_ARTIFACT_FOR_TARGET` | error | [package/01](../package/01-package-contract.md) | An artifact package without an artifact for one of its targets |
| `MISSING_EXPORT_WORLD` | error | [package/01](../package/01-package-contract.md) | An artifact or hybrid package without `export world` |
| `MISSING_PACKAGE_DECL` | error | [package/01](../package/01-package-contract.md) | No `package` declaration |
| `MISSING_PACKAGE_FILE` | error | [package/02](../package/02-resolver.md) | The package directory of a fetched commit has no `sai.package` (§2.6.2) |
| `MISSING_SOURCE` | error | [package/01](../package/01-package-contract.md) | A requirement without `from` |
| `MISSING_TARGETS` | error | [package/01](../package/01-package-contract.md) | No `targets` declaration, or the list is empty |
| `NOT_REQUIRED` | error | [package/02](../package/02-resolver.md) | `sai remove` for an identity that the root does not require (§2.10.6) |
| `NO_RELEASE_TAGS` | error | [package/02](../package/02-resolver.md) | `sai add` found no release tag in the repository (§2.11) |
| `OFFLINE_FETCH_REQUIRED` | error | [package/03](../package/03-lock-file.md) | `--offline` or `--frozen` and needed content or tag information is not cached (§3.8.2) |
| `ONLY_TARGET_NOT_DECLARED` | error | [package/01](../package/01-package-contract.md) | An `only` target missing from the requiring package's `targets` |
| `PACKAGE_DECL_NOT_FIRST` | error | [package/01](../package/01-package-contract.md) | `package` is not the first declaration |
| `PACKAGE_HAS_NO_CONTENT` | error | [package/01](../package/01-package-contract.md) | Neither `source` nor `artifact` is declared |
| `PACKAGE_IDENTITY_MISMATCH` | error | [package/02](../package/02-resolver.md) | A fetched or path package declares another identity or version than requested (§2.6.2, §2.7.1) |
| `PACKAGE_REQUIRES_ITSELF` | error | [package/01](../package/01-package-contract.md) | A package requires its own identity |
| `PACKAGE_TOO_LARGE` | error | [package/04](../package/04-distribution-and-security.md) | Fetched content exceeds a limit of §4.4.4 |
| `PATH_COLLISION` | error | [package/03](../package/03-lock-file.md) | Two file paths differ only by case or Unicode normalization (§3.4.4) |
| `PATH_ESCAPES_REPOSITORY` | error | [package/02](../package/02-resolver.md) | A path source inside a Git package leaves the repository (§2.7.2) |
| `PATH_PACKAGE_NOT_FOUND` | error | [package/01](../package/01-package-contract.md) | A `from path` or `use path` directory without `sai.package` |
| `PATH_VERSION_MISMATCH` | error | [package/02](../package/02-resolver.md) | A path package's declared version does not satisfy the requirement or is in another line (§2.7.1) |
| `POLICY_ITEM_IS_CAPABILITY` | error | [package/01](../package/01-package-contract.md) | A capability name used as a policy item |
| `PROFILE_ALLOW_EXCEEDS_PACKAGE` | error | [package/01](../package/01-package-contract.md) | A profile `allow` name not covered by the package `allow` |
| `RESERVED_NAMESPACE` | error | [package/01](../package/01-package-contract.md) | A package declared in the `wasi` namespace |
| `RESERVED_PACKAGE_NAME` | error | [package/01](../package/01-package-contract.md) | A package declared as `sai:core` or `sai:alloc` |
| `SIZE_BUDGET_EXCEEDED` | error | [package/01](../package/01-package-contract.md) | The build output exceeds a budget; the build fails and writes no output file (§1.14.3 item 4) |
| `SOURCE_CHANGED` | error | [package/04](../package/04-distribution-and-security.md) | A lock change moves a node to another source and was not approved (§4.7.2) |
| `SUBMODULE_IGNORED` | warning | [package/03](../package/03-lock-file.md) | A submodule in fetched content is not part of the package (§3.4.2) |
| `SYMLINK_IGNORED` | warning | [package/03](../package/03-lock-file.md) | A symbolic link in fetched content is not part of the package (§3.4.2) |
| `TAG_MISSING` | warning | [package/04](../package/04-distribution-and-security.md) | The release tag of a locked version no longer exists; the locked commit was used (§4.3.3) |
| `TAG_MOVED` | error | [package/04](../package/04-distribution-and-security.md) | A release tag of a locked version peels to another commit than the lock records, or its content replacement was not approved (§4.3.3, §4.7.2) |
| `TARGET_NOT_DECLARED` | error | [package/01](../package/01-package-contract.md) | Building the root for a target not in its `targets` |
| `TARGET_NOT_SUPPORTED_BY_DEPENDENCY` | error | [package/01](../package/01-package-contract.md) | An active dependency does not list the build target |
| `UNKNOWN_ARTIFACT_KIND` | error | [package/01](../package/01-package-contract.md) | `artifact` followed by a word other than `component` |
| `UNKNOWN_BUDGET_METRIC` | error | [package/01](../package/01-package-contract.md) | A budget metric other than `wasm` |
| `UNKNOWN_DECLARATION` | error | [package/01](../package/01-package-contract.md) | A line starts with a word that is not a declaration keyword |
| `UNKNOWN_POLICY_ITEM` | error | [package/01](../package/01-package-contract.md) | An unknown `policy` item |
| `UNKNOWN_PROFILE` | error | [package/01](../package/01-package-contract.md) | A profile name other than `dev`, `release`, `tiny` |
| `UNKNOWN_TARGET` | error | [package/01](../package/01-package-contract.md) | A name that is not a target name |
| `UNSAFE_DENIED` | error | [package/01](../package/01-package-contract.md) | An unsafe construct in a package with `policy { deny unsafe }` |
| `UNSUPPORTED_GIT_URL` | error | [package/04](../package/04-distribution-and-security.md) | A Git URL uses the scp-like form, `file`, a local path, another scheme, a non-ASCII host, an invalid path, a query, or a fragment (§4.2.1) |
| `UNSUPPORTED_LOCK_VERSION` | error | [package/03](../package/03-lock-file.md) | The lock's format is newer than the toolchain supports (§3.10) |
| `UNSUPPORTED_STRING_FORM` | error | [package/01](../package/01-package-contract.md) | Multiline, raw, byte, or format string, or a line continuation, in `sai.package` or `sai.lock` |
| `UNSUPPORTED_VERSION_OPERATOR` | error | [package/01](../package/01-package-contract.md) | The operator `>` |
| `UNUSED_OVERRIDE` | warning | [package/01](../package/01-package-contract.md) | An override that matches nothing in the resolved graph |
| `UPDATE_CHANGES_LINE` | error | [package/02](../package/02-resolver.md) | `sai update id@w` names a line that the graph does not contain (§2.10.5) |
| `UPDATE_VERSION_NOT_SELECTED` | error | [package/02](../package/02-resolver.md) | `sai update id@w` cannot select `w` because a requirement forces a higher version (§2.10.5) |
| `VERSION_CONFLICT` | error | [package/02](../package/02-resolver.md) | A selected version does not satisfy a requirement of the selected graph, or a pin; reported with both chains (§2.4.3, §2.13.1; named by D-PKG-02) |
| `VERSION_NOT_FOUND` | error | [package/02](../package/02-resolver.md) | The release tag of a requested version does not exist (§2.6.2, §2.10.5, §2.11) |
| `VERSION_REQ_NO_LOWER_BOUND` | error | [package/01](../package/01-package-contract.md) | A requirement without `^`, `~`, `=`, or `>=` |
| `VERSION_REQ_SPANS_LINES` | warning | [package/01](../package/01-package-contract.md) | A requirement interval contains the stable start of the next compatibility line |
| `WIT_DIR_NOT_FOUND` | error | [package/01](../package/01-package-contract.md) | The `wit` directory does not exist |
| `WIT_PACKAGE_MISMATCH` | error | [package/01](../package/01-package-contract.md) | The WIT package name or version differs from the package identity |
| `WIT_REQUIRED` | error | [package/01](../package/01-package-contract.md) | An artifact or hybrid package without `wit` |

## A.4 Target and profile codes

Codes owned by the target document (`targets/`). 4 codes.

| Code | Severity | Owner | Summary |
|---|---|---|---|
| `ENTRY_POINT_NOT_APPLIED` | info | [targets](../targets/targets-and-profiles.md) | The root package defines `main`, but the build produces a core module, which has no entry point (§4.5 item 4) |
| `INVALID_ENTRY_POINT` | error | [targets](../targets/targets-and-profiles.md) | The function `main` of the root module of the root package does not have the form `pub fn main(env: owned process.Env) -> process.ExitCode` (§4.2) |
| `NATIVE_ENTRY_POINT_REQUIRED` | error | [targets](../targets/targets-and-profiles.md) | `sai build` for a native target, and the root package has no entry point (§2.8.5 item 2) |
| `NATIVE_LIBRARY_NOT_FOUND` | error | [targets](../targets/targets-and-profiles.md) | A library named by a reachable `unsafe extern "C"` block cannot be found for a native target (§2.8.2) |

## A.5 Toolchain codes

Codes owned by implementation documents (`impl/`). 44 codes.

| Code | Severity | Owner | Summary |
|---|---|---|---|
| `ALLOCATOR_INTERFACE_MISMATCH` | error | [impl/10](../impl/10-runtime.md) | The package named by `allocator` does not declare the items of §10.2.7 with the required names, kinds and signatures, a function has effects, is generic, `async` or has a contract, `STATE_SIZE` is outside `0..=4096`, or the package is not used in source form (§10.2.7) |
| `ALLOCATOR_REENTRANCY` | error | [impl/10](../impl/10-runtime.md) | A function of the allocator package can reach an allocation primitive of `alloc.heap`, including through drop glue (§10.2.7 item 2) |
| `ARGUMENT_NOT_REMOVABLE` | error | [impl/12](../impl/12-semantic-tooling.md) | `remove_parameter`: the argument at a call site is not a removable expression, so removing it could remove effects or a panic (§12.6.9). |
| `CACHE_UNAVAILABLE` | error | [impl/13](../impl/13-cli.md) | The location of the global cache cannot be determined (§13.7). |
| `CONFLICTING_OPTIONS` | error | [impl/13](../impl/13-cli.md) | Two options that cannot be combined, a value option given twice, or one lint code in two level flags (usage error). |
| `DEPRECATED_DIAGNOSTIC_CODE` | warning | [impl/11](../impl/11-diagnostics.md) | A level attribute, a `lints` entry, or a CLI lint flag names a deprecated or removed code (§11.12.2, §11.12.3) |
| `DESTINATION_NOT_EMPTY` | error | [impl/13](../impl/13-cli.md) | `sai new` was given a directory that exists and is not empty (§13.6.1). |
| `EXTRACT_NOT_POSSIBLE` | error | [impl/12](../impl/12-semantic-tooling.md) | `extract_function`: the selected nodes cannot form a function body that preserves meaning (§12.6.10). |
| `FILE_HAS_SYNTAX_ERRORS` | error | [impl/12](../impl/12-semantic-tooling.md) | A source file of a workspace package has lexer or parser errors, so no operation can be planned (§12.6.2 rule 4). |
| `FIX_NOT_FOUND` | error | [impl/12](../impl/12-semantic-tooling.md) | `sai/applyFix`: the snapshot has no diagnostic with the given code at the given node, or the diagnostic has no fix with the given index (§12.5.3). |
| `FUNCTION_STILL_REFERENCED` | error | [impl/12](../impl/12-semantic-tooling.md) | `delete_function`: something outside the target's declaration, other than `use` items, still refers to it (§12.6.6). |
| `INLINE_NOT_POSSIBLE` | error | [impl/12](../impl/12-semantic-tooling.md) | `inline_function`: the function cannot be inlined while preserving meaning (§12.6.11). |
| `INVALID_OPERATION_ARGUMENT` | error | [impl/12](../impl/12-semantic-tooling.md) | A parameter is missing, unknown, or not a valid value of its type, or a `node` parameter is not inside the target's declaration (§12.6.1, §12.6.2 rules 1 and 2, and the operations). |
| `INVALID_OPERATION_TARGET` | error | [impl/12](../impl/12-semantic-tooling.md) | The target is not of a kind the operation accepts, is a module where a symbol is required, or is declared in a dependency; or a client cannot create files (§12.2.5 rule 4, §12.6.2 rule 3, §12.10.6). |
| `INVALID_OPTION_VALUE` | error | [impl/13](../impl/13-cli.md) | A value is not one of the allowed values or not of the required form (usage error). |
| `LARGE_STACK_FRAME` | info | [impl/06](../impl/06-wasm-lowering-and-layout.md) | A function's shadow-stack frame is larger than `S / 4` (256 KiB) (§6.7.2). |
| `MISSING_ARGUMENT` | error | [impl/13](../impl/13-cli.md) | A value option has no value, or a required argument is missing (usage error). |
| `MISSING_ENTRY_POINT` | error | [impl/13](../impl/13-cli.md) | `sai run` was used on a reactor component; the package has no `main` (§13.6.5). |
| `NOT_A_GIT_REPOSITORY` | error | [impl/13](../impl/13-cli.md) | `sai diff` needs a revision, but the workspace root is not in a Git repository (§13.6.17). |
| `NOT_PRESENT_IN_BUILD` | info | [impl/13](../impl/13-cli.md) | The item named by `sai size --why` or `sai inspect effects --why` is not in the build or the effect set (§13.6.14, §13.6.15). |
| `NO_PACKAGE_FOUND` | error | [impl/13](../impl/13-cli.md) | No `sai.package` was found from the start directory upward (§13.2.1, usage error). |
| `OPERATION_BREAKS_PUBLIC_API` | error | [impl/13](../impl/13-cli.md) | `sai edit` would break the public API and `--allow-breaking` was not given (§13.6.18). |
| `OPERATION_INTRODUCES_ERRORS` | error | [impl/12](../impl/12-semantic-tooling.md) | The planned edits would introduce errors; `data.errors` lists them (§12.6.2 rule 6). |
| `OPERATION_NAME_CONFLICT` | error | [impl/12](../impl/12-semantic-tooling.md) | A name that the operation would introduce is already declared or bound where it would be visible, or would shadow a name that a reference must keep (§12.6.3, §12.6.4, §12.6.5, §12.6.8, §12.6.10, §12.6.12). |
| `OUTPUT_WRITE_FAILED` | error | [impl/13](../impl/13-cli.md) | A file that the toolchain had to write could not be written. Exit code 4 (§13.3.4). |
| `PARAMETER_STILL_USED` | error | [impl/12](../impl/12-semantic-tooling.md) | `remove_parameter`: the body of the target, or of an impl of the trait method, still uses the parameter (§12.6.9). |
| `PATH_NOT_FOUND` | error | [impl/13](../impl/13-cli.md) | A path given to `sai fmt` does not exist (§13.6.7). |
| `ROOT_PACKAGE_HAS_NO_SOURCE` | error | [impl/13](../impl/13-cli.md) | A command that compiles the root package was run on a root without `source` (§13.6.3). |
| `SOURCE_READ_FAILED` | error | [impl/01](../impl/01-architecture.md) | A file of a package unit is listed but cannot be read; data: `path`, `reason` (§1.7.2 rule 3) |
| `STALE_SNAPSHOT` | error | [impl/12](../impl/12-semantic-tooling.md) | A request names a document version that is not the current one; JSON-RPC code `-32801` (§12.3.3, §12.5.2). |
| `SYMBOL_INDEX_DISCARDED` | info | [impl/12](../impl/12-semantic-tooling.md) | The symbol index in `.sai/index/` is unreadable, has another format or version, or has an invalid file, and is discarded; `primary` is `null` (§12.9.3 rule 4). |
| `SYMBOL_INDEX_RECOVERY_REQUIRED` | error | [impl/12](../impl/12-semantic-tooling.md) | An unresolved or invalid symbol-index journal blocks allocation and indexed edits (§12.9.3). |
| `TARGET_NOT_RUNNABLE` | error | [impl/13](../impl/13-cli.md) | `sai run` was used with a target that is not a WASI target: a core target or a native target (§13.6.5). |
| `TRAIT_ALREADY_IMPLEMENTED` | error | [impl/12](../impl/12-semantic-tooling.md) | `implement_trait`: the type already has an impl of the trait with the same arguments, written or derived (§12.6.13). |
| `TYPE_TOO_LARGE` | error | [impl/06](../impl/06-wasm-lowering-and-layout.md) | The size of a reachable Mono type exceeds `MAX_OBJECT` (§6.4.1). Reported at the use that instantiated the type. |
| `UNEXPECTED_ARGUMENT` | error | [impl/13](../impl/13-cli.md) | A positional argument that the command does not take (usage error). |
| `UNFORMATTED_FILE` | error | [impl/14](../impl/14-formatter.md) | `sai fmt --check`: the file is not in canonical format; the primary location is the first differing line (§14.11.4). |
| `UNKNOWN_COMMAND` | error | [impl/13](../impl/13-cli.md) | The command or subcommand does not exist (usage error, §13.3.5). |
| `UNKNOWN_DIAGNOSTIC_CODE` | error | [impl/11](../impl/11-diagnostics.md) | `sai explain` was given a name that is not a registered code (§11.13.1) |
| `UNKNOWN_NODE_ID` | error | [impl/12](../impl/12-semantic-tooling.md) | A node ID is malformed, names a file outside the workspace snapshot, or names no element of the given group (§12.3.3). |
| `UNKNOWN_OPERATION` | error | [impl/12](../impl/12-semantic-tooling.md) | The operation name is not one of §12.6 (§12.6.1). |
| `UNKNOWN_OPTION` | error | [impl/13](../impl/13-cli.md) | The option does not exist for this command (usage error). |
| `UNKNOWN_REVISION` | error | [impl/13](../impl/13-cli.md) | A revision given to `sai diff` does not resolve to a commit (§13.6.17). |
| `UNKNOWN_SYMBOL_ID` | error | [impl/12](../impl/12-semantic-tooling.md) | A string that starts with `symbol://` is not a valid symbol ID, or no symbol of the chosen package node has its hexadecimal part (§12.2.5). |
| `UNKNOWN_SYMBOL_PATH` | error | [impl/12](../impl/12-semantic-tooling.md) | A canonical path does not parse or names no symbol or module, or a node key names no graph node (§12.2.5, §12.4.3). |

## A.6 Counts

v0.1 defines 494 codes: 344 in spec, 101 in package, 4 in targets, 45 in impl. 28 of them are lints.
