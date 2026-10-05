# 15. Attributes and Lints

**Status:** Normative
**Stage:** 0 (attribute syntax and validation, `@must_use`, `@deprecated`, `@inline`, `@cold`, `@repr`, `@target`, `@doc(hidden)`, level attributes, level resolution, the lints of §15.18); 1 (`@export_name`, `@import_name`, `@panic_hook`, and the code-generation effects of `@inline`, `@cold` and `@repr`); 2 (`@derive`); 3 (`@cases`, `@seed`, the `lints { }` block and `policy` as level sources); 4 (`@flags`)
**Related:** [decisions.md](../design/decisions.md) D-SYN-09, D-TOOL-01, D-EXPR-01, D-ERR-01, D-CAP-01, D-CAP-02, D-CONTRACT-01, D-EFF-01, D-LEX-02, D-LEX-03, D-OWN-06, D-PKG-02, D-TYPE-08, D-UNSAFE-01, D-WASM-03, D-WASM-04; [01](01-introduction.md), [02](02-lexical-structure.md), [03](03-grammar.md), [04](04-modules-and-names.md), [05](05-types.md), [06](06-expressions-and-statements.md), [07](07-ownership-and-borrowing.md), [08](08-effects-and-capabilities.md), [09](09-contracts-and-refinements.md), [10](10-errors-and-panics.md), [11](11-async-and-concurrency.md), [12](12-unsafe-and-ffi.md), [13](13-components-and-wit.md), [14](14-testing.md), [16](16-core-library.md), [Appendix A](appendix-a-diagnostic-codes.md), [package/01](../package/01-package-contract.md), [package/02](../package/02-resolver.md), [package/03](../package/03-lock-file.md), [package/04](../package/04-distribution-and-security.md), [impl/02](../impl/02-frontend-ast-hir.md), [impl/03](../impl/03-name-resolution-and-type-checking.md), [impl/06](../impl/06-wasm-lowering-and-layout.md), [impl/09](../impl/09-optimization-and-size.md), [impl/10](../impl/10-runtime.md), [impl/11](../impl/11-diagnostics.md), [impl/12](../impl/12-semantic-tooling.md), [impl/13](../impl/13-cli.md), [impl/14](../impl/14-formatter.md), [targets](../targets/targets-and-profiles.md)

This chapter defines the closed set of attributes of SAI v0.1 (D-SYN-09) and the lint system. For each attribute it fixes the argument grammar, the declarations it may be attached to, its meaning or the document that owns its meaning, and the errors for misuse. For lints it defines the lint levels, the level attributes `@allow`, `@warn` and `@deny`, the level sources and their precedence, the registry of every v0.1 lint with its default level, and the lints owned by this chapter. The attribute syntax is in [03](03-grammar.md) §3.3, the diagnostic data model, severities and the post-processing of levels in [impl/11](../impl/11-diagnostics.md) §11.3, the `lints { }` block and `policy` of `sai.package` in [package/01](../package/01-package-contract.md) §1.12–§1.13, and the CLI lint flags in [impl/13](../impl/13-cli.md) §13.4.4.

---

## 15.1 Conventions

### 15.1.1 Terms

- An **attribute** is the syntax `@name` or `@name(args)` written before a declaration ([03](03-grammar.md) §3.3). Its **name** is the `LIDENT` after `@`. Its **arguments** are the `AttrArg`s between the parentheses.
- The declaration an attribute is written before is its **target**. The **target kind** is the syntactic kind of that declaration (§15.2.2).
- An attribute is **valid** if it passes every check of §15.2.4 and of its own section. An invalid attribute is dropped: the rest of the program is checked as if it had not been written.
- A **diagnostic code** is a stable UPPER_SNAKE name (D-TOOL-01, [impl/11](../impl/11-diagnostics.md) §11.2). Every code is either a **hard diagnostic**, whose severity is fixed, or a **lint**, whose severity is computed from a level ([impl/11](../impl/11-diagnostics.md) §11.2.3).
- A **lint occurrence** is one report of a lint at one primary span. Its **effective level** is computed by §15.16.
- A **level source** is one of `default`, `attribute`, `manifest`, `policy` and `cli` ([impl/11](../impl/11-diagnostics.md) §11.3.4).
- The **root package** and the **workspace members** are those of [impl/13](../impl/13-cli.md) §13.2. A package that is neither is a **dependency** for the purposes of this chapter.

The key words MUST, MUST NOT, SHOULD, SHOULD NOT and MAY are to be interpreted as in RFC 2119.

### 15.1.2 Examples

Examples are written in SAI. A line that is rejected carries a comment `// rejected: CODE`. A line that produces a lint at its default level carries `// lint: CODE`. All other lines are accepted without diagnostics. Each example is a fragment of a larger package, so its package-private items are assumed to be used elsewhere: the lint `UNUSED_ITEM` (§15.18.3) is marked only in the examples of §15.18.

---

## 15.2 Attributes: general rules

### 15.2.1 Syntax and positions

The grammar of [03](03-grammar.md) §3.3 is:

```ebnf
Attributes    ::= ( Attribute nl )*
Attribute     ::= '@' LIDENT AttrArgs?
AttrArgs      ::= '(' List<AttrArg, Comma>? ')'
AttrArg       ::= Ident ( '(' List<AttrArg, Comma>? ')' )?
                | STRING
                | INT
                | Ident '=' ( STRING | INT | Ident )
```

1. Attributes are written after the doc comments of a declaration and before the declaration itself (`OuterDocs Attributes …`). Each attribute is followed by a line break or by another attribute on the same line. The formatter puts each attribute on its own line ([impl/14](../impl/14-formatter.md)).
2. Attributes are allowed before an `Item` (including `use`, `impl`, `extern` blocks, `test` and `property`), a `FieldDecl`, a `VariantDecl`, a `TraitItem`, an `ImplItem`, an `InterfaceItem`, an `IfaceResourceItem` and an `ExternItem` ([03](03-grammar.md)).
3. Attributes are not allowed anywhere else. An attribute sequence before a statement, an expression, a parameter, a generic parameter, a record-variant field, a pattern or a `WorldItem` is `INVALID_ATTRIBUTE_TARGET`, reported once at the first `@` of the sequence; the statement or declaration is then checked as if the attributes were absent. An attribute sequence followed by an item inside a block is `ITEM_IN_BLOCK` ([06](06-expressions-and-statements.md) §6.25.1) instead.
4. There are no attributes on files, modules or packages. Settings for a whole package are written in the `lints { }` block of `sai.package` (§15.16.3).
5. `AttrArgs` is a generic syntax. Each attribute accepts only the argument forms given in its section. `@name()` is not the same as `@name`: the first has an empty argument list, the second has none. No attribute in v0.1 accepts an empty argument list.
6. An `INT` argument is an integer literal ([02](02-lexical-structure.md) §2.6.1). Integer literals are unsigned and have no suffix, so an attribute argument is never negative. Its value is taken with arbitrary precision, and each attribute states the range it accepts. A `STRING` argument is any string literal of [02](02-lexical-structure.md) §2.6.4 (simple, multiline or raw), and its value is the string value of the literal. Byte strings and format strings are not `STRING` tokens, so they are syntax errors in this position ([03](03-grammar.md)).

### 15.2.2 Target kinds

The target kind of an attribute is one of the following. The names correspond to the `TargetKind` values of [impl/02](../impl/02-frontend-ast-hir.md) §2.10.2.

| Target kind | Declarations |
|---|---|
| `fn` | a function item at module level, with or without `pub` or `export` |
| `trait fn` | a function declared in a trait, with or without a default body |
| `impl fn` | a function in an `impl` block. It is an **inherent impl fn**, a **trait impl fn** or an **interface impl fn** according to the kind of the block ([03](03-grammar.md) §3.7) |
| `extern fn` | a function in an `extern` block |
| `interface fn` | a function of an interface, or a function of an interface resource |
| `record` | `type T { … }`, at module level or inside an interface |
| `enum` | `enum E { … }`, at module level or inside an interface |
| `distinct` | `distinct D = B`, with or without a refinement clause |
| `refined` | `type T = B where p` |
| `alias` | `type T = B` without a refinement clause, at module level or inside an interface |
| `resource` | `resource R`, at module level or inside an interface |
| `trait` | `trait Tr` |
| `impl` | an `impl` block of any kind |
| `const` | `const C: T = e` |
| `invariant` | `invariant T { … }` |
| `effect` | `effect e` |
| `interface` | `interface i { … }` |
| `world` | `world w { … }` |
| `extern block` | `extern "abi" … { … }` |
| `use` | `use` and `pub use` |
| `test` | `test "name" { … }` |
| `property` | `property "name" (…) { … }` |
| `field` | a field of a record or resource (`FieldDecl`) |
| `variant` | an enum variant (`VariantDecl`) |
| `assoc type` | an associated type declaration in a trait (`AssocTypeDecl`) or definition in an impl (`AssocTypeDef`) |

A declaration whose kind is not known because of a syntax error gets no attribute checks. Its attributes are dropped without further diagnostics.

### 15.2.3 The attribute set

The v0.1 attributes are exactly those of D-SYN-09. Any other name is `UNKNOWN_ATTRIBUTE` ([03](03-grammar.md)). A raw identifier is never an attribute name: `@r#derive` is `UNKNOWN_ATTRIBUTE`. There are no user-defined attributes and no macros (D-SYN-09).

| Attribute | Arguments | Allowed target kinds | Stage | Section |
|---|---|---|---|---|
| `@derive(Tr, …)` | one or more derive names | `record`, `enum`, `distinct`, `refined` | 2 | §15.3 |
| `@inline`, `@inline(never)` | none, or `never` | `fn`, `trait fn` with a body, `impl fn` | 0 | §15.4 |
| `@cold` | none | `fn`, `trait fn` with a body, `impl fn` | 0 | §15.4 |
| `@must_use`, `@must_use("reason")` | none, or one string | `fn`, `trait fn`, `impl fn`, `extern fn`, `interface fn`, `record`, `enum`, `distinct`, `refined`, `resource` | 0 | §15.5 |
| `@deprecated(…)` | none, a message, `instead = "path"`, or both | see §15.6.1 | 0 | §15.6 |
| `@allow(CODE, …)`, `@warn(CODE, …)`, `@deny(CODE, …)` | one or more codes | every target kind | 0 | §15.15.3 |
| `@export_name("name")` | one string | `fn` marked `export` | 1 | §15.7 |
| `@import_name("name")` | one string | `extern fn` | 1 | §15.8 |
| `@repr(C)`, `@repr(I)` | `C`, or one integer type name | `record` (`C` only), `enum` | 0 | §15.9 |
| `@flags` | none | `enum` inside an interface | 4 | §15.10 |
| `@target(pred)` | one target predicate | module-level items except `interface` and `world` | 0 | §15.11 |
| `@cases(N)` | one integer | `property` | 3 | §15.12 |
| `@seed(N)` | one integer | `property` | 3 | §15.12 |
| `@doc(hidden)` | `hidden` | every target kind | 0 | §15.13 |
| `@panic_hook` | none | `extern fn` in an `extern "wasm"` block | 1 | §15.14 |

### 15.2.4 Validation

The attributes of one target are validated in the following order. Every failed check reports one diagnostic at the attribute or argument concerned and drops that attribute (or that argument, where stated); the remaining attributes are still validated and applied (P8).

1. **Name.** The name is one of §15.2.3, otherwise `UNKNOWN_ATTRIBUTE` ([03](03-grammar.md)). If exactly one attribute name is within edit distance 2 of the written name, the diagnostic carries a fix that replaces it ([impl/02](../impl/02-frontend-ast-hir.md) §2.10.1).
2. **Stage.** If the attribute's stage (§15.2.3) is above the stage the compiler implements, the attribute is `FEATURE_NOT_IMPLEMENTED` ([01](01-introduction.md) §1.4.5) with the feature `attribute @name`, and it is dropped. No later check runs for it.
3. **Target.** The target kind is one the attribute allows, otherwise `INVALID_ATTRIBUTE_TARGET`. Three cases have a more specific code, which is reported instead:
   - `@derive` on a `resource` is `DERIVE_NOT_SUPPORTED` ([05](05-types.md) §5.6.6);
   - `@repr` on a `record` or `enum` declared inside an interface is `WIT_INCOMPATIBLE_TYPE` ([13](13-components-and-wit.md) §13.2.2);
   - `@flags` on an `enum` declared outside an interface is `INVALID_FLAGS_ENUM` ([13](13-components-and-wit.md) §13.9.4).
