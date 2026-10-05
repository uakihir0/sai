# SAI Design Decision Record

**Version:** 0.1
**Status:** Normative. No other document may contradict this record.
**Scope:** Every question that the language draft `specification.txt` (v0.1 Draft) left open or ambiguous.

---

## 0. About this document

This document records, under stable IDs, the design decisions made while turning the draft into a formal language specification and an implementation specification.

- Each decision has an ID of the form `D-<AREA>-<NN>`.
- Each decision states its **rationale** in terms of the principles in draft §87 "SAI Philosophy".
- The detailed documents (`docs/spec/*`, `docs/impl/*`, `docs/package/*`, `docs/targets/*`) refine this record. If they conflict with it, this record wins. To change a decision, amend this record, keep the decision ID, and add an entry to the revision history (§17).

### 0.1 Decision criteria (priority order from draft §87)

When a choice is unclear, apply these principles in this order.

| Tag | Principle |
|---|---|
| P1 | Explicit > Implicit |
| P2 | Static knowledge > Runtime guessing |
| P3 | Semantic operations > Text manipulation |
| P4 | Portable boundary > Language-specific ABI |
| P5 | Capability > Ambient authority |
| P6 | Source compatibility > Repository location |
| P7 | Small core > Large runtime |
| P8 | Predictability > Magic |
| P9 | Boring correctness > Clever syntax |

The principles of draft §2 are treated as preconditions: Wasm-first, no mandatory runtime, pay only for what you use, and AI-native but human-first readable.

**Determinism principle (a corollary of P8):** Whether a program is accepted or rejected (that is, whether it compiles) MUST depend only on deterministic procedures defined by this specification. It MUST NOT depend on the strength of an optimizer or on implementation-specific heuristics. Optimizations can change the performance of an accepted program, but never whether it is accepted.

---

## 1. Lexical structure and syntax (LEX / SYN)

### D-LEX-01 Newlines terminate statements (no semicolons)

- Statements are terminated by a `NEWLINE` token, not by semicolons. `;` is not a token; using it produces a dedicated diagnostic.
- The lexer emits `NEWLINE` according to the following rules.
  1. If the innermost open bracket is `(` or `[`, newlines are ignored (implicit line continuation, as in Python). If the innermost open bracket is `{`, or there is no open bracket, rules 2 and 3 apply.
  2. A `NEWLINE` is emitted only if the previous token belongs to the **terminator set E**:
     identifiers (including contextual keywords), all literals, `true` `false` `self` `Self` `return` `break` `continue` `await`, `)` `]` `}` `>` `?` `_`, and labels (`'name`).
  3. Regardless of rule 2, no `NEWLINE` is emitted if the first token of the next line belongs to the **continuation set C**:
     - `.` followed by a lowercase identifier, `await`, or a decimal integer (method chains, field access, tuple indexing)
     - `and`
     - `or`
     - `else`
  4. Consecutive `NEWLINE`s collapse into one.
- In addition, the parser skips `NEWLINE` at specific grammar positions (before signature clauses, before a function body `{`, and so on). These positions are marked `nl` in the EBNF.
- Rationale: P9, P8. The rule depends only on the previous token and on the first token of the next line (as in Go). It never guesses non-locally the way JavaScript's ASI does.
- Consequence: a line ending in `a >` is a syntax error. To break a binary expression across lines, put the operator at the end of the line or wrap the expression in parentheses. The formatter wraps with parentheses.

### D-LEX-02 Letter case is significant (case rule)

- An identifier whose first letter (ignoring a `r#` prefix and leading `_`) is uppercase (ASCII `A-Z`, or Unicode General Category Lu/Lt) is an **upper identifier**. Every other identifier is a **lower identifier**.
- These declarations MUST use upper identifiers: types (`type`, `enum`, `resource`, `distinct`), traits, enum variants, constants, and type parameters.
- These declarations MUST use lower identifiers: functions, variables, parameters, fields, modules, interfaces, worlds, effects, and labels.
- In patterns, a bare upper identifier refers to a constructor or a constant, and a bare lower identifier introduces a new binding. This syntactically rules out the Rust pitfall where something that looks like a constant is actually a binding.
- Convention: types use `UpperCamelCase`, constants `UPPER_SNAKE_CASE`, everything else `snake_case`. Only the case of the first letter is mandatory. The rest of the convention is checked by the lint `NAMING_CONVENTION` (warning).
- Rationale: P8, P9. It also makes the mapping to WIT kebab-case deterministic (P4).

### D-LEX-03 Identifier character set

- Identifiers follow UAX #31 Default Identifiers (XID_Start / XID_Continue) and MUST be NFC-normalized.
- Non-ASCII identifiers trigger the lint `NON_ASCII_IDENTIFIER` (warning). Names of items that are `pub` or `export`, `pub` fields, variants of `pub` enums, and names of modules, packages, interfaces, and worlds, together with every name declared inside an interface or world, MUST be ASCII (error, `NON_ASCII_PUBLIC_NAME`). This corresponds to draft §4.1.
- The raw identifier syntax `r#name` lets a keyword be used as an identifier, for example to avoid clashes with WIT names.

### D-LEX-04 Keyword classes

- **Strict keywords** cannot be used as identifiers:
  `and as assert async await borrow break const continue else enum export extern false fn for if impl in let loop match move mut not or owned pub return self Self trait true type unsafe use var where while`
- **Contextual keywords** are keywords only in specific positions and identifiers everywhere else:
  `borrows covers distinct effect effects ensures import include interface invariant module old property region requires resource result test world`
- **Reserved words** cannot be used at all. They are reserved for the future or exist so that misuse gets a helpful diagnostic:
  `actor catch class defer dyn macro nil null static throw try undefined yield`
  Using `null`, `nil`, or `undefined` produces the diagnostic `NO_NULL`, which points the user to `Option` (draft §9).

### D-LEX-05 Literals

- Integers: `123` `1_000` `0xFF` `0o77` `0b1010`. There are no type suffixes. The type comes from context, and defaults to `Int` (= `I64`) when the context does not determine it. A literal that does not fit its expected type is a compile error.
- Floats: `1.0` `1e10` `1.5e-3`. At least one digit is required on each side of the decimal point. The default type is `F64`.
- Immediately after a member-access `.`, a digit sequence is always lexed as an integer, never as a float, so that `t.0.1` works.
- Strings `"..."` support the escapes `\n \r \t \\ \" \' \0 \u{H..H}`. Raw newlines are not allowed.
- Multiline strings `"""..."""` strip, from every line, the indentation of the line that holds the closing `"""`.
- Raw strings `r"..."` / `r#"..."#`. Byte strings `b"..."` have type `Slice<Byte>` and allow `\xHH`. Byte literals `b'a'` have type `Byte`. Character literals `'a'` have type `Char`.
- Format strings `f"...{expr}..."` are a Stage 3 feature (D-EXPR-12).
- Comments: `//`, `/* */` (which nest), and doc comments `///` (outer) and `//!` (module-level).

### D-LEX-06 Unicode data version

- The v0.1 lexical rules use Unicode 16.0.0 data for XID properties, General Category, and NFC normalization.
- Rationale: identifier acceptance, case classification, and normalization must be identical across hosts and compiler builds. Pinning one Unicode data version avoids making source compatibility depend on the host's Rust toolchain or operating system.

### D-SYN-01 Records are constructed and destructured with parentheses and labeled arguments

- Construction: `User(id: id, name: "a")`. Field punning `User(id, name)` means `User(id: id, name: name)`. Update syntax: `User(name: n, ..base)`.
- Patterns: `User(id: i, ..)`, or with punning `User(id, ..)`.
- Rationale for parentheses: they syntactically rule out Rust's ambiguity between a struct literal and a block in `if x {`, and they do not interact with the newline rule (P9).
- Only named-field structs exist. There are no tuple structs; use `distinct` for newtypes.
- Ordinary function calls have no named arguments. An argument of the form `label: expr` is allowed only in record construction.

### D-SYN-02 Referring to enum variants

- Fully qualified: `Status.Pending`. When the expected type is known, the **implicit member** forms `.Pending` / `.Err(e)` may be used.
- The prelude names `Some`, `None`, `Ok`, and `Err` can be used unqualified.
- There are three variant shapes: unit `Pending`, tuple `Ok(T)`, and record `Rect(width: F64, height: F64)`.