4. **Arguments.** The arguments match the attribute's argument grammar and value ranges, otherwise `INVALID_ATTRIBUTE_ARGUMENT`. The diagnostic states the expected form.
5. **Duplicates.** An attribute name occurs at most once per target, except `@allow`, `@warn` and `@deny`, which may be repeated. A second occurrence is `DUPLICATE_ATTRIBUTE`, reported at the later one with a note at the earlier one; the later one is dropped. `@inline` and `@inline(never)` have the same name, so writing both is `DUPLICATE_ATTRIBUTE`.
6. **Repeated arguments.** An argument list that names the same entry twice is `DUPLICATE_ATTRIBUTE_ARGUMENT`, reported at the later entry, which is dropped. The lists concerned are the derive names of `@derive` (`@derive(Clone, Clone)`), the codes of the level attributes of one target that have the same level (`@allow(A, A)`, or `@allow(A)` and `@allow(A)` on the same target), and the operands of one `all`, `any` or `none` in `@target` that are written identically.
7. **Conflicts.** The following pairs exclude each other on one target. The attribute that comes later in source order is `CONFLICTING_ATTRIBUTES`, with a note at the earlier one, and is dropped.

   | Attribute | Excludes | Reason |
   |---|---|---|
   | `@inline` (without `never`) | `@cold` | a function cannot be both preferred for inlining and marked as rarely called |
   | `@flags` | `@derive` | a flags type has a fixed set of trait impls ([13](13-components-and-wit.md) §13.9.4) |

8. **Attribute-specific checks** in the section of each attribute.

The checks of steps 1–7 depend only on the target declaration and its attributes, so they are performed without name resolution ([impl/02](../impl/02-frontend-ast-hir.md) §2.10.2). Step 8 may need name resolution, types or the whole package; each section says what it needs.

### 15.2.5 Meaning, order and scope

1. **Order.** The order of the attributes on one target has no meaning. It affects only the order of their diagnostics. Two programs that differ only in attribute order are equivalent.
2. **Scope.** An attribute applies to its target only, with these exceptions:
   - a level attribute applies to every lint occurrence inside the syntax of its target, including nested declarations (§15.16.2);
   - `@target` removes its target together with everything inside it (§15.11);
   - `@doc(hidden)` on a declaration hides its members from documentation too (§15.13).

   In particular, `@deprecated` on a type does not deprecate its fields, variants or methods, and `@must_use` on a trait does not exist.
3. **Kinds of effect.** The attributes act at different levels. This classification is normative: an attribute has no effect other than those listed for it.

   | Effect | Attributes |
   |---|---|
   | which declarations exist | `@target` |
   | static semantics (types, impls, acceptance) | `@derive`, `@repr` (discriminant type), `@flags`, `@must_use` |
   | the interface of the artifact | `@export_name`, `@import_name`, `@panic_hook` |
   | the layout of values in memory | `@repr` |
   | code generation only, never the observable behavior | `@inline`, `@cold` |
   | diagnostics only | `@allow`, `@warn`, `@deny`, `@deprecated` |
   | test execution only | `@cases`, `@seed` |
   | documentation and tooling only | `@doc(hidden)` |

4. **Lint configuration never changes meaning** (package/01 §1.13). Whatever the levels, the artifacts of a successful build are identical. A lint at level `deny` can make a build fail; it cannot change what a successful build produces.

### 15.2.6 Attributes and generated or imported declarations

- Declarations generated by the compiler (derived impls, the SAI view of WIT items, desugarings) carry no attributes, except as stated in their owning document. The SAI view of a WIT item marked with the WIT gate `@deprecated` is a deprecated declaration (§15.6.2, [13](13-components-and-wit.md) §13.7.3).
- `Result` and the anonymous future types are must-use types by definition (D-EXPR-01, §15.5.1). Their declarations in [16](16-core-library.md) show `@must_use`.

---

## 15.3 `@derive`

### 15.3.1 Form

```ebnf
DeriveArgs ::= '(' DeriveName ( ',' DeriveName )* ','? ')'
DeriveName ::= UIDENT
```

1. `@derive` takes one or more derive names. `@derive` without arguments and `@derive()` are `INVALID_ATTRIBUTE_ARGUMENT`. An argument that is not a plain `UIDENT` (a string, an integer, a key-value pair, a name with parentheses, or a raw identifier) is `INVALID_ATTRIBUTE_ARGUMENT`.
2. The names are matched textually against the fixed list of [05](05-types.md) §5.12.1. They are never resolved in scope, so a local trait called `Clone` does not affect `@derive(Clone)`. A name that is not in the list is `UNKNOWN_DERIVE` ([05](05-types.md)); if exactly one fixed name is within edit distance 2, the diagnostic carries a fix that replaces it.
3. A name written twice is `DUPLICATE_ATTRIBUTE_ARGUMENT` (§15.2.4 step 6). Two `@derive` attributes on one declaration are `DUPLICATE_ATTRIBUTE`, with a machine-applicable fix that merges the lists.
4. The order of the names has no meaning. The generated impls are the same for every order.

### 15.3.2 Targets and meaning

- `@derive` is allowed on records, enums, distinct types and refined types, including records and enums declared inside an interface ([13](13-components-and-wit.md) §13.2.2). On a resource it is `DERIVE_NOT_SUPPORTED` ([05](05-types.md), D-OWN-06). On every other target kind, including aliases, traits and functions, it is `INVALID_ATTRIBUTE_TARGET`.
- `@derive` together with `@flags` is `CONFLICTING_ATTRIBUTES` (§15.2.4 step 7).
- What each derived impl is, which names are derivable for which declaration kind (`DERIVE_NOT_SUPPORTED`), and the checks on fields and supertraits (`DERIVE_FIELD_MISSING_IMPL`, `MISSING_SUPERTRAIT_IMPL`, `OVERLAPPING_IMPLS`) are defined by [05](05-types.md) §5.12.

```sai
@derive(Copy, Clone, PartialEq, Eq, Hash)
pub type Point {
    pub x: I32
    pub y: I32
}

@derive(Clone, Clone)                 // rejected: DUPLICATE_ATTRIBUTE_ARGUMENT
type Pair {
    a: I32
}

@derive(Debug)                        // rejected: UNKNOWN_DERIVE
type Named {
    name: String
}

@derive(Clone)                        // rejected: INVALID_ATTRIBUTE_TARGET
type Alias = Point
```

---

## 15.4 `@inline`, `@inline(never)` and `@cold`

### 15.4.1 Form and targets

1. `@inline` takes no arguments or the single argument `never`. Any other argument list, including `@inline()` and `@inline(always)`, is `INVALID_ATTRIBUTE_ARGUMENT`.
2. `@cold` takes no arguments. `@cold()` and `@cold(x)` are `INVALID_ATTRIBUTE_ARGUMENT`.
3. Both are allowed on a function that has a body: a `fn`, an `impl fn`, and a `trait fn` with a default body. On a `trait fn` without a body, an `extern fn`, an `interface fn`, or any other target kind they are `INVALID_ATTRIBUTE_TARGET`. Closures cannot carry attributes (§15.2.1).
4. `@inline` without `never` together with `@cold` is `CONFLICTING_ATTRIBUTES`. `@inline(never)` together with `@cold` is allowed.

### 15.4.2 Meaning

None of these attributes changes the observable behavior of a program: its results, its effects, its panics and traps, and its interaction with imports are the same with and without them. They may change code size, speed and stack use. They apply to every instantiation of a generic function. On a trait method they apply to its default body only; an impl that overrides the method does not inherit them.

- **`@inline`** is a hint that inlining calls of the function is profitable. An implementation MAY ignore it.
- **`@inline(never)`** is a requirement on the implementation: no call of the function is inlined. Every reachable instantiation of the function exists as a function of its own in the output, and every call of it, in the output, is a call of that function. Dead-code elimination still removes the function if it is not reachable ([impl/09](../impl/09-optimization-and-size.md)).
- **`@cold`** is a hint that the function is rarely called. An implementation SHOULD NOT inline it and MAY place the paths that lead to its calls out of line.

In profiles that emit a name section, a function with `@inline(never)` therefore always has its own entry in it, which makes the attribute useful for profiling and for stack traces of hosts.

```sai
@inline
fn square(x: I64) -> I64 {
    x * x
}

@cold
@inline(never)
fn report_bad_input(code: I32) -> I32 {
    code + 1000
}

@inline(always)                       // rejected: INVALID_ATTRIBUTE_ARGUMENT
fn cube(x: I64) -> I64 {
    x * x * x
}

@inline
@cold                                 // rejected: CONFLICTING_ATTRIBUTES
fn rare(x: I64) -> I64 {
    x - 1
}
```

---

## 15.5 `@must_use`

### 15.5.1 Form, targets and must-use types

1. `@must_use` takes no arguments, or one non-empty `STRING`, the **reason**. Any other argument list is `INVALID_ATTRIBUTE_ARGUMENT`.
2. It is allowed on functions (`fn`, `trait fn`, `impl fn`, `extern fn`, `interface fn`) and on records, enums, distinct types, refined types and resources. On every other target kind it is `INVALID_ATTRIBUTE_TARGET`. On an alias it is `INVALID_ATTRIBUTE_TARGET`, because an alias denotes its target type.

A type `T` is a **must-use type** if and only if one of the following holds:

1. `T` is an instance of a record, enum, distinct type or resource whose declaration carries `@must_use`. `Result<T, E>` is one ([16](16-core-library.md), D-EXPR-01).
2. `T` is a refined type whose declaration carries `@must_use`, or whose base type is a must-use type.
3. `T` is an anonymous future type: the type of a call of an `async fn` or of an `async` block ([11](11-async-and-concurrency.md) §11.2, §11.3.2, D-EXPR-01).

No other type is a must-use type. In particular, a tuple, an array, `Option<T>`, `Box<T>`, `borrow T` and a distinct type over a must-use base are not, whatever they contain (N-A-42). A named type that implements `Future` is a must-use type only if its declaration carries `@must_use`.

### 15.5.2 Must-use calls

A call expression `f(…)`, method call `e.m(…)` or path call `T.m(…)` ([06](06-expressions-and-statements.md)) is a **must-use call** if the function it resolves to carries `@must_use`, or if it resolves to a method of a trait impl whose method declaration in the trait carries `@must_use`. For a call of an `async fn` carrying `@must_use`, the expression `call.await` whose operand is that call is a must-use call too; the call itself already has a must-use type (rule 3).

Calls through a value of a function pointer or closure type are never must-use calls, because `@must_use` is not part of any type. Operator expressions are never must-use calls, even when they resolve to a trait method.

### 15.5.3 Discarding

[06](06-expressions-and-statements.md) §6.25.2 defines when a value is discarded. A discarded value whose type is neither `()` nor `Never` is the error `UNUSED_MUST_USE` ([06](06-expressions-and-statements.md), D-EXPR-01) if its type is a must-use type or if it is the value of a must-use call. The message includes the reason string of the attribute that applies, if any: the type's attribute for rule 1 and 2, the function's attribute for a must-use call (the function's reason first if both apply).

- `UNUSED_MUST_USE` is a hard error. It is not a lint, so `@allow(UNUSED_MUST_USE)` is `NOT_A_LINT` (§15.15.4).
- `let _ = e` and `let _: T = e` are the explicit discards (D-EXPR-01). Binding the value to a name that is never used is not a discard; it is `UNUSED_VARIABLE` (§15.18.1).
- `@must_use` on a function whose declared return type is `()` or `Never` has no effect and is the lint `UNUSED_ATTRIBUTE` (§15.18.6). So is `@must_use` on a function whose declared return type is a concrete must-use type, because the call is already reported through its type.

```sai
@must_use("the sum is the only output")
fn mix(a: U32, b: U32) -> U32 {
    (a +% b) ^ 0x5BD1
}

@must_use
type Ticket {
    id: U64
}

fn issue(id: U64) -> Ticket {
    Ticket(id: id)
}

fn caller() {
    mix(1, 2)                         // rejected: UNUSED_MUST_USE
    issue(7)                          // rejected: UNUSED_MUST_USE
    let _ = mix(1, 2)
    let _ = issue(8)
}
```

---

## 15.6 `@deprecated`

### 15.6.1 Form and targets

```ebnf
DeprecatedArgs ::= '(' Message ')'
                 | '(' Message ',' Instead ')'
                 | '(' Instead ')'
Message        ::= STRING                      (* non-empty *)
Instead        ::= 'instead' '=' STRING        (* a path, §15.6.3 *)
```

1. `@deprecated` takes no arguments or one of the three forms above. The message comes first. Any other argument list, an empty message, or an `instead` key written twice is `INVALID_ATTRIBUTE_ARGUMENT`.
2. It is allowed on: `fn`, `trait fn`, inherent impl fn, `extern fn`, `interface fn`, `record`, `enum`, `distinct`, `refined`, `alias`, `resource`, `trait`, `const`, `effect`, `interface`, `world`, `field`, `variant`, and an `assoc type` declared in a trait.
3. It is `INVALID_ATTRIBUTE_TARGET` on a trait impl fn, an interface impl fn and an `assoc type` defined in an impl (their deprecation follows the trait or interface), and on `use`, `impl`, `extern block`, `invariant`, `test` and `property` (they cannot be referred to by name).
4. `@deprecated` has no effect on acceptance, types, code generation or the artifact (§15.2.5). Its only effect is the lint `DEPRECATED_REFERENCE` (§15.6.4) and the documentation of the declaration ([impl/12](../impl/12-semantic-tooling.md)).

### 15.6.2 Deprecated declarations and deprecated contexts

A declaration is **deprecated** if and only if:

1. it carries a valid `@deprecated`, or
2. it is a declaration of the SAI view of a WIT item that carries the WIT gate `@deprecated` ([13](13-components-and-wit.md) §13.7.3). Its message is the one that [13](13-components-and-wit.md) derives from the gate. It has no `instead` path.

Deprecation is not inherited. The fields, variants, associated functions, methods and associated types of a deprecated type or trait are not deprecated unless they carry `@deprecated` themselves (§15.2.5 rule 2). Every use of them that also names the deprecated type is reported for the type (§15.6.4).

A **deprecated context** is a region of source in which no `DEPRECATED_REFERENCE` is reported. It is:

1. the whole syntax of a deprecated declaration: its doc comments, its attributes, its signature or definition, its clauses, its body, and the syntax of all its members;
2. the whole syntax of an `impl` block whose self type is a path to a deprecated record, enum, distinct type, refined type, alias or resource **of the same package**, or whose trait is a deprecated trait of the same package;
3. the whole syntax of an `invariant` whose type is a deprecated declaration of the same package.

Rules 2 and 3 exist because `@deprecated` cannot be written on an `impl` or an `invariant` (§15.6.1). The package that deprecates a type keeps its own impls and invariants of that type without warnings. An impl of a deprecated type or trait of another package is not a deprecated context, so its header is reported.

### 15.6.3 The `instead` path

The `instead` string names the declaration that replaces the deprecated one. Its value MUST match `Ident ( '.' Ident )*` exactly: identifiers of [02](02-lexical-structure.md) §2.4 separated by single dots, without white space, raw-identifier prefixes or package paths. It is resolved without local bindings and without generic parameters:

1. For a **member** (a field, a variant, a trait fn, an `assoc type` in a trait, an inherent impl fn, an `interface fn` or an interface resource function), a path with one segment names a sibling: a member of the same kind in the same record, enum, trait, inherent impl's self type, interface or interface resource. A field names a field, a variant a variant, a function a function of the same container, an `assoc type` an `assoc type`.
2. For every other declaration, and for a member with a path of two or more segments, the path is resolved as a qualified path ([04](04-modules-and-names.md) §4.5.2) whose first segment is looked up in the module scope `S(M)` and then the prelude ([04](04-modules-and-names.md) §4.5.1 steps 3 and 4), where `M` is the module that contains the deprecated declaration. Type-relative segments are resolved as [05](05-types.md) defines.

The path MUST resolve, after `@target` filtering, to a declaration that is not the deprecated declaration itself and is not deprecated. If the deprecated declaration is visible outside its package ([04](04-modules-and-names.md) §4.6), the replacement MUST be visible outside its package too. A violation of any of these rules is `INVALID_ATTRIBUTE_ARGUMENT` at the string, and the `instead` part is dropped; the message, if any, is kept. The check needs name resolution, so it is performed after it ([impl/03](../impl/03-name-resolution-and-type-checking.md)).

The replacement does not have to have the same kind, signature or type as the deprecated declaration. `@deprecated(instead = "Config.with_timeout")` on a function is valid.

### 15.6.4 The lint `DEPRECATED_REFERENCE`

`DEPRECATED_REFERENCE` (default `warn`) is reported at every **reference** to a deprecated declaration that is not inside a deprecated context. The references are:

1. a path in an expression, a type, a pattern, a bound, a `where` clause, an `effects` or `covers` clause, an `impl` header, a world reference, an `export world` of `sai.package`, or a `use` item, whose resolved entity is the deprecated declaration. The occurrence is reported at the path segment that resolves to it. Each segment of one path is checked, so `OldEnum.OldVariant` reports twice when both the enum and the variant are deprecated;
2. a method call `e.m(…)` or path call `T.m(…)` that resolves to a deprecated function, reported at `m`;
3. a field access `e.f`, and a field named in a record construction, a functional update or a record pattern, where the field is deprecated, reported at the field name;
4. a variant written as a path or as an implicit member `.V` ([06](06-expressions-and-statements.md)), in an expression or a pattern, where the variant is deprecated;
5. an associated type named in a path or bound in an associated type binding (`Item = T`), where the associated type is deprecated;
6. a level attribute or a `lints { }` entry is never a reference: codes are not declarations.

The following are not references:

- the declaration itself, its definition in an impl (a trait impl fn or an `assoc type` defined in an impl that implements a deprecated trait member), and its WIT view's generated code;
- names inside an `instead` string and inside doc comments;
- references that the compiler creates: derived impls, the desugarings of `for`, `?`, `.await`, operators and format strings ([06](06-expressions-and-statements.md)), and drop glue. Expressions that the user writes inside these constructs, such as the operands of an operator or the interpolated expressions of a format string, are references as usual;
- a `pub use` that re-exports a deprecated declaration of its own package. The re-exported binding denotes the same declaration ([04](04-modules-and-names.md) §4.3.5), so every reference through it is reported.

A plain `use` of a deprecated declaration is reported at the use item, and each reference through the binding is reported again, because each is a place that must change. References in the package that declares the deprecated item are reported as well; that package uses `@allow(DEPRECATED_REFERENCE)` or a deprecated context where it keeps using the item on purpose.

The message is "`<canonical path>` is deprecated", followed by `: <message>` when there is a message. If the declaration has an `instead` path, a help says "use `<instead>` instead". If the `instead` path has one segment and the source text of the reported token is the name of the deprecated declaration, the diagnostic carries a fix with applicability `maybe-incorrect` ([impl/11](../impl/11-diagnostics.md) §11.7.2) that replaces that token with the `instead` name. No fix is offered in other cases, because they can need new imports or different arguments.

```sai
@deprecated("computes the area of a rectangle only", instead = "area_of")
pub fn area(w: I64, h: I64) -> I64 {
    area_of(w, h)
}

pub fn area_of(w: I64, h: I64) -> I64 {
    w * h
}

pub type Config {
    @deprecated("no longer read")
    pub retries: U32
    pub timeout_ms: U32
}

pub fn twelve() -> I64 {
    area(3, 4)                            // lint: DEPRECATED_REFERENCE
}

pub fn budget(c: borrow Config) -> U32 {
    c.retries + c.timeout_ms              // lint: DEPRECATED_REFERENCE
}

@deprecated
pub fn old_twelve() -> I64 {
    area(3, 4)
}

@deprecated("")                           // rejected: INVALID_ATTRIBUTE_ARGUMENT
pub fn empty_message() {
}

@deprecated(instead = "no_such_fn")       // rejected: INVALID_ATTRIBUTE_ARGUMENT
pub fn dangling() {
}
```

The call of `area` in `old_twelve` is not reported, because `old_twelve` is a deprecated context.

---

## 15.7 `@export_name`

1. `@export_name` takes exactly one non-empty `STRING`, the **export name**. Any other argument list is `INVALID_ATTRIBUTE_ARGUMENT`.
2. It is allowed only on a `fn` marked `export` ([03](03-grammar.md) §3.3). On a `fn` without `export`, and on every other target kind, it is `INVALID_ATTRIBUTE_TARGET`. Functions exported through a world get their names from WIT ([13](13-components-and-wit.md)); they are not `export fn` items.
3. The **export name** of an `export fn` is the string of its `@export_name`, or else its name without a raw-identifier prefix `r#` ([04](04-modules-and-names.md) §4.6.6, [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.6.7). Any string is a valid core Wasm export name, so there is no further restriction on its content.
4. `@export_name` whose string is equal to the function's name is the lint `UNUSED_ATTRIBUTE` (§15.18.6), with a machine-applicable fix that removes it.

**Uniqueness.** Export names are checked in the root package of a build whose artifact is a core module, after `@target` filtering ([04](04-modules-and-names.md) §4.6.6):

- Two `export fn` items with the same export name are `DUPLICATE_EXPORT_NAME`. The items are ordered by the module path of their module (byte order of the canonical path, [04](04-modules-and-names.md) §4.9) and then by source position. Every item after the first with a given export name is reported, at its `@export_name` string or, without one, at its name, with a note at the first.
- The export name `memory` is reserved, whether or not the module has a memory ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.10.5). An `export fn` whose export name is `memory` is `DUPLICATE_EXPORT_NAME`, with a note that the memory is exported under that name.
- Comparison is by the bytes of the UTF-8 strings. No normalization is applied, so `"é"` written precomposed and decomposed are two names. The export names of functions are compared with the NFC-normalized identifier ([02](02-lexical-structure.md) §2.4) when no `@export_name` is written.

In a package that is not the root package, `export` behaves like `pub` and `@export_name` has no effect and no uniqueness check (N-A-15). In a component build, `export fn` items are not exported (`EXPORT_FN_NOT_APPLIED`, [13](13-components-and-wit.md) §13.6.4), so `@export_name` has no effect and no uniqueness check either. The signature rules of core exports are in [12](12-unsafe-and-ffi.md) and D-WASM-04.

```sai
@export_name("sai_add")
export fn add(a: I32, b: I32) -> I32 {
    a + b
}

@export_name("sai_add")                   // rejected: DUPLICATE_EXPORT_NAME
export fn add_wrapping(a: I32, b: I32) -> I32 {
    a +% b
}

export fn memory() -> I32 {               // rejected: DUPLICATE_EXPORT_NAME
    0
}

@export_name("helper")                    // rejected: INVALID_ATTRIBUTE_TARGET
pub fn helper() -> I32 {
    1
}
```

The example assumes that it is the root package of a core-module build.

---

## 15.8 `@import_name`

1. `@import_name` takes exactly one non-empty `STRING`. Any other argument list is `INVALID_ATTRIBUTE_ARGUMENT`.
2. It is allowed only on an `extern fn`, in an `extern "wasm"` block or an `unsafe extern "C"` block. On every other target kind it is `INVALID_ATTRIBUTE_TARGET`.
3. In an `extern "wasm"` block, the string is the **import field name** of the function, which is otherwise its name without `r#`. The import module name is the string after `module` ([03](03-grammar.md) §3.10). The rules for import names, including when two declarations denote one import and `CONFLICTING_WASM_IMPORT`, are in [12](12-unsafe-and-ffi.md) §12.7.6.
4. In an `unsafe extern "C"` block (Stage 7), the string is the linker symbol name of the function ([12](12-unsafe-and-ffi.md)).
5. The function keeps its SAI name. SAI code calls it by that name; `@import_name` changes only the name in the artifact.
6. `@import_name` whose string is equal to the function's name is the lint `UNUSED_ATTRIBUTE` (§15.18.6), with a machine-applicable fix that removes it.

```sai
extern "wasm" module "env" {
    @import_name("now-ms")
    fn now_ms() -> I64

    @import_name("log") // lint: UNUSED_ATTRIBUTE
    fn log(code: I32)
}
```

---

## 15.9 `@repr`

### 15.9.1 Form and targets

```ebnf
ReprArgs ::= '(' ReprKind ')'
ReprKind ::= 'C' | 'I8' | 'I16' | 'I32' | 'I64' | 'U8' | 'U16' | 'U32' | 'U64'
```

1. `@repr` takes exactly one argument, written as a plain `UIDENT`: `C`, or the name of a fixed-width integer type. The argument is matched textually and is never resolved, so a local type named `C` does not affect it. `ISize` and `USize` are not accepted, because their width depends on the target; `Byte`, `Bool`, `Char` and every other name are not accepted either. `@repr`, `@repr()`, `@repr(C, U8)` and these names are `INVALID_ATTRIBUTE_ARGUMENT`.
2. It is allowed on a `record` and on an `enum`, at module level. On a record only `C` is accepted; `@repr(U8)` on a record is `INVALID_ATTRIBUTE_ARGUMENT`. On an enum, `C` and every integer type are accepted, for unit-only enums and for enums with payloads alike.
3. On a record or enum declared inside an interface it is `WIT_INCOMPATIBLE_TYPE` ([13](13-components-and-wit.md) §13.2.2), because the layout of a WIT type is fixed by the Canonical ABI. On every other target kind, including `distinct`, `refined`, `alias` and `resource`, it is `INVALID_ATTRIBUTE_TARGET`. A distinct type and a refined type have the layout of their base type ([05](05-types.md)).
4. `@repr` on an enum without variants has no effect, because the enum has no values, and is the lint `UNUSED_ATTRIBUTE` (§15.18.6).

### 15.9.2 Meaning

`@repr` has two effects (§15.2.5):

1. **Discriminant type.** The discriminant type of an enum ([05](05-types.md) §5.6.2) is the integer type `I` for `@repr(I)`, `I32` for `@repr(C)`, and `I64` without `@repr`. Every discriminant, explicit or implicit, MUST be in the range of that type, otherwise `DISCRIMINANT_OUT_OF_RANGE` ([05](05-types.md)). `I32` for `@repr(C)` is the type of the enumeration constants of C (N-A-43).
2. **Layout.** The layout of a value is compiler-private without `@repr` (D-WASM-03). With `@repr` it is fixed by the rules of [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.4, which every implementation MUST follow for these types, because unsafe code and `extern "C"` functions depend on it ([12](12-unsafe-and-ffi.md)):
   - `@repr(C)` on a record: the fields are laid out in declaration order, each at the smallest offset that is at least the end of the previous field and a multiple of the field's alignment; the alignment is the largest field alignment, and the size is the end of the last field rounded up to the alignment (D-WASM-03).
   - `@repr(C)` on an enum: always a tag of 4 bytes followed by the payloads, each payload laid out like a `@repr(C)` record of its fields. No niche is used ([impl/06](../impl/06-wasm-lowering-and-layout.md) §6.4.12).
   - `@repr(I)` on an enum: always a tag of type `I` followed by the payloads. No niche is used.

   A field whose type has a compiler-private layout still has that layout inside a `@repr` type. Only the arrangement of the `@repr` type itself is fixed.

`@repr` does not change the meaning of any expression. Values, patterns, equality, derived impls and conversions are the same with and without it, except that the discriminant values are those of the discriminant type.

```sai
@repr(U8)
enum Opcode {
    Nop
    Push = 10
    Pop
}

@repr(C)
type Header {
    tag: U8
    len: U32
}

@repr(U8)
enum Wide {
    Small = 1
    Large = 300                           // rejected: DISCRIMINANT_OUT_OF_RANGE
}

@repr(U16)                                // rejected: INVALID_ATTRIBUTE_ARGUMENT
type Pair {
    a: U16
    b: U16
}

@repr(USize)                              // rejected: INVALID_ATTRIBUTE_ARGUMENT
enum Size {
    One
    Two
}
```

---

## 15.10 `@flags`

1. `@flags` takes no arguments. `@flags()` and any argument are `INVALID_ATTRIBUTE_ARGUMENT`.
2. It is allowed only on an `enum` declared inside an interface. On an enum outside an interface it is `INVALID_FLAGS_ENUM` ([13](13-components-and-wit.md) §13.9.4). On every other target kind it is `INVALID_ATTRIBUTE_TARGET`.
3. `@flags` together with `@derive` is `CONFLICTING_ATTRIBUTES` (§15.2.4 step 7). A flags type has the fixed set of impls and members that [13](13-components-and-wit.md) §13.9.4 lists.
4. The shape of a flags enum (at most 32 unit variants without discriminants, `INVALID_FLAGS_ENUM`; no variants at all is `WIT_EMPTY_TYPE`), the meaning of a flags type, and the rejection of variant patterns (`FLAGS_PATTERN`) are defined by [13](13-components-and-wit.md) §13.9.4 (N-D-38).

---

## 15.11 `@target`

### 15.11.1 Form

```ebnf
TargetArgs ::= '(' Pred ')'
Pred       ::= STRING                                     (* a target name *)
             | LIDENT                                     (* a target capability *)
             | ( 'all' | 'any' | 'none' ) '(' Pred ( ',' Pred )* ','? ')'
```

1. `@target` takes exactly one predicate. `@target`, `@target()` and `@target(a, b)` are `INVALID_ATTRIBUTE_ARGUMENT`; two conditions are combined with `all` or `any`.
2. A `STRING` is a **target name**. Its value MUST be a target name of the [targets doc](../targets/targets-and-profiles.md) (`wasm32-core`, `wasm64-core`, `wasm32-wasi`, `wasm64-wasi` in v0.1), otherwise `UNKNOWN_TARGET` ([package/01](../package/01-package-contract.md)). Target names are strings because they contain `-`, which is not an identifier character.
3. A plain `LIDENT` without parentheses is a **target capability**. It MUST be one of `threads`, `host`, `native_ffi`, `process` and `memory64` ([08](08-effects-and-capabilities.md) §8.12.1), otherwise `INVALID_ATTRIBUTE_ARGUMENT`. `alloc`, WIT capabilities and profile names are not predicates. The note of the diagnostic says that a predicate can test only the target and its capabilities.
4. `all(…)`, `any(…)` and `none(…)` take one or more predicates. An empty list, a combinator without parentheses, a key-value pair, an `INT`, and any other identifier with parentheses are `INVALID_ATTRIBUTE_ARGUMENT`. The negation is spelled `none(p)` because `not` is a strict keyword ([02](02-lexical-structure.md) §2.5.1).
5. Two operands of one combinator that are written identically are `DUPLICATE_ATTRIBUTE_ARGUMENT` (§15.2.4 step 6); the later one is dropped, which does not change the value.

An invalid `@target` is dropped like every invalid attribute (§15.1.1), so its item is present for every target.

### 15.11.2 Evaluation

Every command that checks or builds code does so for exactly one target ([impl/13](../impl/13-cli.md) §13.4.1). A predicate is evaluated against that target and against the kind of artifact the command produces:

| Predicate | Holds when |
|---|---|
| `"name"` | the target of the command is `name` |
| `c` | the build provides the target capability `c` ([08](08-effects-and-capabilities.md) §8.12.7). `host` holds only when the artifact is a core module, so it is false in `sai component build` for a core target. `memory64` holds for `wasm64-core` and `wasm64-wasi`. `threads` holds for no target. `native_ffi` and `process` hold only for the native targets ([targets](../targets/targets-and-profiles.md) §2.8, §3.1), so they are false for every Wasm target and for every build of a compiler without Stage 7 |
| `all(p, …)` | every operand holds |
| `any(p, …)` | at least one operand holds |
| `none(p, …)` | no operand holds |

A command that checks code without producing an artifact (`sai check`, the language server) evaluates `host` for the artifact that `sai build` would produce for the same target ([impl/13](../impl/13-cli.md)). The predicate does not depend on the profile, the optimization level, the package or the command otherwise (N-A-44).

### 15.11.3 Targets and effect

1. `@target` is allowed on module-level items: `fn`, `record`, `enum`, `distinct`, `refined`, `alias`, `resource`, `trait`, `impl`, `const`, `invariant`, `effect`, `extern block`, `use`, `test` and `property`. On an `interface`, a `world` and every member (field, variant, trait or impl item, interface or extern item) it is `INVALID_ATTRIBUTE_TARGET` (N-A-44).
2. The predicate is evaluated during item collection, before any other check of the package ([04](04-modules-and-names.md) §4.5.4 phase 1, [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.3.1). An item whose predicate is false does not exist in the build: it binds no name, takes part in no duplicate check, coherence check, reachability or lint, and produces no diagnostic except those of lexing and parsing, which include `UNKNOWN_ATTRIBUTE`. The formatter and the syntax-based tools still see it ([impl/02](../impl/02-frontend-ast-hir.md) §2.10.2).
3. Everything inside a removed item is removed with it: the items of an `impl`, the functions of an `extern` block, the methods and associated types of a trait.
4. An item whose predicate is true, or that has no `@target`, is checked as usual. A reference to a removed item is an ordinary unresolved name (`UNRESOLVED_NAME`, [04](04-modules-and-names.md)), so the code that uses a target-specific item is made target-specific itself.
5. Two items with the same name are allowed if, for the target being checked, at most one of them exists ([04](04-modules-and-names.md) §4.4.3). There is no check across targets: each target is checked separately.

A program is therefore conforming for some targets and not for others. This is one of the explicit target dependences of [01](01-introduction.md) §1.4.1 rule 3.

```sai
@target(host)
extern "wasm" module "env" {
    fn now_ms() -> I64
}

@target(host)
pub fn now() -> I64
    effects { host }
{
    now_ms()
}

@target(none(host))
pub fn now() -> I64 {
    0
}

@target(any("wasm64-core", "wasm64-wasi"))
pub const WORD_BYTES: USize = 8

@target(none(memory64))
pub const WORD_BYTES: USize = 4

@target("wasm32-linux")                   // rejected: UNKNOWN_TARGET
pub fn native_only() {
}

@target(release)                          // rejected: INVALID_ATTRIBUTE_ARGUMENT
pub fn fast() {
}
```

The first `now` uses the import and requires the `host` capability ([08](08-effects-and-capabilities.md)). In a component build it does not exist, and the second `now` is used instead. `@target(memory64)` and `@target(any("wasm64-core", "wasm64-wasi"))` hold for the same v0.1 targets.

---

## 15.12 `@cases` and `@seed`

1. `@cases(N)` takes exactly one `INT` in the range `1 ..= 2^32 − 1`. `@seed(N)` takes exactly one `INT` in the range `0 ..= 2^64 − 1`. Any other argument, and a value outside the range, is `INVALID_ATTRIBUTE_ARGUMENT`. Both accept every form of integer literal, so `@seed(0xC0FFEE)` is valid.
2. Both are allowed only on a `property` ([03](03-grammar.md) §3.11). On a `test` and on every other target kind they are `INVALID_ATTRIBUTE_TARGET`.
3. `@cases(N)` sets the number of generated cases of the property, and `@seed(N)` the seed of its generator. Their exact meaning and the defaults without them are defined by [14](14-testing.md).
4. `@seed` takes precedence over the `--seed` option of `sai test`, which applies only to properties without `@seed` ([impl/13](../impl/13-cli.md) §13.6.6). A property with `@seed` therefore runs the same cases in every run.
5. They affect only the execution of tests. They have no effect on any build artifact.

```sai
@cases(500)
@seed(42)
property "addition commutes" (a: I32, b: I32) {
    assert a +% b == b +% a
}

@cases(0)                                 // rejected: INVALID_ATTRIBUTE_ARGUMENT
property "never runs" (a: I32) {
    assert a == a
}
```

---

## 15.13 `@doc(hidden)`

1. `@doc` takes exactly the single argument `hidden`. `@doc`, `@doc()`, `@doc("text")` and every other argument are `INVALID_ATTRIBUTE_ARGUMENT`. Documentation text is written in doc comments ([02](02-lexical-structure.md)), never in an attribute.
2. It is allowed on every target kind.
3. A declaration with `@doc(hidden)` and all its members are omitted from the documentation generated for the package and from the completion lists offered in other packages ([impl/12](../impl/12-semantic-tooling.md)). Inside its own package the declaration is completed as usual.
4. `@doc(hidden)` changes nothing else. The declaration keeps its visibility, can be named from other packages, is part of the public API for the checks of [04](04-modules-and-names.md) §4.6.7 and for compatibility checks ([impl/12](../impl/12-semantic-tooling.md)), and keeps its WIT form inside an interface.
5. `@doc(hidden)` on a declaration that is not visible outside its package ([04](04-modules-and-names.md) §4.6), or on a member of a declaration that already carries `@doc(hidden)`, has no effect and is the lint `UNUSED_ATTRIBUTE` (§15.18.6).

```sai
@doc(hidden)
pub fn internal_checksum(x: U32) -> U32 {
    x ^ 0xFFFF
}

pub type Counter {
    @doc(hidden) // lint: UNUSED_ATTRIBUTE
    count: U32
}
```

The field `count` is not `pub`, so it is not visible outside the package and `@doc(hidden)` has no effect on it.

---

## 15.14 `@panic_hook`

`@panic_hook` marks a function of an `extern "wasm"` block as the reporting channel of panics in a core-module build (D-ERR-01). Its rules are defined by [10](10-errors-and-panics.md) §10.6:

1. It takes no arguments (`INVALID_ATTRIBUTE_ARGUMENT`).
2. It is allowed only on an `extern fn` in an `extern "wasm"` block. On a function in an `unsafe extern "C"` block, a function with a body, and every other target kind, it is `INVALID_ATTRIBUTE_TARGET`.
3. The attribute-specific checks of §15.2.4 step 8 are those of [10](10-errors-and-panics.md) §10.6.1 rules 3 to 5: the signature (`PANIC_HOOK_SIGNATURE_MISMATCH`), the root package (`PANIC_HOOK_OUTSIDE_ROOT`) and uniqueness in the build (`DUPLICATE_PANIC_HOOK`).
4. When the hook is used, how it is called, and its effect on reachability and capabilities are defined by [10](10-errors-and-panics.md) §10.6.2–§10.6.4. In a component build and in a `tiny` build the declaration is accepted and the hook is never called.

---

## 15.15 Lints and lint levels

### 15.15.1 Lints and hard diagnostics

Every diagnostic code is either a hard diagnostic or a lint ([impl/11](../impl/11-diagnostics.md) §11.2.3). A **lint** reports a condition in a well-formed program that is probably a mistake, a hazard or a departure from the conventions of the language. A program with lint occurrences is conforming whatever their levels ([01](01-introduction.md) §1.4.1). A lint whose effective level is `deny` is emitted as an error and makes the command fail ([impl/11](../impl/11-diagnostics.md) §11.3.1), but the program is not ill-formed, and no other diagnostic and no artifact depends on it.

The lints of v0.1 are exactly those of the registry in §15.17.2. Every other code is a hard diagnostic. For the codes listed there, the registry is authoritative over the severity column of the owning document.

### 15.15.2 Levels

1. The levels are `allow < info < warn < deny` ([impl/11](../impl/11-diagnostics.md) §11.3.2). Their mapping to severities is [impl/11](../impl/11-diagnostics.md) §11.3.3: `allow` is not emitted, `info` is an info, `warn` a warning and `deny` an error.
2. `info` is a default level only. There is no `@info` attribute, no `info` entry in `lints { }` and no `--info` flag. An info-default lint can be raised to `warn` or `deny`, or silenced with `allow`.
3. There is no level above `deny` that forbids lowering it (no `forbid`). A promise that cannot be overridden is made with `policy` (§15.16.5), which exists for exactly the lints where such a promise is part of a package's contract (N-A-45).

### 15.15.3 Level attributes

```ebnf
LevelArgs ::= '(' LintName ( ',' LintName )* ','? ')'
LintName  ::= Ident
```

1. `@allow`, `@warn` and `@deny` each take one or more lint names. Without arguments, and with an empty list, they are `INVALID_ATTRIBUTE_ARGUMENT`. An argument that is not a plain identifier (a string, an integer, a key-value pair, a name with parentheses, or a raw identifier) is `INVALID_ATTRIBUTE_ARGUMENT`; only that argument is dropped.
2. They are allowed on every target kind (§15.2.3), may be repeated on one target, and may be combined with each other.
3. A name given twice at the same level on one target, in one attribute or in two, is `DUPLICATE_ATTRIBUTE_ARGUMENT`, reported at the later one, which is dropped.
4. A name given at two different levels on one target is `CONFLICTING_LINT_LEVEL` ([package/01](../package/01-package-contract.md) §1.13), reported at the later one in source order with a note at the earlier one. The later one is dropped. The code is the one that the `lints { }` block uses for the same conflict (N-A-46).
5. Each name is checked by §15.15.4.

### 15.15.4 Lint names

Level attributes, the entries of `lints { }` ([package/01](../package/01-package-contract.md) §1.13) and the CLI lint flags ([impl/13](../impl/13-cli.md) §13.4.4) accept the same names, and the same checks apply to all three ([impl/11](../impl/11-diagnostics.md) §11.12.3):

1. A name is matched exactly, with case, against the codes of the diagnostic registry ([impl/11](../impl/11-diagnostics.md) §11.2.4).
2. A name that is not a code is `UNKNOWN_LINT` (error). The diagnostic offers the lint codes within edit distance 2 as candidates. If the name written in upper case is a lint code (`@allow(unused_variable)`), it carries a machine-applicable fix that replaces it. The name is dropped.
3. A name that is the code of a hard diagnostic is `NOT_A_LINT` (error), with the help of [impl/11](../impl/11-diagnostics.md) §11.12.3. The name is dropped. `@allow(UNUSED_MUST_USE)` is an example (§15.5.3). This also holds for an entry of `lints { }`: the hard code is `NOT_A_LINT`, not `UNKNOWN_LINT`.
4. A name whose registry status is `deprecated` is `DEPRECATED_DIAGNOSTIC_CODE` (warning), and the level applies to it and to every code of its `superseded_by` list. A `removed` name is `DEPRECATED_DIAGNOSTIC_CODE`, and the level has no effect ([impl/11](../impl/11-diagnostics.md) §11.12.2). No v0.1 code is deprecated or removed.

These checks are made in every package whose source is compiled, including dependencies, because they are hard diagnostics. A package that names a lint introduced by a newer toolchain therefore requires that toolchain ([impl/11](../impl/11-diagnostics.md) §11.12.3).

### 15.15.5 No lint groups

Every lint name denotes exactly one code. There are no lint groups, no wildcard names and no name that denotes "all lints" (N-A-47). A group would change its members when a toolchain adds a lint, so `@deny(GROUP)` would reject code that an earlier toolchain accepted (P6), and the effect of an attribute would not be visible from its text (P8). To change the level of several lints, each is named.

---

## 15.16 Level resolution

### 15.16.1 Level sources

The effective level of a lint occurrence is chosen from these sources ([impl/11](../impl/11-diagnostics.md) §11.3.4):

| Source | Written as | Applies to occurrences in | Section |
|---|---|---|---|
| `default` | the registry | every package | §15.17.2 |
| `manifest` | `lints { }` of the root package's `sai.package` | the root package and the workspace members | §15.16.3 |
| `cli` | `--allow`, `--warn`, `--deny` | the root package and the workspace members | §15.16.4 |
| `attribute` | `@allow`, `@warn`, `@deny` | the syntax of the target | §15.16.2 |
| `policy` | `policy { deny … }` of a package | that package's own code (`duplicate_lines`: the whole graph) | §15.16.5 |

### 15.16.2 Scope of level attributes

The **syntax range** of a target is the source range from the first token of its outer doc comments, or of its first attribute if it has no doc comments, to the last token of the declaration. The ranges of declarations are nested: the range of a field lies inside that of its record, that of a method inside that of its `impl`, and so on.

1. A valid level attribute naming code `C` on target `D` **applies** to an occurrence of `C` if the occurrence's primary span ([impl/11](../impl/11-diagnostics.md) §11.4.2) lies within the syntax range of `D`.
2. If several level attributes for `C` apply, the one on the innermost target applies. There is at most one per target (§15.15.3), so the choice is unique.
3. An attribute naming a deprecated code applies to the codes of its `superseded_by` list too (§15.15.4 rule 4), but an attribute on the same target that names such a code directly takes precedence.
4. A level attribute applies only to occurrences in the file that contains it. An occurrence whose primary span is in `sai.package`, in `sai.lock`, or that has no location, has no applicable attribute.
5. An occurrence in a declaration that the compiler generates has the primary span that the owning document gives it, for example the `@derive` attribute of a derived impl, and attributes apply to that span.
6. The level attributes of a declaration removed by `@target` (§15.11) do not exist.

A level attribute does not apply to diagnostics about itself: an `UNKNOWN_LINT` in `@allow(…)` is a hard error, and hard diagnostics have no level.

### 15.16.3 The `lints` block

The `lints { }` block of `sai.package` is root-only ([package/01](../package/01-package-contract.md) §1.13). The block of the root package applies to every occurrence in the root package and in the workspace members ([impl/13](../impl/13-cli.md) §13.2.2). The blocks of other packages, including workspace members, are ignored. Its entries are checked by §15.15.4. Two entries for one code with different levels are `CONFLICTING_LINT_LEVEL`, and with the same level `DUPLICATE_LIST_ITEM` ([package/01](../package/01-package-contract.md)).

### 15.16.4 Command-line flags

The flags `--allow`, `--warn` and `--deny` take one name each and are repeatable ([impl/13](../impl/13-cli.md) §13.4.4). Their names are checked by §15.15.4. Two flags with different levels for one code are `CONFLICTING_OPTIONS` ([impl/13](../impl/13-cli.md)); the order of the flags never matters. They apply to the root package and the workspace members. `--deny-warnings` is not a level source; it is applied after resolution (§15.16.7).

### 15.16.5 `policy`

The items of `policy { }` ([package/01](../package/01-package-contract.md) §1.12) that concern lints set the level `deny` with the source `policy`:

| Policy item | Lint | Applies to |
|---|---|---|
| `deny ambient_imports` | `AMBIENT_IMPORT` | occurrences in the source of the package that declares the item |
| `deny runtime_contracts` | `CONTRACT_RUNTIME_CHECK` | occurrences in the source of the package that declares the item |
| `deny duplicate_lines` | `DUPLICATE_PACKAGE_LINE` | every occurrence in the build; only the root package's item is in force |

The package-local items apply whenever the package is compiled, including as a dependency ([package/01](../package/01-package-contract.md) §1.12 rule 3). The item `deny unsafe` is not a lint level: it makes unsafe constructs the hard error `UNSAFE_DENIED` ([package/01](../package/01-package-contract.md)).

### 15.16.6 Precedence

The effective level of an occurrence of code `C` is given by the first of these rules that applies (N-A-48):

1. **`policy`**: a policy item that applies to the occurrence sets `deny`.
2. **`attribute`**: the level attribute that applies by §15.16.2.
3. **`cli`**: the level of the flag for `C`, if the occurrence is in the root package or a workspace member.
4. **`manifest`**: the level of the entry for `C` in the root package's `lints { }`, if the occurrence is in the root package or a workspace member.
5. **`default`**: the default level of `C` (§15.17.2).

The order expresses who makes the statement. A policy is a promise that the package makes about itself, so nothing lowers it. An attribute is written by the author at the code it concerns, so it is the most specific statement. A flag is the choice of one invocation and is more specific than the package-wide `lints` block. The result is always exactly one level, and it depends only on the source, the manifests and the command line ([impl/11](../impl/11-diagnostics.md) §11.3.2).

The source recorded in the diagnostic ([impl/11](../impl/11-diagnostics.md) §11.3.4) is the source of the rule that decided, and its location is that of the attribute, the `lints` entry or the `policy` item.

### 15.16.7 After resolution

The effective level is turned into a severity by [impl/11](../impl/11-diagnostics.md) §11.3.3. Two rules then apply, in this order:

1. **Dependency cap.** An occurrence in a package that is neither the root package nor a workspace member is dropped, whatever its level, unless its level source is `policy` (N-H-4).
2. **`--deny-warnings`.** Every remaining warning becomes an error, lints and hard warnings alike.

Level attributes in a dependency are therefore observable only through policy items, which they cannot override.

### 15.16.8 Ineffective level attributes

A level attribute names a code for which it can never apply in the following cases. Each is the lint `UNUSED_ATTRIBUTE` (§15.18.6), reported at the name, with a machine-applicable fix that removes the name (or the attribute, if it is the only name):

1. it sets `allow` or `warn` for a code that a policy item of the same package sets to `deny` (§15.16.5), because policy takes precedence;
2. it names `DUPLICATE_PACKAGE_LINE`, whose occurrences never have a primary span in a SAI source file.

A level attribute is not reported because the code did not occur in its range. Whether a lint occurs depends on the code inside the range, and an `@allow` that is not needed today is harmless.

### 15.16.9 Example

```sai
pub type Counter {
    count: U32
}

@deny(UNUSED_VARIABLE)
impl Counter {
    pub fn add(self: borrow mut Self, n: U32) {
        self.count += n
    }

    @allow(UNUSED_VARIABLE)
    pub fn reset(self: borrow mut Self, reason: U32) {
        self.count = 0
    }

    pub fn clear(self: borrow mut Self, reason: U32) {   // rejected: UNUSED_VARIABLE
        self.count = 0
    }

    @warn(UNUSED_VARIABLE)
    @deny(UNUSED_VARIABLE)                // rejected: CONFLICTING_LINT_LEVEL
    pub fn get(self: borrow Self) -> U32 {
        self.count
    }
}

@allow(UNUSED_MUST_USE)                   // rejected: NOT_A_LINT
@allow(UNUSED_VARIABLES)                  // rejected: UNKNOWN_LINT
pub fn noop() {
}
```

In `clear`, the innermost applicable attribute is the `@deny` on the `impl`, so the unused parameter is an error. In `reset`, the `@allow` on the method is innermost. With `sai check --allow UNUSED_VARIABLE`, `clear` is still rejected, because an attribute takes precedence over a flag.

---

## 15.17 The lint registry

### 15.17.1 Which codes are lints

A warning or info about a program is a lint if the user can remove its cause by changing the source code it points to, and the condition does not depend on decisions that are private to the implementation. It is a hard diagnostic if it reports:

1. something that the user cannot change in their own source, such as a name collision inside imported WIT;
2. a fact about the toolchain, the manifest, the lock file, the resolved graph or the build configuration, or a property that depends on compiler-private choices such as layout or frame size; or
3. a construct whose meaning the compiler has changed, such as a doc comment in an invalid position that is read as an ordinary comment. Silencing such a diagnostic would hide a change of meaning.

`DUPLICATE_PACKAGE_LINE` is a lint although it is a fact about the resolved graph, because `policy { deny duplicate_lines }` sets its level ([package/01](../package/01-package-contract.md) §1.12) and only lints have levels. Every error that a rule of the specification states is hard ([impl/11](../impl/11-diagnostics.md) §11.2.3). The rule of this section decides the class of every v0.1 warning and info, and the results are the lists of §15.17.2 and §15.17.3 (N-A-49).

### 15.17.2 The lints of v0.1

The following table is the complete list of lints in v0.1. "Owner" is the document that defines when the lint is reported; the default level and the class are defined here.

| Code | Default | Owner | Stage | Summary |
|---|---|---|---|---|
| `NON_ASCII_IDENTIFIER` | `warn` | [02](02-lexical-structure.md) §2.4 | 0 | an identifier contains non-ASCII characters |
| `UNNECESSARY_RAW_IDENT` | `warn` | [02](02-lexical-structure.md) §2.4 | 0 | `r#` on a name that is not a keyword |
| `UNUSED_IMPORT` | `warn` | [04](04-modules-and-names.md) §4.3.6 | 0 | a non-`pub` `use` binding is never referenced |
| `REDUNDANT_IMPORT` | `warn` | [04](04-modules-and-names.md) §4.3.6 | 0 | a `use` binding denotes what its name already denotes |
| `SHADOWS_PRELUDE` | `warn` | [04](04-modules-and-names.md) §4.5.1 | 0 | a module-level binding shadows a prelude name |
| `IRREFUTABLE_CONDITION_PATTERN` | `warn` | [06](06-expressions-and-statements.md) §6.14.2, §6.16.2 | 0 | `if let` or `while let` with an irrefutable pattern |
| `REDUNDANT_BINDING_MODE` | `warn` | [06](06-expressions-and-statements.md) §6.21.3 | 0 | a binding mode that has no effect, such as a `borrow mut` rest binding on a `SliceMut` |
| `REDUNDANT_UPDATE_BASE` | `warn` | [06](06-expressions-and-statements.md) §6.18.1 | 0 | `..base` in a record construction that names every field |
| `STATEMENT_HAS_NO_EFFECT` | `warn` | [06](06-expressions-and-statements.md) §6.25.2 | 0 | a discarded expression without side effects |
| `UNREACHABLE_CODE` | `warn` | [06](06-expressions-and-statements.md) §6.26.2, [10](10-errors-and-panics.md) §10.4.6 | 0 | code after a diverging expression |
| `UNREACHABLE_PATTERN` | `warn` | [06](06-expressions-and-statements.md) §6.22 | 0 | a match arm or alternative that can never match |
| `REDUNDANT_PROVENANCE` | `warn` | [07](07-ownership-and-borrowing.md) §7.7.3 | 2 | a `borrows` entry that duplicates or is subsumed by another |
| `UNUSED_UNSAFE` | `warn` | [12](12-unsafe-and-ffi.md) §12.2.1, §12.2.4 | 2 | an `unsafe` marker or block that is not needed |
| `MISSING_SAFETY_DOC` | `warn` | [12](12-unsafe-and-ffi.md) §12.2.2, §12.2.3 | 2 | a `pub` unsafe function or trait without a `# Safety` section |
| `DUPLICATE_EFFECT` | `warn` | [08](08-effects-and-capabilities.md) §8.2.3 | 3 | the same atom twice in one effect set |
| `UNUSED_EFFECT_DECLARATION` | `warn` | [08](08-effects-and-capabilities.md) §8.9.2 | 3 | a private function declares an atom it does not need |
| `PUBLIC_EFFECTS_INFERRED` | `warn` | [08](08-effects-and-capabilities.md) §8.9.3, D-EFF-01 | 3 | a `pub` function of a pre-1.0 package relies on inferred effects |
| `CONTRACT_RUNTIME_CHECK` | `info` | [09](09-contracts-and-refinements.md) §9.7.1, D-CONTRACT-01 | 3 | an obligation that is not proven and is checked at run time |
| `DUPLICATE_PACKAGE_LINE` | `warn` | [package/02](../package/02-resolver.md) §2.9, D-PKG-02 | 3 | several compatibility lines of one package in the graph |
| `AMBIENT_IMPORT` | `warn` | [08](08-effects-and-capabilities.md) §8.5.3, D-CAP-02 | 4 | application code imports a WASI interface directly |
| `UNUSED_VARIABLE` | `warn` | this chapter, §15.18.1 | 0 | a local binding or parameter that is never used |
| `UNNECESSARY_VAR` | `warn` | this chapter, §15.18.2 | 0 | a `var` binding that is never mutated |
| `UNUSED_ITEM` | `warn` | this chapter, §15.18.3 | 0 | a package-private item that is never referenced |
| `UNUSED_LABEL` | `warn` | this chapter, §15.18.4 | 0 | a loop label that no `break` or `continue` names |
| `NAMING_CONVENTION` | `warn` | this chapter, §15.18.5 | 0 | a declared name that does not follow the case convention of its kind |
| `UNUSED_ATTRIBUTE` | `warn` | this chapter, §15.18.6 | 0 | a valid attribute that has no effect |
| `DEPRECATED_REFERENCE` | `warn` | this chapter, §15.6.4 | 0 | a reference to a deprecated declaration |
| `MISSING_DOCS` | `allow` | this chapter, §15.18.7 | 0 | a public declaration without a doc comment |

### 15.17.3 Warnings and infos that are not lints

The following v0.1 codes are hard warnings or hard infos. Their severity is fixed, and naming them in a level attribute, a `lints` entry or a lint flag is `NOT_A_LINT`. `--deny-warnings` turns the warnings among them into errors ([impl/11](../impl/11-diagnostics.md) §11.3.3).

| Code | Severity | Owner | Reason (§15.17.1) |
|---|---|---|---|
| `MISPLACED_DOC_COMMENT` | warning | [02](02-lexical-structure.md) §2.3 | 3: the doc comment is read as an ordinary comment |
| `WIT_ITEM_SHADOWED` | warning | [13](13-components-and-wit.md) §13.7.4 | 1: the collision is inside imported WIT |
| `EXPORT_WORLD_NOT_APPLIED` | info | [13](13-components-and-wit.md) §13.6.2 | 2: a fact about the build |
| `EXPORT_FN_NOT_APPLIED` | info | [13](13-components-and-wit.md) §13.6.4 | 2: a fact about the build |
| `ENTRY_POINT_NOT_APPLIED` | info | [targets](../targets/targets-and-profiles.md) §4.5 | 2: a fact about the build |
| `LARGE_STACK_FRAME` | info | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.7.2 | 2: depends on compiler-private frame layout; an info so that `--deny-warnings` cannot make acceptance depend on optimization |
| `DEPRECATED_DIAGNOSTIC_CODE` | warning | [impl/11](../impl/11-diagnostics.md) §11.12.3 | 2: a fact about the toolchain's registry |
| `NOT_PRESENT_IN_BUILD` | info | [impl/13](../impl/13-cli.md) §13.6.14 | 2: a fact about a query of the CLI |
| `SYMBOL_INDEX_DISCARDED` | info | [impl/12](../impl/12-semantic-tooling.md) §12.9.3 | 2: a fact about the toolchain's cached index |
| `DUPLICATE_LIST_ITEM` | warning | [package/01](../package/01-package-contract.md) §1.3.1 | 2: a fact about the manifest |
| `VERSION_REQ_SPANS_LINES` | warning | [package/01](../package/01-package-contract.md) §1.10.2 | 2: a fact about the manifest |
| `UNUSED_OVERRIDE` | warning | [package/01](../package/01-package-contract.md) §1.16, [package/02](../package/02-resolver.md) §2.7.3 | 2: a fact about the manifest and the graph |
| `INVALID_LICENSE_EXPRESSION` | warning | [package/01](../package/01-package-contract.md) §1.17 | 2: a fact about the manifest |
| `CONFUSABLE_PACKAGE_NAME` | warning | [package/04](../package/04-distribution-and-security.md) §4.11.1 | 2: a fact about package identities |
| `LOCK_NOT_CANONICAL` | warning | [package/03](../package/03-lock-file.md) §3.2.3 | 2: a fact about the lock file |
| `SYMLINK_IGNORED` | warning | [package/03](../package/03-lock-file.md) §3.4.2 | 2: a fact about fetched content |
| `SUBMODULE_IGNORED` | warning | [package/03](../package/03-lock-file.md) §3.4.2 | 2: a fact about fetched content |
| `TAG_MISSING` | warning | [package/04](../package/04-distribution-and-security.md) §4.3.3 | 2: a fact about the repository |
| `CACHE_CORRUPT` | warning | [package/04](../package/04-distribution-and-security.md) §4.4.2 | 2: a fact about the cache |
| `FOOTPRINT_EXCEEDS_POLICY` | info | [package/04](../package/04-distribution-and-security.md) §4.7.1 | 2: a fact about the graph and the policy |
| `FOOTPRINT_NOT_GRANTED` | info | [package/04](../package/04-distribution-and-security.md) §4.7.1 | 2: a fact about the graph and the manifest |

### 15.17.4 Changing the registry

The rules of [impl/11](../impl/11-diagnostics.md) §11.12 apply to lints. Within a major language version a lint stays a lint, a hard code stays hard, and a default level is raised only from `allow` or `info` to `warn`. A document that adds a warning or info states its class by the rule of §15.17.1 and adds it to §15.17.2 or §15.17.3.

---

## 15.18 The lints of this chapter

The lints of this section are reported only for code that exists for the build target (§15.11). Each occurrence is reported once, at the position stated.

### 15.18.1 `UNUSED_VARIABLE`

1. **What is checked.** Every binding introduced by a pattern of a `let` statement, a `var` statement, a `match` arm, an `if let` or `while let` condition or a `for` loop; every closure parameter; every parameter of a function that has a body (a `fn`, an `impl fn`, a `trait fn` with a default body); and every parameter of a `property`. `self` is not checked. Parameters of functions without a body (trait fns without a default body, `extern fn`, `interface fn`) are not checked.
2. **Used.** A binding is used if a path expression anywhere in its scope resolves to it ([04](04-modules-and-names.md) §4.5.1), including the target of an assignment, the operand of `borrow` or `borrow mut`, the receiver of a method call, an interpolated expression of a format string, and a capture by a closure. The bindings of one name in the alternatives of an or-pattern are one binding.
3. **Exempt.** A binding whose name starts with `_` is not reported ([02](02-lexical-structure.md) §2.4 rule 4). `_` alone is not a binding.
4. **Report.** At the binding's name. The diagnostic carries a machine-applicable fix that renames the binding to `_name`. If the binding is a punned field of a record pattern ([03](03-grammar.md) §3.14), the fix writes `name: _name` instead, so that the field is unchanged.

### 15.18.2 `UNNECESSARY_VAR`

1. **What is checked.** Every binding declared mutable with `var` ([07](07-ownership-and-borrowing.md) §7.2.4 rule 1): a `var` statement, a `var` binding mode in a pattern, a `var` parameter and a `var` closure parameter, including `var self`.
2. **Reported** when no use of the binding needs mutability: changing it to an immutable binding would not produce `ASSIGN_TO_IMMUTABLE` or `BORROW_MUT_OF_IMMUTABLE` ([07](07-ownership-and-borrowing.md) §7.2.4) anywhere. For example, assignments and compound assignments to a place rooted at the binding, `borrow mut` of such a place ([07](07-ownership-and-borrowing.md) §7.5.1), a receiver auto-borrowed as `borrow mut` (§7.5.3 of 07), and a capture by mutable borrow (§7.11.2 of 07) need mutability. A write through a `borrow mut` held in the binding does not (07 §7.2.4 rule 4).
3. A binding that is reported by `UNUSED_VARIABLE` is not reported by `UNNECESSARY_VAR` too.
4. **Report.** At the `var` keyword. The fix is machine-applicable: it replaces `var` with `let` in a `var` statement and removes `var` elsewhere.

### 15.18.3 `UNUSED_ITEM`

1. **What is checked.** In the root package and the workspace members: every module-level `fn`, `record`, `enum`, `distinct`, `refined`, `alias`, `resource`, `trait`, `const`, `effect`, `interface` and `world` that is not visible outside its package ([04](04-modules-and-names.md) §4.6), and every inherent impl fn that is not `pub`.
2. **Used.** An item is used if a reference to it (§15.6.4 rules 1–5, with the deprecated declaration replaced by the item) occurs outside its own syntax range (§15.16.2), anywhere in the package, including tests, properties and other unused items, or in `sai.package` (`export world`). References from inside the item, such as a recursive call, do not count. With test analysis (`sai test`, `sai check --tests`), the references from tests and properties are those that the analysis finds. Without it, tests and properties are not resolved, and an item counts as referenced if its name occurs as an identifier token in a test or property of the same package ([14](14-testing.md) §14.2.4 item 4, N-I-39).
3. **Exempt.** Items whose name starts with `_`; `pub` and `export` items; `impl` blocks and their trait impl fns and associated types; tests and properties; `use` declarations (they have `UNUSED_IMPORT`); `extern` blocks and `extern fn` (they describe what the host provides, and an import that is not reachable is not emitted, [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.10.1, N-F-20); `invariant` declarations; the root-module functions that implement a world's `export` function ([04](04-modules-and-names.md) §4.8.2); and the functions of an interface impl.
4. **Report.** At the item's name, with a help that suggests removing it or adding a leading `_`. There is no fix, because removing code is never automatic.

The rule is syntactic on purpose: two private functions that call only each other are both used. The result depends only on the references in the package (P8).

### 15.18.4 `UNUSED_LABEL`

A loop label ([03](03-grammar.md) §3.13.5) that no `break` or `continue` inside its loop names is reported at the label. The fix is machine-applicable and removes the label and its `:`.

The following example shows the lints of §15.18.1 to §15.18.4.

```sai
fn checksum(data: borrow Array<U32, 4>, seed: U32) -> U32 { // lint: UNUSED_VARIABLE
    var total: U32 = 0
    var limit: USize = 4 // lint: UNNECESSARY_VAR
    var i: USize = 0
    'scan: while i < limit { // lint: UNUSED_LABEL
        total = total +% (*data)[i]
        i += 1
    }
    total
}

fn old_checksum(data: borrow Array<U32, 4>) -> U32 { // lint: UNUSED_ITEM
    (*data)[0]
}

fn _draft_checksum() -> U32 {
    0
}

pub fn report(data: borrow Array<U32, 4>) -> U32 {
    checksum(data, 7)
}
```

The parameter `seed` is never used; the fix renames it to `_seed`. `limit` is declared with `var` but never assigned or borrowed mutably; the fix replaces `var` with `let`. `total` and `i` are assigned and are not reported. No `break` or `continue` names `'scan`; the fix removes `'scan: `. `old_checksum` is private and referenced nowhere. `_draft_checksum` is not reported because its name starts with `_`, and `report` is not reported because it is `pub`.

### 15.18.5 `NAMING_CONVENTION`

The case of a name already decides its kind (D-LEX-02, [02](02-lexical-structure.md) §2.4). This lint checks the rest of the convention. For each declared name, the **checked part** is the name without a raw prefix `r#` and without leading `_`. If it is empty, nothing is checked. A character is uppercase if its General Category is Lu or Lt, and lowercase if it is Ll.

| Convention | Rule on the checked part | Declarations |
|---|---|---|
| UpperCamelCase | contains no `_` | records, enums, distinct types, refined types, aliases, resources, traits, enum variants, generic type parameters, associated types |
| UPPER_SNAKE_CASE | contains no lowercase character | `const` items |
| snake_case | contains no uppercase character | functions and methods, local bindings, parameters, fields, effects, interfaces, worlds, extern fns, `use` aliases, loop labels |

1. Every declaration in the source of the package is checked once, at its name. References are not checked. Module names come from file names, which [04](04-modules-and-names.md) §4.2.3 already restricts, and are not checked here.
2. Names that SAI does not choose are not checked: the SAI view of WIT items ([13](13-components-and-wit.md)), and names written as strings (`@export_name`, `@import_name`, target names).
3. **Fix.** The diagnostic carries a `rename_symbol` fix ([impl/11](../impl/11-diagnostics.md) §11.7.3) to the converted name, keeping the leading `_` and the raw prefix if any. The conversion to snake_case puts `_` before an uppercase character that follows a lowercase character or a digit, or that is followed by a lowercase character and preceded by an uppercase character, and then lowercases. The conversion to UpperCamelCase removes each `_` and uppercases the character after it. The conversion to UPPER_SNAKE_CASE is the snake_case conversion followed by uppercasing. The fix is `machine-applicable` for a declaration that is not visible outside its package and is not inside an interface or world, and `maybe-incorrect` otherwise, because the rename changes the public API or the WIT. No fix is offered if the converted name is a keyword or is already bound in a scope that the rename would affect.

```sai
pub fn nextValue(n: I64) -> I64 { // lint: NAMING_CONVENTION
    n + 1
}

pub type Http_Client { // lint: NAMING_CONVENTION
    pub retryCount: U32 // lint: NAMING_CONVENTION
}

pub const Max_size: U32 = 64 // lint: NAMING_CONVENTION
```

The fixes rename them to `next_value`, `HttpClient`, `retry_count` and `MAX_SIZE`. All four declarations are visible outside the package, so the fixes are `maybe-incorrect`.

### 15.18.6 `UNUSED_ATTRIBUTE`

A valid attribute that has no effect on its target is reported at the attribute, with a machine-applicable fix that removes it. The cases are exactly:

| Attribute | Has no effect when | Section |
|---|---|---|
| `@must_use` on a function | its declared return type is `()`, `Never` or a concrete must-use type | §15.5.3 |
| `@export_name`, `@import_name` | the string equals the function's name | §15.7, §15.8 |
| `@repr` | the enum has no variants | §15.9.1 |
| `@doc(hidden)` | the declaration is not visible outside its package, or a declaration that contains it carries `@doc(hidden)` | §15.13 |
| `@allow`, `@warn`, `@deny` | the cases of §15.16.8; reported at the name | §15.16.8 |

A "concrete must-use type" is a must-use type (§15.5.1) that does not mention a generic parameter of the function. A function returning a generic `T` may return a must-use type for some instantiations only, so `@must_use` on it is not reported.

### 15.18.7 `MISSING_DOCS`

`MISSING_DOCS` has the default level `allow`; a package enables it with `lints { warn MISSING_DOCS }` or an attribute. It reports, in the root package and the workspace members, every declaration that is visible outside its package ([04](04-modules-and-names.md) §4.6) and has no outer doc comment ([02](02-lexical-structure.md) §2.3): items declared `pub` or `export`, their `pub` fields, the variants of `pub` enums, `pub` inherent impl fns, the items of `pub` traits, and the items of interfaces. `use`, `impl` blocks, trait impl fns, tests and properties are not checked. The occurrence is at the declaration's name, with a fix of applicability `has-placeholders` that inserts a `///` line ([impl/11](../impl/11-diagnostics.md) §11.7.2).

```sai
/// A point in the plane.
@warn(MISSING_DOCS)
pub type Point {
    /// The horizontal coordinate.
    pub x: I32
    pub y: I32 // lint: MISSING_DOCS
}
```

The attribute raises `MISSING_DOCS` from `allow` to `warn` for `Point` and its fields (§15.16.2). `Point` and `x` have doc comments; `y` does not.

---

## 15.19 Stages

| Stage | Contents of this chapter |
|---|---|
| 0 | the attribute syntax and the validation of §15.2.4; `@must_use` (§15.5); `@deprecated` and `DEPRECATED_REFERENCE` (§15.6); the validation of `@inline` and `@cold` (§15.4.1); the validation of `@repr` and the discriminant type (§15.9.1, §15.9.2); `@target` (§15.11); `@doc(hidden)` (§15.13); the level attributes and the lint flags of the CLI (§15.15, §15.16.4); the resolution of levels with the sources `attribute`, `cli` and `default` (§15.16); the registry (§15.17); the lints of §15.18 |
| 1 | `@export_name` (§15.7), `@import_name` (§15.8) and `@panic_hook` (§15.14); the code-generation meaning of `@inline`, `@inline(never)` and `@cold` (§15.4.2) and the layout of `@repr` (§15.9.2), because code generation starts at Stage 1 |
| 2 | `@derive` (§15.3), because it generates trait impls |
| 3 | `@cases` and `@seed` (§15.12), because tests are Stage 3; the `lints { }` block and `policy` as level sources (§15.16.3, §15.16.5), because `sai.package` beyond its `package` field is Stage 3 ([04](04-modules-and-names.md) §4.2.1, N-A-1) |
| 4 | `@flags` (§15.10) |

1. An attribute of a stage above the stage the toolchain implements is `FEATURE_NOT_IMPLEMENTED` and is dropped (§15.2.4 step 2).
2. A toolchain classifies the names of level attributes, `lints` entries and lint flags by the complete v0.1 registry (§15.17.2, §15.17.3 and [Appendix A](appendix-a-diagnostic-codes.md)), whatever stage it implements. A lint whose stage (§15.17.2) is above the implemented stage is never reported. A level for it is valid, has no effect, and is not `UNUSED_ATTRIBUTE` (N-A-62).
3. A toolchain that ignores the fields of `sai.package` other than `package` ([04](04-modules-and-names.md) N-A-1) resolves levels from the sources `attribute`, `cli` and `default` only.

---

## 15.20 Decisions introduced by this chapter

| ID | Rule | Rationale |
|---|---|---|
| N-A-42 | The must-use types are exactly: instances of records, enums, distinct types and resources whose declaration carries `@must_use`; refined types that carry `@must_use` or whose base is a must-use type; and anonymous future types. Tuples, arrays, `Option`, `Box`, borrows and distinct types over a must-use base are not must-use types (§15.5.1). | D-EXPR-01 names `Result` and futures only. A closed definition keeps `UNUSED_MUST_USE`, a hard error, predictable. A refined type is its base with a predicate, but a distinct type is a new nominal type (D-TYPE-08) and decides for itself. |
| N-A-43 | The discriminant type of an enum is the integer type named by `@repr(I)`, `I32` for `@repr(C)`, and `I64` otherwise. Discriminants out of its range are `DISCRIMINANT_OUT_OF_RANGE` (§15.9.2). | A C `enum` is an `int`, which is 32 bits on every v0.1 target. The choice matches the 4-byte signed tag of [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.4.12. |
| N-A-44 | `@target` is evaluated once per command, for the command's one target and the artifact kind that `sai build` would produce for it, independently of the profile. It is allowed on module-level items other than `interface` and `world`, and on nothing else (§15.11.2, §15.11.3). | One target per invocation ([impl/13](../impl/13-cli.md) §13.4.1). Interfaces and worlds describe WIT, which must not depend on the target. Members would give one type different shapes on different targets. Profile independence makes `dev` and `release` builds of one target check the same code (P8). |
| N-A-45 | The levels are `allow`, `info`, `warn` and `deny`. `info` is used only as a default level. There is no `forbid` level; `policy` is the only source that dependent code cannot override (§15.15.2). | One mechanism for rules that cannot be overridden, owned by the package that states them ([package/01](../package/01-package-contract.md) §1.12). |
| N-A-46 | Level attributes with different levels for one code on one target are `CONFLICTING_LINT_LEVEL`; the later one is dropped. The same level twice is `DUPLICATE_ATTRIBUTE_ARGUMENT` (§15.15.3). | The same rule as for the `lints` block ([package/01](../package/01-package-contract.md) §1.13). Attribute order has no meaning (§15.2.5), so no order can resolve a conflict silently. |
| N-A-47 | There are no lint groups. Every level attribute, `lints` entry and lint flag names single codes (§15.15.5). | Names stay explicit, and adding a lint within a major version (§15.17.4) never changes what an existing configuration covers. |
| N-A-48 | The level of a lint occurrence is taken from the first source that applies, in the order `policy`, attribute (innermost first), CLI flag, `lints` block, default. The dependency cap (except for `policy`) and `--deny-warnings` are applied afterwards (§15.16.6, §15.16.7). | An attribute states intent at the place it concerns. A flag is per invocation and overrides the manifest's general setting. `policy` is a guarantee of the package and cannot be weakened locally. |
| N-A-49 | A warning or info about source is a lint unless it reports something the user cannot change in their source, a fact about the toolchain, manifest, lock, graph, build or compiler-private choices, or a construct whose meaning the compiler changed. `DUPLICATE_PACKAGE_LINE` is a lint because `policy` raises it. The resulting lists of §15.17.2 and §15.17.3 are complete for v0.1 (§15.17.1). | [impl/11](../impl/11-diagnostics.md) §11.2.3 requires a class for every code. One rule decides all classes, and the lists are the single place that states them. |
| N-A-50 | The attributes of one target are validated in the order of §15.2.4. Steps 1–7 need no name resolution. A failed check drops only the attribute or argument concerned; the other attributes still apply (§15.2.4). | Diagnostics are deterministic and do not cascade; syntactic checks can run in the parser's validation pass ([impl/02](../impl/02-frontend-ast-hir.md) §2.10.2). |
| N-A-51 | Exactly two pairs of attributes exclude each other on one target: `@inline` (without `never`) with `@cold`, and `@flags` with `@derive`. The later one is `CONFLICTING_ATTRIBUTES` (§15.2.4 step 7). | Each pair contradicts itself; every other combination has a defined meaning. |
| N-A-52 | Deprecation is not inherited by members. A deprecated context is the syntax of a deprecated declaration, and of an `impl` or `invariant` of a deprecated declaration of the same package (§15.6.2). | `@deprecated` cannot be written on an `impl` or `invariant`, and the deprecating package keeps its own impls without warnings. Other packages' impls are real uses. |
| N-A-53 | The `instead` string is a dotted path. A one-segment path on a member names a sibling; every other path is resolved from the module scope of the deprecated declaration and the prelude. It must resolve, after `@target`, to another declaration that is not deprecated and is visible outside the package if the deprecated one is. Otherwise it is `INVALID_ATTRIBUTE_ARGUMENT` and is dropped (§15.6.3). | The replacement must be a name that the users of the deprecated declaration can write. |
| N-A-54 | The lint for uses of deprecated declarations is `DEPRECATED_REFERENCE`. It is reported at every reference outside a deprecated context, including references in the declaring package and plain `use` items, and not for compiler-created references or for a `pub use` of the declaring package (§15.6.4). | The name follows [impl/11](../impl/11-diagnostics.md) §11.2.2. Every reported place is a place that must change. |
| N-A-55 | `DUPLICATE_EXPORT_NAME` is owned by this chapter. Export names must be unique in the root package of a core-module build after `@target` filtering, compared by bytes; `memory` is reserved. Later items in canonical-path and source order are reported (§15.7). | The check is defined where `@export_name` is. The order makes the reported item deterministic (P8). |
| N-A-56 | `@inline` and `@cold` are hints. `@inline(never)` is a requirement: every reachable instantiation is a function of its own, and every call of it is a call (§15.4.2). | Profiling and host stack traces need a stable function. None of the three changes observable behavior. |
| N-A-57 | `@repr(C)` is allowed on records and enums; `@repr(I)` with `I` one of `I8`–`I64`, `U8`–`U64` on enums, including enums with payloads. `ISize`, `USize` and `Byte` are not repr types. The layout is fixed by [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.4 (§15.9). | An ABI needs a width that does not change between `wasm32` and `wasm64`. |
| N-A-58 | A target predicate is a target name as a `STRING`, a capability as an identifier, or `all`, `any`, `none` of predicates. An invalid `@target` leaves its item present (§15.11.1). | Target names contain `-` and are not identifiers ([03](03-grammar.md) AttrArg); `not` is a strict keyword ([02](02-lexical-structure.md) §2.5.1). Keeping the item avoids cascades of unresolved names. |
| N-A-59 | `@doc(hidden)` affects only generated documentation and completion in other packages. It is `UNUSED_ATTRIBUTE` on a declaration that is not visible outside its package or is inside a hidden declaration (§15.13). | Hiding never changes what code can name, so it cannot break a program. |
| N-A-60 | A level attribute is `UNUSED_ATTRIBUTE` only in the cases of §15.16.8. That no occurrence of the code exists in its scope is never reported (§15.16.8). | Whether an occurrence exists changes with every edit, target and stage; reporting it would make attributes appear and disappear as warnings. |
| N-A-61 | `UNUSED_ITEM` counts a reference anywhere in the package outside the item's own syntax, including from tests and other unused items; it does not use reachability (§15.18.3). Without test analysis, a reference from a test is an identifier token with the item's name (14 N-I-39). | The rule is local and deterministic. Reachability belongs to dead-code elimination. |
| N-A-62 | Every toolchain classifies level names by the complete v0.1 registry. A lint of a stage the toolchain does not implement is never reported, and a level for it is valid and has no effect (§15.19). | A package that configures a lint of a later stage still builds with a toolchain of an earlier stage. |
| N-A-63 | The names in level attributes are checked in every package of the build, including dependencies (§15.15.4), even though the levels of dependency lints are capped (§15.16.7). | A source file is valid or invalid independently of where it is compiled (P8). |
| N-A-64 | `NAMING_CONVENTION` checks declarations only, after removing `r#` and leading `_`. Its rename fix is machine-applicable only for declarations that are not visible outside their package and are not inside an interface or world (§15.18.5). | Renaming public API or WIT changes a contract and must be reviewed. |
| N-A-65 | `UNNECESSARY_VAR` is reported exactly when removing `var` would produce no `ASSIGN_TO_IMMUTABLE` or `BORROW_MUT_OF_IMMUTABLE` (§15.18.2). | The lint uses the mutability rules of [07](07-ownership-and-borrowing.md) §7.2.4 and cannot drift from them. |

---

## 15.21 Diagnostics

Codes defined by this chapter:

| Code | Severity | Description |
|---|---|---|
| `INVALID_ATTRIBUTE_TARGET` | error | an attribute on a declaration kind or position it does not allow (§15.2.1, §15.2.4 step 3, and each attribute's section) |
| `INVALID_ATTRIBUTE_ARGUMENT` | error | an argument list that does not match the attribute's grammar or value range, an `instead` path that does not resolve to a valid replacement (§15.6.3), or a `@target` operand that is not a target name, a capability or a combinator (§15.11.1) |
| `DUPLICATE_ATTRIBUTE` | error | the same attribute twice on one target, other than the level attributes (§15.2.4 step 5) |
| `DUPLICATE_ATTRIBUTE_ARGUMENT` | error | the same derive name, the same lint name at the same level, or the same `@target` operand twice (§15.2.4 step 6, §15.15.3) |
| `CONFLICTING_ATTRIBUTES` | error | `@inline` with `@cold`, or `@flags` with `@derive`, on one target (§15.2.4 step 7) |
| `DUPLICATE_EXPORT_NAME` | error | two `export fn` items of the root package of a core-module build with the same export name, or an export name `memory` (§15.7) |
| `UNUSED_VARIABLE` | warning (lint) | a local binding or parameter that is never used (§15.18.1) |
| `UNNECESSARY_VAR` | warning (lint) | a `var` binding that never needs to be mutable (§15.18.2) |
| `UNUSED_ITEM` | warning (lint) | a package-private item that is never referenced (§15.18.3) |
| `UNUSED_LABEL` | warning (lint) | a loop label that no `break` or `continue` names (§15.18.4) |
| `NAMING_CONVENTION` | warning (lint) | a declared name that does not follow the case convention of its kind (§15.18.5) |
| `UNUSED_ATTRIBUTE` | warning (lint) | a valid attribute that has no effect (§15.18.6) |
| `DEPRECATED_REFERENCE` | warning (lint) | a reference to a deprecated declaration outside a deprecated context (§15.6.4) |
| `MISSING_DOCS` | warning (lint), default `allow` | a declaration visible outside its package without a doc comment (§15.18.7) |

Codes used by this chapter and defined elsewhere:

| Code | Severity | Description |
|---|---|---|
| `UNKNOWN_ATTRIBUTE` | error | defined in [03](03-grammar.md) §3.16; an attribute name that is not one of §15.2.3 |
| `FEATURE_NOT_IMPLEMENTED` | error | defined in [01](01-introduction.md) §1.4.5; an attribute of a stage the toolchain does not implement (§15.19) |
| `ITEM_IN_BLOCK` | error | defined in [06](06-expressions-and-statements.md) §6.25.1; reported instead of `INVALID_ATTRIBUTE_TARGET` for an attribute sequence followed by an item inside a block (§15.2.1 rule 3) |
| `UNKNOWN_DERIVE` | error | defined in [05](05-types.md) §5.12; a derive name that is not in the fixed list (§15.3.1) |
| `DERIVE_NOT_SUPPORTED` | error | defined in [05](05-types.md) §5.12; including `@derive` on a resource (§15.2.4 step 3) |
| `DERIVE_FIELD_MISSING_IMPL` | error | defined in [05](05-types.md) §5.12 |
| `MISSING_SUPERTRAIT_IMPL` | error | defined in [05](05-types.md) §5.9.1 |
| `OVERLAPPING_IMPLS` | error | defined in [05](05-types.md) §5.10.2; a derived impl overlaps another impl |
| `DISCRIMINANT_OUT_OF_RANGE` | error | defined in [05](05-types.md) §5.6.2; a discriminant outside the discriminant type of §15.9.2 |
| `UNUSED_MUST_USE` | error | defined in [06](06-expressions-and-statements.md) §6.25.2, D-EXPR-01; the must-use types and calls are those of §15.5 |
| `UNRESOLVED_NAME` | error | defined in [04](04-modules-and-names.md) §4.5.1; a reference to an item removed by `@target` (§15.11.3) |
| `WIT_INCOMPATIBLE_TYPE` | error | defined in [13](13-components-and-wit.md) §13.2.2; `@repr` on a type inside an interface |
| `INVALID_FLAGS_ENUM` | error | defined in [13](13-components-and-wit.md) §13.9.4; including `@flags` outside an interface |
| `FLAGS_PATTERN` | error | defined in [13](13-components-and-wit.md) §13.9.4 |
| `EXPORT_FN_NOT_APPLIED` | info | defined in [13](13-components-and-wit.md) §13.6.4; `@export_name` has no effect in a component build |
| `PANIC_HOOK_SIGNATURE_MISMATCH` | error | defined in [10](10-errors-and-panics.md) §10.6.1 |
| `PANIC_HOOK_OUTSIDE_ROOT` | error | defined in [10](10-errors-and-panics.md) §10.6.1 |
| `DUPLICATE_PANIC_HOOK` | error | defined in [10](10-errors-and-panics.md) §10.6.1 |
| `CONFLICTING_WASM_IMPORT` | error | defined in [12](12-unsafe-and-ffi.md) §12.7.6; two imports with one module and field name but different signatures, including names set by `@import_name` |
| `UNKNOWN_TARGET` | error | defined in [package/01](../package/01-package-contract.md) §1.8; a target name in `@target` that the targets doc does not define |
| `CONFLICTING_LINT_LEVEL` | error | defined in [package/01](../package/01-package-contract.md) §1.13; used for level attributes too (§15.15.3, N-A-46) |
| `DUPLICATE_LIST_ITEM` | warning | defined in [package/01](../package/01-package-contract.md) §1.3.1; a `lints` entry repeated with the same level |
| `UNSAFE_DENIED` | error | defined in [package/01](../package/01-package-contract.md) §1.12 and [12](12-unsafe-and-ffi.md) §12.11.3 |
| `UNKNOWN_LINT` | error | a level attribute, a `lints` entry or a CLI lint flag names something that is not a diagnostic code (§15.15.4); the registry lookup is in [impl/11](../impl/11-diagnostics.md) §11.12.3 |
| `NOT_A_LINT` | error | a level attribute, a `lints` entry or a CLI lint flag names the code of a hard diagnostic (§15.15.4) |
| `DEPRECATED_DIAGNOSTIC_CODE` | warning | defined in [impl/11](../impl/11-diagnostics.md) §11.12.3 |
| `CONFLICTING_OPTIONS` | error | defined in [impl/13](../impl/13-cli.md) §13.3.1 and §13.4.4; two lint flags with different levels for one code |
| `ASSIGN_TO_IMMUTABLE` | error | defined in [07](07-ownership-and-borrowing.md) §7.2.4; used to define `UNNECESSARY_VAR` |
| `BORROW_MUT_OF_IMMUTABLE` | error | defined in [07](07-ownership-and-borrowing.md) §7.2.4; used to define `UNNECESSARY_VAR` |
| `NON_ASCII_IDENTIFIER` | warning (lint) | defined in [02](02-lexical-structure.md) §2.4; level and class in §15.17.2 |
| `UNNECESSARY_RAW_IDENT` | warning (lint) | defined in [02](02-lexical-structure.md) §2.4; level and class in §15.17.2 |
| `UNUSED_IMPORT` | warning (lint) | defined in [04](04-modules-and-names.md) §4.3.6; level and class in §15.17.2 |
| `REDUNDANT_IMPORT` | warning (lint) | defined in [04](04-modules-and-names.md) §4.3.6; level and class in §15.17.2 |
| `SHADOWS_PRELUDE` | warning (lint) | defined in [04](04-modules-and-names.md) §4.5.1; level and class in §15.17.2 |
| `IRREFUTABLE_CONDITION_PATTERN` | warning (lint) | defined in [06](06-expressions-and-statements.md) §6.14.2; level and class in §15.17.2 |
| `REDUNDANT_BINDING_MODE` | warning (lint) | defined in [06](06-expressions-and-statements.md) §6.21.3; level and class in §15.17.2 |
| `REDUNDANT_UPDATE_BASE` | warning (lint) | defined in [06](06-expressions-and-statements.md) §6.18.1; level and class in §15.17.2 |
| `STATEMENT_HAS_NO_EFFECT` | warning (lint) | defined in [06](06-expressions-and-statements.md) §6.25.2; level and class in §15.17.2 |
| `UNREACHABLE_CODE` | warning (lint) | defined in [06](06-expressions-and-statements.md) §6.26.2; level and class in §15.17.2 |
| `UNREACHABLE_PATTERN` | warning (lint) | defined in [06](06-expressions-and-statements.md) §6.22; level and class in §15.17.2 |
| `REDUNDANT_PROVENANCE` | warning (lint) | defined in [07](07-ownership-and-borrowing.md) §7.7.3; level and class in §15.17.2 |
| `UNUSED_UNSAFE` | warning (lint) | defined in [12](12-unsafe-and-ffi.md) §12.2.1; level and class in §15.17.2 |
| `MISSING_SAFETY_DOC` | warning (lint) | defined in [12](12-unsafe-and-ffi.md) §12.2.2; level and class in §15.17.2 |
| `DUPLICATE_EFFECT` | warning (lint) | defined in [08](08-effects-and-capabilities.md) §8.2.3; level and class in §15.17.2 |
| `UNUSED_EFFECT_DECLARATION` | warning (lint) | defined in [08](08-effects-and-capabilities.md) §8.9.2; level and class in §15.17.2 |
| `PUBLIC_EFFECTS_INFERRED` | warning (lint) | defined in [08](08-effects-and-capabilities.md) §8.9.3 (D-EFF-01); level and class in §15.17.2 |
| `CONTRACT_RUNTIME_CHECK` | info (lint) | defined in [09](09-contracts-and-refinements.md) §9.7.1 (D-CONTRACT-01); raised by `policy { deny runtime_contracts }` (§15.16.5) |
| `DUPLICATE_PACKAGE_LINE` | warning (lint) | defined in [package/02](../package/02-resolver.md) §2.9 (D-PKG-02); raised by `policy { deny duplicate_lines }` (§15.16.5) |
| `AMBIENT_IMPORT` | warning (lint) | defined in [08](08-effects-and-capabilities.md) §8.5.3 (D-CAP-02); raised by `policy { deny ambient_imports }` (§15.16.5) |

The hard warnings and infos of §15.17.3 are defined by the documents named there. This chapter only states that they are not lints.