### D-SYN-03 `.` is the only path separator

- Every namespace separator in expressions and types is `.`: `module.item`, `Type.assoc_fn`, `Enum.Variant`, `T.Item`. There is no `::`.
- Only the package path in `use` (and world `import`/`export`) uses the `ns:pkg/a/b` form (draft §27).
- Within a module, types, values, modules, traits, effects, interfaces, and worlds share a single namespace. Duplicates are an error.

### D-SYN-04 Explicit generic arguments

- Generic arguments are written the same way in expressions: `f<T>(x)`, `json<User>()`, `Vec<I32>.new()`. There is no turbofish (draft §80).
- Disambiguation, using the same rule as C#: when the parser sees `IDENT <` in an expression, it tries to parse a generic argument list. If that succeeds and the token after the closing `>` is one of `(` `.` `)` `]` `}` `,` `?` `NEWLINE` `=>` `{` EOF, the list is taken as generic arguments. Otherwise the parser backtracks and reads `<` as a comparison.
- Comparisons are non-associative (`a < b < c` is a syntax error), so this rule never misreads a valid program.

### D-SYN-05 Operators and precedence

From highest to lowest precedence:

1. Postfix: `.field` `.method(..)` `(..)` `[..]` `?` `.await`
2. Prefix: `-` `~` `*` (deref) `borrow` `borrow mut`
3. `*` `/` `%` `*%`
4. `+` `-` `+%` `-%`
5. `<<` `>>`
6. `&`
7. `^`
8. `|`
9. `..` `..=` (non-associative)
10. `==` `!=` `<` `<=` `>` `>=` (non-associative)
11. `not`
12. `and`
13. `or`

- The logical operators are `and`, `or`, and `not`, as in the draft's examples (§13). Bitwise complement is `~`. `!` is not an operator.
- Assignment (`=`, `+=`, ...) is a statement, not an expression. `a = b = c` and `if a = b` cannot be written.
- `as` is never a numeric conversion. It is used only for aliases in `use ... as alias`. All numeric conversions are explicit core functions (D-TYPE-06).

### D-SYN-06 Function signature clauses have a fixed order

```
[attrs] [pub|export] [unsafe] [async] fn name<G>(params) [-> Ret]
    [where ...]
    [borrows { ... }]
    [effects { ... }]
    [requires expr]*
    [ensures expr]*
{ body }
```

The order is fixed and the formatter emits it in this order (P9: one way to write it).
Parameters are simple names (`name: Type` or `var name: Type`). They cannot be destructuring patterns. There are no default, named, or variadic parameters.

### D-SYN-07 List separators inside braces

- Lists inside `{ }` (struct fields, variants, match arms, effect sets, allow lists, ...) are separated by `NEWLINE` or `,`.
- Lists inside `( )`, `[ ]`, and `< >` are separated by `,` (newlines are ignored there). A trailing comma is allowed.
- The formatter's canonical form is `{ a, b }` when the list fits on one line, and otherwise one element per line with no commas. The fields of a record or resource declaration and the variants of an enum declaration are always one per line, so that a change to one of them is a one-line change (P3, P8). The item list of a `use` is printed `.{A, B}`, without inner spaces, and its items are sorted (`docs/impl/14-formatter.md`).

### D-SYN-08 A file is a module; there are no inline modules

- `<source>/a/b.sai` is the module `a/b`. Directories form namespaces.
- The root module of a package is always `<source>/lib.sai`. Fixing the name keeps it predictable (P8). The kind of artifact (core module, component, command) is decided by `sai.package` and the world, not by the file name.
- `lib` is reserved as a module name. Module names MUST be lower identifiers.
- `unsafe module` is a declaration at the top of a file and makes the whole module an unsafe context. It replaces the draft's `unsafe module decoder { ... }` (§69) with a file-level declaration.

### D-SYN-09 Attributes are a closed set

- Syntax: `@name` or `@name(args)`, placed before an item, field, or variant. There are no user-defined attributes and no macros (draft §83).
- v0.1 attributes: `@derive(..)` `@inline` `@inline(never)` `@cold` `@must_use` `@deprecated("..")` `@allow(CODE)` `@warn(CODE)` `@deny(CODE)` `@export_name("..")` `@import_name("..")` `@repr(..)` `@flags` `@target(..)` `@cases(N)` `@seed(N)` `@doc(hidden)` `@panic_hook`.
- `docs/spec/15-attributes-and-lints.md` gives the details.

### D-SYN-10 Statement-level restrictions

- `let` and `var` always require an initializer.
- `var` declares a single mutable variable: `var x: T = e`. To make a binding inside a pattern mutable, use the binding mode `var x` in that pattern (`let (var a, b) = pair`).
- Items (functions, types, ...) cannot be declared inside blocks in v0.1. Use closures for local helpers.
- There is no array-repeat syntax (`[x; n]`). Use the core function `Array.filled(x)`, which takes its length from the expected type.

---

## 2. Modules, names, and visibility (MOD)

### D-MOD-01 `use`

```sai
use acme:server/http/client // modules of the current package also use the full identity
use acme:user-client // binds the root module as `user_client` (kebab -> snake)
use sai:http/client // binds the module as `client`
use sai:http/client as http_client // binds it under an alias
use sai:http/client.{HttpClient, Request as Req} // imports individual items
```

- There are no glob imports (`*`) (P1).
- Modules of the current package are referred to by the full package identity as well; there are no relative paths. Using the same form inside and across packages makes code easier to grep and to analyze (P3, P8). Renaming a package is handled as a semantic operation.
- A local binding cannot shadow an imported module alias (error `SHADOWS_MODULE`).
- The core prelude is implicitly imported into every module (`docs/spec/16-core-library.md`).

### D-MOD-02 Three visibility levels: package-private, `pub`, and `export`

- The default is package-private (draft §5.3). There is no module-private level.
- `pub` makes an item visible outside the package.
- `export` makes a function an export of the produced core Wasm module. `export` implies `pub`. Writing `pub export` is redundant and is an error.
- Struct fields are also package-private by default and made public with `pub`.
- `pub distinct Id = U64` makes the type public but keeps its representation opaque. `pub distinct Id = pub U64` also makes the constructor and `.value` public.

### D-MOD-03 Shadowing

- An inner scope (nested block, match arm, closure) may shadow an outer binding.
- **Redeclaring a name in the same block is an error** (`DUPLICATE_BINDING`), so within one block a name always refers to one binding (P8).
- A function body is a scope nested inside the parameter scope, so `let x = x.trim()` is allowed.

### D-MOD-04 Re-exports

- `pub use ns:pkg/m.{Item, ...}` re-exports the listed items as part of the current module's public API. Modules themselves cannot be re-exported.
- A re-exported item keeps its original symbol identity. The re-export is an alias, not a new symbol (P3: semantic tooling sees one symbol).
- Re-export chains must not form cycles (`REEXPORT_CYCLE`).

---

## 3. Type system (TYPE)

### D-TYPE-01 Primitive types

- `I8 I16 I32 I64 U8 U16 U32 U64 F32 F64 Bool Byte Char ISize USize`.
- `Int` and `UInt` are **transparent aliases** of `I64` and `U64`.
- `Byte` has the same representation as `U8` but is a distinct type. It supports comparison and bitwise operations, but not arithmetic. It converts to and from `U8` through explicit functions. `Slice<Byte>` and `Slice<U8>` convert through zero-cost explicit functions.
- `Char` is a Unicode scalar value (U+0000..U+D7FF, U+E000..U+10FFFF), 32 bits wide, with no arithmetic.
- The unit type is `()` and its only value is `()`.
- `Never`, the type of diverging expressions, is a prelude type name.

### D-TYPE-02 Compound types

- Tuples `(A, B)`; a one-element tuple is `(A,)`.
- `Array<T, N>`, where `N` is a const generic of type `USize`.
- Structural types: tuples, `Array`, `borrow T`, `borrow mut T`, and function pointer types.
- Nominal types: `type` (struct), `enum`, `resource`, and `distinct`.
- **View types** contain a borrow and carry exactly one region. The shared views `Str`, `Slice<T>`, and `borrow T` are Copy. The exclusive views `SliceMut<T>` and `borrow mut T` are not Copy.
- Owned types are lang items of the alloc library: `String`, `Vec<T>`, `Box<T>`. `Bytes` is a type alias of `Vec<Byte>`.
- **There are no dynamically sized types (DSTs).** Every type has a statically known size. `Str` is a value `(ptr, len)` (P9: it avoids Rust's `str` vs `&str` duality).
- There are no trait objects (`dyn`) in v0.1 (draft §15, "static polymorphism"). Use function pointers or enums for dynamic dispatch.

### D-TYPE-03 Type declarations

```sai
// A record type.
type User {
    id: UserId
    pub name: String
}

// An enum.
enum Shape {
    Circle(F64)
    Rect(width: F64, height: F64)
    Empty
}

type Bytes = Vec<Byte> // transparent alias
type Percentage = I32 where self >= 0 and self <= 100 // refined type
distinct UserId = U64 // nominal newtype

// A resource type.
resource File {
    fd: U32
}
```

- A refined type is nominal per declaration. A refined type `R` with base `B` coerces implicitly to `B`. `B` coerces implicitly to `R` only when the Standard Fact Checker (SFC, D-CONTRACT-02) proves the predicate. When it cannot, the coercion is a compile error and explicit validation `R.try_from(v)` is required. This is the draft §13 rule that safe boundaries require validation.
- Refinement subtyping applies only at the top level of a value. It does not propagate into type constructors: `Vec<Percentage>` and `Vec<I32>` do not convert into each other (P8).
- A distinct type `D = B` has the constructor `D(b)`, the pattern `D(b)`, and the accessor `d.value`. It inherits none of the base type's traits or operators; `@derive(..)` forwards the needed implementations. Literals never have a distinct type directly (write `Money(0)`).
- Distinct types can be combined with a refinement: `distinct Age = U8 where self <= 150`.

### D-TYPE-04 Implicit coercions form a closed list

Only these implicit conversions exist (P1):

1. `Never` to any type
2. `borrow mut T` to `borrow mut T` for a shorter region (reborrow)
3. `borrow mut T` to `borrow T`
4. `SliceMut<T>` to `Slice<T>`
5. function item type to function pointer type (when the effects are compatible; effects are compared from Stage 3 on)
6. refined type `R` to its base type `B`, and `borrow R` to `borrow B` (`borrow mut R` never coerces, because a mutable borrow could break the predicate; `docs/spec/09-contracts-and-refinements.md` N-C-40)
7. base type `B` to refined type `R` (only when the SFC proves it)
8. region subtyping (invisible in surface syntax). Unlike the other items, it applies at every value flow, not only at coercion sites (`docs/spec/07-ownership-and-borrowing.md` N-B-9).

Coercions happen only at **coercion sites**: annotated `let` initializers, function arguments, `return` and tail expressions, struct field initializers, the right-hand side of assignments, array elements, and match arms and if branches when an expected type exists. At most one coercion (other than region subtyping) applies per site, coercions never chain, and they never apply below a type constructor (`docs/spec/05-types.md` §5.15.3).

There is **no** implicit conversion from `String` to `Str`; write `s.as_str()`. Numeric widening is never implicit either.

### D-TYPE-05 Type inference

- Function parameter and return types must always be annotated. An omitted return type means `()`. There is no global inference across functions.
- Function bodies are inferred with bidirectional type checking and unification. Closure parameter types can be inferred from the expected type.
- An integer literal whose type is not otherwise determined defaults to `Int`; a float literal defaults to `F64`.

### D-TYPE-06 Numeric semantics

- Integer `+ - *` **panic on overflow in every profile** (P8: profiles never change semantics). The wrapping operators `+% -% *%` and the core functions `checked_*`, `saturating_*`, and `wrapping_*` are provided.
- `/` and `%`: division by zero panics. For signed integers `MIN / -1` panics (overflow), while `MIN % -1` evaluates to `0` and does not panic, because the remainder is representable (`docs/spec/06-expressions-and-statements.md` N-C-8). The sign of `%` follows the dividend (same as Wasm `rem_s`).
- Shifts `<<` and `>>`: the right operand always has type `U32`. A shift amount greater than or equal to the bit width panics. `>>` on a signed integer is an arithmetic shift.
- Both operands of a binary arithmetic operator must have the same type. There is no mixing and no implicit widening.
- Unary `-` is not defined for unsigned integers. `-MIN` on a signed integer panics.
- Floats follow IEEE 754 with Wasm semantics. NaN bit patterns are unspecified. Float remainder is a library function.
- Conversions are core functions: lossless `T.from(x)`, checked `T.try_from(x) -> Option<T>`, and named lossy methods such as `x.wrapping_to<T>()`, `x.saturating_to<T>()`, and `x.trunc_to<T>()`.

### D-TYPE-07 Generics

- Type parameters `<T, U: A + B, const N: USize>` and `where` clauses. A const argument can only be a literal or the name of a const parameter. The name of a `const` item is not a const argument, and there is no const arithmetic in v0.1 (`CONST_ARG_NOT_SIMPLE`).
- Generics are monomorphized (draft §14). An implementation may share code between instances as long as observable semantics do not change.

### D-TYPE-08 Traits

- Traits support supertraits, associated types (`type Item`), default methods, and generic traits.
- Not in v0.1: associated consts, GATs, HKTs, specialization, negative impls, and auto traits.
- Receivers are explicit: `self: Self`, `self: borrow Self`, or `self: borrow mut Self` (as written in draft §15). There is no bare `self` shorthand.
- Associated types are written `Self.Item` / `T.Item`, and bound equalities as `I: Iterator<Item = I32>`.
- **Coherence (orphan rule)**: `impl Trait for T` is allowed only if `Trait` is defined in the current package, or the head type constructor of `T` is. A blanket impl `impl<T: X> Tr for T` may only be written in the package that defines `Tr`. Impls must not overlap; overlap is checked by syntactic unification of impl headers. For coherence purposes, `sai:core` and `sai:alloc` count as a single package.
- Method resolution prefers inherent methods over trait methods. Calling a trait method as `x.m()` requires the trait to be in scope (imported or in the prelude). Ambiguity is an error and must be resolved by writing `Trait.m(x)`.
- Derivable traits: `Copy Clone PartialEq Eq PartialOrd Ord Hash Default Arbitrary`. Only for distinct types, the operator traits `Add Sub Mul Div Rem Neg BitAnd BitOr BitXor BitNot` can also be derived; they forward to the base type.
- Copy must be derived explicitly for nominal types. Tuples and `Array` are Copy automatically when all their elements are, because they are structural types. A refined type `type R = B where p` is Copy exactly when `B` is, because its values are values of `B` (`docs/spec/05-types.md` §5.18).

### D-TYPE-09 Functions and closures

- Each function item has its own zero-sized type that implements the `Fn` traits. It coerces to a function pointer type `fn(A) -> B effects { .. }`.
- Closure syntax: `fn(x, y: I32) -> I32 { x + y }`; a forced move capture is written `move fn() { .. }`. The capture mode (borrow / borrow mut / move) is inferred from how the body uses each captured variable.
- A closure type implements `Fn` (called through `borrow Self`), `FnMut` (called through `borrow mut Self`), or `FnOnce` (called by consuming `Self`).
- There are no opaque return types (Rust's `impl Trait`) in v0.1. A closure is passed down as a generic parameter or used as a local value whose type is inferred.

---

## 4. Ownership and borrowing (OWN)

### D-OWN-01 Values

- A value of a Copy type is copied on each use. Any other value is **moved** by assignment, argument passing, and return. Using a value after it has been moved is an error.
- `owned T` is a synonym of `T`; it can be written to make intent explicit, for example for resources (draft §17).
- Partial moves of fields out of local variables are allowed, except out of types that implement `Drop`. Moving out through a borrow is forbidden; use `core.mem.replace`, `take`, or `clone`.
- Conditional moves use drop flags.

### D-OWN-02 Borrows are explicit at the call site

- A borrow is created with `borrow x` or `borrow mut x`, where the operand is a place expression. `borrow mut` requires the root binding to be `var`, or the place to go through a mutable view.
- Function arguments are never borrowed implicitly. Write `width(borrow image)` (P1). Omitting `borrow` is a type error, and a structured fix inserts it.
- **Exception (auto-ref)**: only the receiver of a method call is borrowed automatically, as required by the method's `self` type.
- **Auto-deref**: field access and method calls automatically go through `borrow T`, `borrow mut T`, and `Box<T>`. Every other dereference is written with prefix `*`. There is no user-definable Deref trait.
- A value of type `borrow mut T` used where `borrow mut T` is expected is reborrowed, not moved (D-TYPE-04 item 2).

### D-OWN-03 Regions without lifetime syntax

- The surface syntax has no lifetime annotations (draft §16.3). The specification and the implementation describe the rules internally with region variables written `'a`.
- **Type definitions**: if any field type syntactically contains a view type, or a type that carries a region (ignoring type parameters), the user-defined type has **exactly one implicit region parameter**. All its view fields share that region. Regions that arrive through type parameters belong to the type argument (the region of `Vec<Str>` belongs to `Str`).
- **Function signatures** (elision rules, independent of the function body):
  1. Each view position in the inputs gets its own independent universal region.
  2. Regions in the output are determined as follows:
     - (a) With a `borrows { p1, .., pk }` clause, the output region is outlived by the regions of the listed provenances.
     - (b) Without (a), if the `self` parameter carries regions, the output region is outlived by every region in the type of `self`.
     - (c) Otherwise, the output region is outlived by every input region. From the caller's side, all inputs stay borrowed while the result is in use.
  3. A provenance is written in one of two ways. A parameter name `p` means every region in the type of `p`. A field path `p.f.g` means only the regions in the type of that field. For example, `borrows { self.input }` refers to the region of the data that `Self` holds, not to the borrow of `self` itself.
- Regions inside function pointer types and `Fn`-family bounds are bound by that type (higher-ranked) and elided locally. Such types carry no free region, so a callback field does not give its type a region parameter (`docs/spec/07-ownership-and-borrowing.md` N-B-7).
- Provenance is never inferred from the function body. The signature alone determines the borrowing contract (P8; this enables modular checking, incremental compilation, and semantic diffs).
- With these rules, every SAI program translates into a subset of Rust with explicit lifetimes. Soundness is argued through this correspondence.

### D-OWN-04 Borrow checking

- Borrow checking uses NLL on MIR. A region is a set of CFG points, computed as the least solution of liveness and outlives constraints. An access that conflicts with a loan at a point where the loan is live is an error.
- Conflicts:
  - A shared loan conflicts with writes, moves, `borrow mut`, and StorageDead of the same or an overlapping place.
  - A mutable loan conflicts with every access.
  - A drop is a write-like access: it conflicts with shared and mutable loans of the dropped place (`docs/spec/07-ownership-and-borrowing.md` N-B-15).
- Two-phase borrows apply only to auto-ref'd `borrow mut` receivers, so that `v.push(v.len())` is accepted. The base of an `IndexMut` place and a `borrow mut` receiver that is reborrowed implicitly are auto-ref'd receivers in this sense (N-B-16).
- Outlives constraints are location-insensitive, as in NLL: a borrow returned on one path is live on every path from its creation (N-B-24).
- Consequence: a borrow returned from a function keeps its inputs borrowed according to the elision rules above. When safety cannot be proven, the user must transfer ownership, clone, or restructure (draft §16.3).

### D-OWN-05 Drop

- At the end of a scope, local variables are dropped in reverse declaration order. Struct fields are dropped in declaration order. Temporaries are dropped at the end of the statement that contains them.
- Temporary lifetime extension applies only when the whole initializer of a `let` or `var` is `borrow e` or `borrow mut e` with `e` a value expression (for example `let x = borrow make()`). It extends the temporary to the end of the enclosing block. Nothing nested is extended (`docs/spec/07-ownership-and-borrowing.md` N-B-6).
- `Drop.drop` cannot have effects (`effects {}`). Deallocation is not an effect.
- Fallible cleanup, such as flushing a file, is done with an explicit `close()` (P1).

### D-OWN-06 Resources

- A `resource` is a nominal, affine type with identity. It is never Copy, and Copy cannot be derived for it. Dropping it runs its `Drop` impl, or the resource-drop of an imported handle.
- Resources map one-to-one to WIT resources when exported or imported.
- Linear types (types that must be consumed explicitly) are a future extension.

### D-OWN-07 Allocation regions (arenas)

- `region r { .. }` is a block that introduces an arena `r` (of type `borrow Arena`) and frees the whole arena at the end of the block.
- Allocation in the arena is explicit: `r.alloc(v) -> borrow mut T`, `r.alloc_str(s) -> Str`, `r.alloc_slice(s) -> Slice<T>`. Implicit allocations inside the block are never redirected to the arena (P1).
- In v0.1, only `T: Copy` values can be allocated in an arena, which guarantees that they have no drop glue.
- Borrows obtained from the arena are tied to the arena's region, so the ordinary borrow checker rejects any escape from the block (draft §19).
- An arena obtains chunks from the global allocator, so using one has the `alloc` effect.
- `alloc` hands out `borrow mut T` through a shared `borrow Arena`. This is sound because each allocation is fresh memory that nothing else refers to; the library implements it with unsafe code (`docs/spec/16-core-library.md` §16.15.5).
- The arena's own fields (its chunk list and position) change under that shared borrow. `Arena` is the one exception to "no interior mutability" (`docs/spec/12-unsafe-and-ffi.md` §12.6.4 rule 5). It is not observable, because the fields are private and every method has the `alloc` effect, so no pure function and no fact of the Standard Fact Checker depends on them. The alternative, keeping the state behind a pointer to separately allocated memory, would make every `region` block allocate even when nothing is allocated in it (P7), and a general cell type would add interior mutability to the language (P8).
- A "borrow region" (a region variable) and an "allocation region" (a `region` block) are different concepts, and the documents always distinguish the two terms.

---

## 5. Effects and capabilities (EFF / CAP)

### D-EFF-01 Declaring and inferring effects

- An effects clause `effects { a, b }` declares an upper bound on the effects a function may perform.
- **An omitted effects clause always means "infer".** This meaning never depends on visibility.
- For the following functions, if the inferred effect set is non-empty, it MUST be written explicitly (error `MISSING_EFFECTS_DECLARATION`; a structured fix inserts the inferred set):
  - `pub` functions of packages whose version is 1.0.0 or later
  - `export` functions
  - functions exposed through a world or an interface
  - This is how draft §20.1's "required for stable APIs" is realized. Functions with an empty effect set may omit the clause, so pure functions never need one (consistent with draft §20's "pure" example).
- For `pub` functions of packages below 1.0.0, inference is allowed, and the lint `PUBLIC_EFFECTS_INFERRED` (warning) fires when the inferred set is non-empty.
- An effects clause on a trait method declaration (without a body) is an upper bound for every impl. Without one, there is no bound, and the method's effects are treated as latent.
- Inference is a fixed-point computation over the SCCs of the call graph, performed on MIR after drop elaboration.

### D-EFF-02 Latent effects (effects reached through type parameters)

- In the body of a generic function `g<T>`, the effects reached through the type parameter `T` are called **latent(T)**. They come from calls to trait methods in `T`'s bounds, and from calls of `T` itself when it is an `Fn` type. latent(T) is not part of `g`'s own effects; it is **attributed to the caller that instantiates `T`**.
- The effects of a call `g<C1..Cn>(..)` are `E(g) ∪ latentᴮ(C1) ∪ .. ∪ latentᴮ(Cn)`. Here latentᴮ(C), for the bound B that `g` puts on C, is the union of the effects of the methods in `impl B for C`, plus the effects of calling C if C is callable. Nested type arguments (`Map<Filter<I, F1>, F2>`) are handled recursively.
- An effects clause on a bound caps those effects: `F: Fn(I32) -> I32 effects {}`, `T: Iterator<Item = I32> effects { alloc }`.
- A function pointer type states its effects as part of the type. Without an effects clause, a function pointer type is pure.
- There is no syntax for effect variables, yet higher-order functions such as map, filter, and for_each are effect-polymorphic (P8, P9).

### D-EFF-03 Built-in effect vocabulary (v0.1)

In effect position, the following root names always refer to built-in effects; they take precedence over module aliases. The roots are `alloc fs net clock random stdio env process thread host ffi`. An import binding with one of these names cannot be named as an effect and must be renamed with `as`, and a user-defined effect cannot be named after a root (`EFFECT_NAME_RESERVED`, `docs/spec/08-effects-and-capabilities.md` §8.2.2).

| Effect | Meaning | Capability |
|---|---|---|
| `alloc` | heap or arena allocation | `alloc` |
| `fs.read` | reading files and directories, metadata | `wasi:filesystem` |
| `fs.write` | creating, modifying, deleting | `wasi:filesystem` |
| `net.connect` | sockets, DNS | `wasi:sockets` |
| `net.http` | outgoing HTTP | `wasi:http` |
| `clock.read` | wall clock, monotonic clock, waiting | `wasi:clocks` |
| `random.read` | randomness | `wasi:random` |
| `stdio.read` | standard input | `wasi:cli` |
| `stdio.write` | standard output and error | `wasi:cli` |
| `env.read` | arguments and environment variables | `wasi:cli` |
| `process.exit` | exit with a status code | `wasi:cli` |
| `process.spawn` | spawning processes (native only) | `process` |
| `thread.spawn` | spawning threads | `threads` |
| `host` | calling raw imports declared with `extern "wasm"` | `host` |
| `ffi` | calling native FFI | `native_ffi` |

- **Interface effects**: calling an imported WIT function has the effect `ns:pkg/iface`. A function that declares a built-in effect E may contain the interface effects of the package that cap(E) names (for example, a function declaring `fs.read` may call `wasi:filesystem/*`). In source code an interface effect is named by its import binding (`use wasi:http/outgoing-handler` binds `outgoing_handler`, written `effects { outgoing_handler }`).
- **User-defined effects** are declared as `pub effect write covers { net.connect, alloc }` and referred to by module path, such as `db.write`. Inside the defining package, a function that declares `db.write` may perform the effects in its covers set. Outside the package the effect is opaque. Capability checking expands covers sets recursively.
- Panics are not tracked as an effect.

### D-CAP-01 Capability policy

- There are three kinds of capability names:
  - `alloc`
  - a WIT package name (`wasi:http`) or interface name (`wasi:filesystem/types`); a package name includes all of its interfaces
  - target capabilities: `threads` `host` `native_ffi` `process` `memory64`
- `allow { }` in `sai.package` is the maximum grant. **An omitted `allow` means the empty set** (P1, P5).
- `deny` overrides `allow`. It may deny part of an allowed capability (allow `wasi:http`, deny `wasi:http/outgoing-handler`). Listing the same name in both is an error.
- Checking: the effects of all code reachable from the root (after DCE) are expanded to capabilities. They must be included in `allow` and disjoint from `deny`, and this must hold for the policy of the root package and for the policy of every dependency. When a dependency is a Wasm component, all of its imports are treated as reachable.
- Reaching a capability that the target does not provide is a compile error (draft §59).

### D-CAP-02 Ambient authority

- There is no global mutable state: no `static` and no `static mut`. Constants exist.
- Code can affect the outside world only through import functions (WIT or `extern "wasm"`), whose effects are tracked and checked against policy.
- The standard library is capability-passing: it takes capability values such as `FileSystem`, `HttpClient`, and `Clock`. Importing a WASI interface directly from application code triggers the lint `AMBIENT_IMPORT` (warning), which `policy { deny ambient_imports }` turns into an error.
- An application entry point receives a bundle of capabilities: `pub fn main(env: owned process.Env) -> process.ExitCode`. Which capabilities the bundle carries is determined by `allow` (`docs/targets/targets-and-profiles.md`).

---

## 6. Expressions, statements, and control flow (EXPR)

### D-EXPR-01 Block values

- The value of a block is the value of its last statement if that statement is an expression statement.
- When the expected type is `()` (statement position, or the body of a function returning `()`), the value of the last expression is discarded.
- Discarding values: discarding a value of a type marked `@must_use` (`Result` and the futures of `async fn` calls and `async` blocks are; `Option` is not; [spec/15](../spec/15-attributes-and-lints.md) §15.5.1) is the error `UNUSED_MUST_USE`. `let _ = e` discards explicitly. An expression statement without side effects produces a warning.

### D-EXPR-02 Evaluation order

- Evaluation is left to right: operands, arguments (receiver first), record fields (in source order), array elements.
- The exception is the assignment statement `place = e`: **the right-hand side is evaluated first**, then the place is evaluated and written. A compound assignment `place op= e` means `{ let t = e; place = place op t }`, where the subexpressions of `place` are evaluated only once.
- `and` and `or` short-circuit.

### D-EXPR-03 Control flow

- `if c { } else if c2 { } else { }`, `if let P = e { } else { }`, `while c { }`, `while let P = e { }`, `for P in e { }`, `loop { }`, `break [label] [value]`, `continue [label]`, `return [e]`.
- `let P = e else { <diverging block> }` (let-else).
- Labels: `'outer: loop { .. break 'outer }`.
- An expression in condition position (`if`, `while`, `for`, `match` scrutinee) must not start with `{`; parenthesize a block expression used as a condition. There is no expression form with a postfix `{`, so a `{` after a complete condition always starts the body.

### D-EXPR-04 match

- Arms are written `pattern [if guard] => expr` and separated by NEWLINE (or `,`). Or-patterns `A | B`, binding patterns `name @ pat`, ranges `1..=5`, and slice patterns `[a, b, ..rest]` are supported.
- Exhaustiveness is always required (error). An unreachable arm is a warning. Integer exhaustiveness is judged against the natural range of the type; refinements are not taken into account (v0.1).
- Binding modes are explicit: `borrow x`, `borrow mut x`, and `var x` in patterns. Rust 2018's implicit binding modes (match ergonomics) are not adopted (P1). A scrutinee of type `borrow T` is not dereferenced automatically either; write `match *b { .. }`. A binding that would move a non-Copy part out from behind a borrow is an error, and a structured fix suggests `borrow x`.

### D-EXPR-05 The `?` operator

- `?` applies to `Result<T, E>` in a function returning `Result<U, E>`, and **the error types must be identical**. There is no implicit `From` conversion (P1). Convert error types with `.map_err(f)?`.
- The same applies to `Option<T>`. `Option` and `Result` cannot be mixed.
- Inside an `async` function, write `e.await?`.

### D-EXPR-06 assert and panic

- `assert cond` and `assert cond, "message"` are statements, checked in every profile.
- `panic(msg: Str) -> Never` is a core prelude function, and so is `unreachable() -> Never`.

### D-EXPR-07 Constants

- `const NAME: T = <const expression>`. A const expression consists of literals, arithmetic and logical operators, construction of structs, enums, tuples, and arrays, references to other constants, and `Str` literals. There is no const fn in v0.1.

### D-EXPR-08 Temporaries and drop order

As in D-OWN-05.

### D-EXPR-09 Ranges

- `a..b` and `a..=b` construct the core types `Range<T>` and `RangeInclusive<T>`. `..b` and `a..` are allowed too.

### D-EXPR-10 Indexing

- `a[i]` on `Array`, `Slice`, and `SliceMut` is a built-in place expression. Indexing `Vec`, `String`, and `Str` goes through the traits `Index` / `IndexMut`.
- Bounds are always checked; out-of-range indexing panics. Indexing a `Str` with a range panics unless both ends fall on UTF-8 character boundaries.
- Range indexing (`s[1..3]`, `xs[a..b]`) yields a view (`Str`, `Slice<T>`, `SliceMut<T>`). The view is computed into a temporary that serves as the place, with the region of the borrow of the indexed value; because shared views are Copy, `s[1..3]` read as a value is a `Str` (`docs/spec/16-core-library.md` N-J-14).

### D-EXPR-11 for loops

- `for x in xs` consumes an `IntoIterator` by value. To iterate by borrow, write `for x in xs.iter()`, or `for x in borrow xs` (via `IntoIterator for borrow Vec<T>`).

### D-EXPR-12 Format strings (Stage 3)

- `f"Hello {name}!"` desugars to a sequence of calls on the core trait `Format` (writes into a `core.fmt.Writer`). Producing a `String` from an f-string has the `alloc` effect. Format specifiers are named, as in `{x:hex}` (details in the core library document).

### D-EXPR-13 String concatenation

- `Str + Str -> String` and `String + Str -> String` are provided as `Add` impls in alloc. Both have the `alloc` effect. `String + Str` reuses the left-hand buffer.
- There is no `Str + String` or `String + String`; convert the right-hand operand with `.as_str()`. This follows from D-TYPE-04, which has no implicit `String` to `Str` coercion.

---

## 7. Contracts and refinements (CONTRACT)

### D-CONTRACT-01 How each kind of check is handled

| Kind | Proven statically | Not proven |
|---|---|---|
| Conversion to a refined type (`B` to `R`) | no runtime code | **compile error** (explicit `R.try_from(v)` required) |
| `requires` (caller's obligation) | no runtime code | checked at runtime at the call site; panics on violation |
| `ensures` (callee's obligation) | no runtime code | checked at runtime at each return; panics on violation |
| `invariant` | no runtime code | checked at runtime at each check point; panics on violation |
| Trust boundary (values received by a component export, or other external input not marked trusted) | n/a | refinements and invariants are validated automatically at runtime; a failure panics and therefore traps the instance (a value that is not even a valid value of its WIT type traps during lifting) |

- Runtime checks **remain in every profile**. The optimizer may remove a check only when it has a sound proof that the check cannot fail.
- An obligation that falls back to a runtime check is reported by the lint `CONTRACT_RUNTIME_CHECK` (info). `policy { deny runtime_contracts }` turns this into an error.
- This follows the draft's distinction between §13 and §23: refinements require validation, while contracts fall back to runtime validation where needed.

### D-CONTRACT-02 The Standard Fact Checker (SFC)

- Program **acceptance** (whether a conversion to a refined type is accepted) is decided exclusively by the SFC. Implementations may use stronger provers, but only for optimization (removing checks).
- Facts available to the SFC:
  - the `requires` clauses of the current function
  - the predicates of refined-type variables in scope
  - branch conditions that dominate the current point (`if`, integer patterns in `match`)
  - linear definitions of immutable bindings `let x = e`
  - the `ensures` clauses of called functions
  - the invariants of values whose types have invariants
  - the value ranges of integer types
  - conditions established by a successful `assert`
- Facts about a `var` are invalidated at assignments and at loop heads. Loop invariants are not inferred.
- Decision procedure: facts are normalized into linear inequalities, and **Fourier–Motzkin elimination over the rationals** is applied, with integer tightening of strict inequalities (`x < c` becomes `x ≤ c - 1`). If the facts plus the negated goal are unsatisfiable, the goal is proven.
- The variable elimination order and the limits (256 atoms, 16 variables, 16 DNF branches) are fixed by the specification. They are counted after relevance filtering: distinct atomic formulas (256), distinct integer variables (16), cubes after each multiplication (16), and constraints after each elimination step (256), so unrelated facts never cause a limit to be reached (`docs/spec/09-contracts-and-refinements.md` N-C-64). Exceeding a limit deterministically yields "Unknown" (not proven).
- Facts about floats are not used. Non-linear terms are treated as opaque atoms.
- Distinct types over integer bases: the SFC treats a distinct value as its base value when the operator involved (comparison or arithmetic) is the derived, forwarding implementation (D-TYPE-08). The constructor `D(e)` and the accessor `d.value` are the identity for the SFC. This is what lets `ensures result >= Money(0)` be proven from `requires amount <= balance` (P2). A hand-written operator impl is an opaque function call.
- Details are in `docs/spec/09-contracts-and-refinements.md`.

### D-CONTRACT-03 Predicate language

- Predicates in refinements, contracts, and invariants are `Bool` expressions without effects (an empty effect set; no `alloc` either).
- Contextual keywords: `self` in refinements and invariants; `result` and `old(e)` (the value at function entry) in `ensures`.
- Predicates the SFC does not understand are allowed. They are simply never proven statically: converting to a refined type then needs explicit validation, and contracts become runtime checks.

### D-CONTRACT-04 Invariants

- `invariant T { pred* }` lists predicates that must all hold. The fields of a type with an invariant can be written **only from within the module that defines the type** (not even from other modules of the same package).
- Check points: every construction expression, and every return from a function of the defining module (for parameters of type `borrow mut T` and `T`, and for return values).
- **Dirty places.** Inside the defining module, a place of an invariant type becomes dirty at a field write, at a `borrow mut` of a field, and at a `borrow mut` of the whole value that is not a direct call argument. A dirty place is checked before each use as a whole and, if it is rooted in a parameter or a capture, at each return point. Without these extra check points a broken value could escape through locals and nested places, and invariant facts would be unsound (`docs/spec/09-contracts-and-refinements.md` §9.4.3, N-C-53).

---

## 8. Errors and panics (ERR)

### D-ERR-01

- Recoverable errors are `Result`s. There are no exceptions.
- A panic lowers to a trap on every target (no unwinding in v0.1).
- In the dev and release profiles, if the host provides a reporting channel (WASI stderr, or an import through `@panic_hook`), the panic message and location are reported before trapping. In tiny the message is discarded and the program just traps.
- A panic does not propagate across a component boundary as an error. A trap stops the component instance.

---

## 9. Async and concurrency (ASYNC) — Stage 5

### D-ASYNC-01 Futures and implicit pinning

- Calling `async fn f() -> T` returns a value of an anonymous state machine type that implements `Future<Output = T>`. Futures are lazy and do nothing until awaited.
- `Future` is a core trait whose poll method is **unsafe**:
  `unsafe fn poll(self: borrow mut Self, cx: borrow mut Context) -> Poll<Self.Output>`.
  Its safety contract: once `poll` has been called on a value, that value must not be moved again until it is dropped.
- Safe code can drive a future only through `.await` (and `for await`). The awaited future lives in a hidden slot of the awaiting frame, and safe code can never name or move it. Executors and combinators (`block_on`, `join`, `select`, `scope` of the package `sai:async`) live in the standard library and uphold the contract with unsafe code.
- As a result, **borrows of frame-local values may be held across `.await`**, so `http.get(url.as_str()).await` works, and **there is no `Pin` type** (P9: natural code, P8: no wrapper types). Self-references only appear after the first poll, and by then the frame no longer moves.
- A future that has not yet been polled is an ordinary movable value.
- Effects of async code: calling an `async fn` only constructs the future and has no effects. The effects of its body (declared or inferred, D-EFF-01) are carried by the anonymous future type and are incurred by the expression that drives it (`.await` / `for await`). Executors and combinators are generic over `F: Future`, so for them these effects are latent effects of `F` and are attributed to the instantiating caller (D-EFF-02).
- Structured concurrency: child tasks can be spawned only through the `Scope` that `scope(fn(s) { .. })` of `sai:async` hands to its body (`s.spawn(..)`), and the scope waits for all children before it ends. Tasks cannot be detached. Dropping a future cancels it.
- The `Stream` trait (`unsafe fn poll_next`, with the same contract) and `for await x in s { }`.
- The scheduler is a library (`sai:async`), linked only when used. When the Component Model async ABI (callback form) is available, async code lowers to it.

### D-THREAD-01

- Threads are gated by the target capability `threads` and the effect `thread.spawn`. They are not implemented in v0.1.
- `Send` and `Share` are reserved as names of future auto traits.

---

## 10. Unsafe and FFI (UNSAFE)

### D-UNSAFE-01

- `unsafe { }` blocks, `unsafe fn`, and the file-level `unsafe module` header.
- Unsafe operations: dereferencing the raw pointers `Ptr<T>` / `PtrMut<T>`, calling `unsafe fn`s (including `Future.poll`), calling `extern "C"` functions, intrinsics such as `core.mem.transmute`, and implementing an `unsafe trait`.
- `sai inspect unsafe` MUST be able to list every use of unsafe.
- `extern "wasm" module "env" { fn log(v: I32) }` declares core Wasm imports. Signatures are restricted to scalar types. Calling such a function is not unsafe, but it has the `host` effect and needs the `host` capability (denied by default in the tiny profile, draft §46.1).
- `unsafe extern "C" { .. }` is native-only (Stage 7). A package that uses it is marked non-portable.

---

## 11. Components and WIT (WIT)

### D-WIT-01 Interfaces and worlds

```sai
interface processor {
    enum ImageError {
        TooLarge
        Unsupported
    }
    resource Image {
        fn new(data: Bytes) -> Image
        fn width(self: borrow Self) -> U32
    }
    fn resize(image: Bytes, width: U32, height: U32) -> Result<Bytes, ImageError>
}

impl processor {
    type Image = MyImage // binds the abstract resource to a concrete resource type
    fn resize(image: Bytes, width: U32, height: U32) -> Result<Bytes, processor.ImageError> { .. }
}

world image_service {
    import wasi:http/outgoing-handler
    export processor
}
```

- An interface contains types (records, enums, variants, flags, resources, aliases) and functions, and maps one-to-one to a WIT interface. A nominal type defined outside an interface cannot appear in the interface's signatures. Signatures can use primitive types; `Option`, `Result`, tuples, `Vec`, `String`, and `Bytes`; and types of other interfaces (`other.T`, corresponding to WIT `use`).
- `impl <interface>` is the implementation used to export that interface. A package may contain at most one impl per interface.
- An imported interface is bound with `use ns:pkg/iface`, just like a module.
- Names map between `snake_case` / `UpperCamelCase` and WIT `kebab-case`. The mapping must be a bijection; a collision is an error.
- The type mapping table and the lowering to the Canonical ABI are defined in `docs/spec/13-components-and-wit.md` and `docs/impl/07-component-lowering.md`. These cannot cross a component boundary: `ISize`, `USize`, function types, non-resource return types that contain borrows, `Never`, and `Array` (when WIT fixed-length lists are unavailable).

### D-WIT-02 Consuming WIT

- The compiler understands WIT directly (draft §37). The `wit/` directory of a Wasm package becomes visible to SAI code as declarations, as is.
- `sai bindgen wit` generates `.wit` files from SAI interfaces and worlds. `sai bindgen wit --show <pkg>` shows how a WIT package looks from the SAI side.

### D-WIT-03 World imports and exports are per interface

- `import` and `export` in a world name individual interfaces (`import wasi:http/outgoing-handler`), never a whole package (`import wasi:http`). A world is the concrete contract of a component, so it must be explicit (P1). Coarse grants at the package level belong to the capability policy (`allow { wasi:http }`), not to worlds.
- The draft's §43 example `import wasi:http` is revised accordingly (`docs/design/draft-reconciliation.md`).

---

## 12. MIR (MIR) — details in `docs/impl/04-mir.md`

### D-MIR-01 MIR skeleton (names shared by all documents)

- `Body { locals: [LocalDecl], blocks: [BasicBlock], arg_count, return_local: _0, source_scopes, phase }`
- `Place = Local + [Projection]`, with projections `Field(idx)` `Deref` `Index(Local)` `ConstIndex{offset, from_end}` `Subslice{from, to}` `Downcast(variant)`.
- `Operand = Copy(Place) | Move(Place) | Const(Constant)`
- `Rvalue`: `Use(op)` `Borrow(region, Shared|Mut|TwoPhaseMut, place)` `RawPtr(Const|Mut, place)` `BinaryOp(op, a, b)` `CheckedBinaryOp(op, a, b)` `UnaryOp(op, a)` `Convert(kind, op)` `Aggregate(kind, [op])` `Discriminant(place)` `Len(place)` `Repeat(op, n)` `Closure(def, [op])`
- `Statement`: `Assign(place, rvalue)` `StorageLive(l)` `StorageDead(l)` `SetDiscriminant(place, v)` `FakeRead(cause, place)` `Nop`
- `Terminator`: `Goto` `SwitchInt{discr, targets, otherwise}` `Return` `Unreachable` `Call{func, args, dest, target, effects}` `Drop{place, target}` `Assert{cond, expected, kind, target}` `Await{future, dest, resume}` (only before the async transform) `Trap{kind}`
- `AssertKind` (panic causes): `Overflow(op)` `DivByZero` `RemByZero` `BoundsCheck` `ShiftOverflow` `Contract(id)` `Refinement(id)` `Invariant(id)` `Unwrap` `UserAssert` `ExplicitPanic`
- There is no unwinding, so `Call` and `Drop` have no cleanup edges.
- MIR phases: `Built` → `DropElaborated` → `Analyzed` (borrowck, effects, and SFC done) → `Optimized` → `Mono` (after monomorphization; the backend input).
- Borrow checking and effect inference run on generic MIR (`DropElaborated` / `Analyzed`). The backend consumes `Mono`.

---

## 13. Wasm lowering and memory layout (WASM) — details in `docs/impl/06-wasm-lowering-and-layout.md`

### D-WASM-01 Baseline

- The baseline for wasm32 and wasm64 is Wasm 2.0 (multi-value, bulk memory, sign extension, non-trapping float-to-int, mutable globals, reference types).
- Tail calls and SIMD are opt-in features. wasm64 requires memory64.
- The GC and exception-handling proposals are not used (no mandatory GC or exception runtime).

### D-WASM-02 Linear memory layout

```
0                 8                8+S              data_end          heap →
| null guard (8B) | shadow stack ↓ |  static data   |  heap (bump/free-list, memory.grow) |
```

- The stack comes first (stack-first layout). A function that uses the shadow stack checks in its prologue whether `sp - frame < 8`, and traps if so. This check is present in every profile, because memory safety must not depend on the profile.
- No object is ever placed at addresses 0–7, so the value 0 can serve as a niche for `borrow`, `Box`, and `Ptr`.
- **A module that never loads or stores memory, and needs no memory at its export or import boundary, has no memory section.** The shadow stack (and the `__sp` global) exists only if some function needs it.

### D-WASM-03 Layout (compiler-private; not stable across compiler versions)

- Primitive types have their natural size and alignment. `Bool` is 1 byte (values 0/1; niches 2..=255). `Char` is 4 bytes (niches: 0x110000 and above, and the surrogate range).
- Struct and tuple fields are laid out after a stable sort by decreasing alignment, ties broken by declaration order. `@repr(C)` uses declaration order and C rules.
- Enum layout: a tag (the smallest unsigned integer that can hold every discriminant) plus a union of payloads. Niche filling applies when exactly one variant has a payload and that payload has enough niches (`Option<borrow T>` is a single pointer).
- `Str`, `Slice<T>`, and `SliceMut<T>` are `(ptr: USize, len: USize)`. `String` and `Vec<T>` are `(ptr, len, cap)`. `Box<T>` is a pointer.
- A closure is a struct whose fields are its captures. An imported resource handle is an `I32`. The rep of an exported resource is a `Box` pointer.

### D-WASM-04 Internal calling convention (compiler-private)

- An aggregate with at most 4 scalar leaves is flattened, both as an argument and as a return value (using multi-value). With 5 or more leaves, an argument is passed as a pointer to caller-allocated memory and a return value through an sret pointer.
- A `borrow T` of a small Copy type may be passed by value, because safe code cannot observe the difference.
- The exception is a core `export fn`, which may use scalar types only. Rich types are exported through a component build.

### D-WASM-05 Allocator

- A program that never reaches `alloc` links no allocator.
- The default allocator is a small size-class free-list on top of `memory.grow`. The tiny profile uses an even smaller bump + free-list allocator.
- `allocator <pkg>` in the root `sai.package` replaces the allocator.

---

## 14. Packages, resolver, and lock file (PKG) — details in `docs/package/*`

### D-PKG-01 `sai.package` is a small declarative language

- One file describes one package. It has no arbitrary code execution, conditionals, variables, or includes (draft §31).
- Declarations: `package`, `compiler`, `source`, `wit`, `requires`, `requires test`, `targets`, `allow`, `deny`, `policy`, `lints`, `export world`, `artifact`, `profile`, `allocator`, `overrides`, and the metadata declarations `description`, `license`, `authors`, `repository`. `requires test`, `lints`, `profile`, `allocator`, and `overrides` are effective only in the root package. Metadata never affects resolution or building.
- `source` has no default and `targets` is required (`docs/package/01-package-contract.md` N-G-6, N-G-7). Every requirement names its source with `from`, including the bundled standard packages (N-G-9).
- A requirement may carry `grant { .. }`, the maximum set of capabilities given to that dependency and its own dependencies (N-G-12).
- `export world` may be declared by a package whose `targets` include a core target. A core build then emits a core module from the `export fn` items with the information diagnostic `EXPORT_WORLD_NOT_APPLIED`, and `sai component build` wraps it with the world's exports (N-G-8, `docs/spec/13-components-and-wit.md` §13.6.2).
- A version requirement always has an operator: `^1.4`, `~1.4.2`, `=1.4.2`, `>=1.2, <2.0`. A bare version without an operator is an error (P1).
- Dependency sources: `from git "<url>" [dir "<subdir>"]` and `from path "<dir>"`. Registries are a future extension.
- Target-specific dependencies: `sai:wasi @ ^0.2 from git ".." only { wasm32-wasi, wasm64-wasi }`.

### D-PKG-02 Resolver: Minimal Version Selection (MVS)

- Compatibility lines: the major version for versions 1.0.0 and later, the minor version for `0.y`, and the exact patch for `0.0.z`. **Different compatibility lines of the same package are separate nodes and may coexist** (warning `DUPLICATE_PACKAGE_LINE`; `policy { deny duplicate_lines }` makes it an error).
- For each compatibility line, the resolver selects **the maximum of the minimum versions requested** (Go's MVS). Lock file entries are treated as additional requirements from the root, but a lock floor applies only to a node that some edge reaches during selection, so removed requirements leave the lock (`docs/package/02-resolver.md` N-G-24). When the lock is consistent with the root's contract it is used unchanged and no selection runs (N-G-29).
- After selection, every requirement's range (including `<` and `=`) is checked. A violation is the error `VERSION_CONFLICT`, which explains the chain of requirements. **There is no backtracking** (P8).
- Git: the resolver fetches the tag `v<version>` (or `<dir>/v<version>` when `dir` is given). If the identity and version declared in the fetched `sai.package` do not match the request, it is an error. MVS only needs the versions that are actually requested, so no index is needed. Only `sai update` and `sai add` list tags (ls-remote; N-G-31). There are no branch or commit sources (N-G-27) and no yanking (N-G-53).
- Source URL selection: the root's `overrides` come first. Otherwise, the `from` of the requirer that requested the selected version is used, with ties broken by the lexicographic order of the requirers' identities.
- No scripts run during dependency resolution. Git hooks, submodules, and LFS are disabled.

### D-PKG-03 Lock file

- `sai.lock` uses the same lexical rules as `sai.package` in a deterministic, line-oriented declarative form, which keeps diffs and merges easy.
- Each entry records: the version; the source (URL, dir, commit); the source checksum; the WIT checksum; artifact checksums (per target); the resolved capabilities; the versions of dependencies; and compiler compatibility.
- A checksum is `sai1:` followed by a SHA-256 digest written as 64 lowercase hexadecimal digits (`docs/package/03-lock-file.md` N-G-38). A tree hash is the SHA-256 of the concatenation, over files sorted by the bytes of their paths, of lines `sha256(content) <two spaces> <path>\n`. The source and WIT checksums are tree hashes; an artifact checksum is the SHA-256 of the artifact file (N-G-39).
- A build is rejected if fetched content does not match the lock file (draft §73).

### D-PKG-04 Distribution of the standard library

- `sai:core` and `sai:alloc` ship with the toolchain and are always implicitly available; they are never declared as dependencies. Language desugarings depend only on core and alloc.
- `sai:collections`, `sai:text`, `sai:fmt`, `sai:io`, `sai:fs`, `sai:net`, `sai:http`, `sai:async`, and `sai:process` are ordinary packages declared in `requires`. The toolchain ships a pre-populated cache of them, so using the standard packages needs no network access.

### D-PKG-05 Choosing between source and artifact

- For a hybrid package, the form is chosen by deterministic rules:
  - Use the source if the profile is `tiny`, if the target is a core target, or if there is no artifact for the target.
  - Otherwise prefer the source as well. The root can force the artifact with `overrides { acme:image use artifact }`.
- Both forms provide the interfaces of the package's export world. In source form the calls are linked inside the root's module through Canonical ABI glue, so the choice does not change what the build provides or requires (spec/13 N-D-104).
- The chosen form is shown in the build metadata (`sai size`, `sai inspect`).

---

## 15. Targets and profiles (TGT) — details in `docs/targets/targets-and-profiles.md`

### D-TGT-01

- `wasm32-core` / `wasm64-core`: produce a core Wasm module, freestanding, without WASI.
- `wasm32-wasi` / `wasm64-wasi`: **produce a component targeting WASI 0.2 or later**. WASI preview1 is out of scope. wasm64-wasi is experimental because it depends on memory64 support in the Component Model.
- `sai component build` can also wrap the output of a core target as a component (with WIT exports only, no WASI).

### D-TGT-02 Profiles

| Item | dev | release | tiny |
|---|---|---|---|
| Optimization | minimal | maximal (speed) | maximal (size) |
| Overflow / bounds / contract checks | always on | always on | always on |
| Panic | report + trap | report + trap | trap only |
| Debug info (DWARF) | yes | no | no |
| Name section | module, function, local and global names | module and function names | none |
| DCE / whole-program / LTO | partial | yes | maximal |
| Allocator | default | default | tiny allocator |
| Denied by default | none | none | WASI, host, threads, dynamic linking, reflection |

- A profile can be extended in `sai.package` with `profile <name> { allow { .. } budget { wasm <= 16 KiB } }`. User-defined profiles do not exist in v0.1.
- A profile `allow` can only lift that profile's default denials, and only within the package `allow`, which stays the maximum grant (D-CAP-01, `docs/package/01-package-contract.md` N-G-15). Profile blocks are effective only in the root package.
- `tiny` denies the WASI capabilities on WASI targets too. A tiny command that writes to standard output therefore declares `profile tiny { allow { wasi:cli } }`; the `wasi:io` interfaces it needs are implied by `wasi:cli` ([targets](../targets/targets-and-profiles.md) §3.3, §5.9).
- The full table of profile properties, with the section that implements each, is [targets](../targets/targets-and-profiles.md) §5.2.

---

## 16. Tooling (TOOL)

### D-TOOL-01 Diagnostics

- Every diagnostic has a stable code in UPPER_SNAKE_CASE. A code, once assigned, is never reused.
- Diagnostics are emitted both as human-readable text and as JSON (`--format json`). Fixes are structured and refer to semantic node IDs.
- Lint names and diagnostic codes share one namespace (`@allow(UNUSED_VARIABLE)`).

### D-TOOL-02 Symbol identity

- A symbol ID has the form `symbol://<ns>:<pkg>/<module path>/<item>[/<member>]#<6 hex digits>`. The hex part is computed deterministically from the symbol's canonical path when the symbol is first seen (a hash of the path, with probing on collision), and persisted in `.sai/index`; it survives renames and moves. The exact computation is in [impl/12](../impl/12-semantic-tooling.md) §12.2.3.
- A node ID has the form `<kind>:<hex>` and is valid within one document snapshot (version).

### D-TOOL-03 Additional CLI commands

- In addition to the commands of draft §77: `sai lsp` (LSP plus SAI extensions), `sai explain <CODE>`, and `sai inspect {unsafe|mir|effects|capabilities|layout|wit|deps}`.
- Build outputs go to `out/<target>/<profile>/` and tooling state (index, cache) to `.sai/`, kept separate (draft §65).

### D-TOOL-04 Formatter

- 4-space indentation, LF line endings, a maximum line width of 100, and exactly one trailing newline. There are no settings (draft §4.3).
- The formatter is idempotent and preserves comments.

### D-TOOL-05 Implementation language (informative)

- The reference implementation should be written in Rust, so that it can use the Bytecode Alliance crates `wasm-encoder`, `wasmparser`, `wit-parser`, `wit-component`, and `wasmtime`.
- Git operations use `gix` (pure Rust); the system git and hooks are never invoked.
- Self-hosting in SAI is a long-term goal.

---

## 17. Revision history

| Date | Change |
|---|---|
| 2026-10-05 | Initial version: decisions made to turn draft v0.1 into a formal specification. Includes the revised async model (D-ASYNC-01: implicit pinning with an unsafe `poll`), per-interface world imports (D-WIT-03), re-exports (D-MOD-04), statement restrictions (D-SYN-10), and string concatenation (D-EXPR-13). |
