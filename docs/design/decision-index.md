# Index of Detailed Decisions

**Status:** Informative (generated from the decision tables of the documents)

The detailed documents record the decisions they make in a decisions section near their end, under IDs of the form `N-<group>-<n>`. The group letter names the set of documents that the decision belongs to, and the number is unique within the group. An ID is never reused. This index lists every such ID with the document and section that define it. The defining document decides; the summary here is its first sentence only, and a section reference in a summary is to the defining document.

The decisions of [decisions.md](decisions.md) have IDs of the form `D-<AREA>-<NN>` and rank above every `N-` decision ([decisions.md](decisions.md) §0).

## N-A

Defined in spec/04, spec/05, spec/15.

| ID | Defined in | Summary |
|---|---|---|
| N-A-1 | [spec/04](../spec/04-modules-and-names.md) §4.11 | The package identity always comes from the `package` field of `sai.package`, including in toolchains that implement only Stages 0–2. |
| N-A-2 | [spec/04](../spec/04-modules-and-names.md) §4.11 | The module tree is computed by §4.2.3: byte-ordered walk, entries starting with `.` and non-`.sai` files ignored, symbolic links rejected (`SYMLINK_IN_SOURCE`), segments must match `[a-z][a-z0-9_]*` and be neither strict keywords, reserved words nor `lib` (`INVALID_MODULE_NAME`), and a directory without a sibling file is a namespace only. |
| N-A-3 | [spec/04](../spec/04-modules-and-names.md) §4.11 | A submodule is not a member of its parent module. |
| N-A-4 | [spec/04](../spec/04-modules-and-names.md) §4.11 | A `PKG_PATH` names a module if one exists, otherwise an interface declared in the parent module. |
| N-A-5 | [spec/04](../spec/04-modules-and-names.md) §4.11 | The binding name of a `use` is the alias, else the last segment, else the package name, with `-` mapped to `_`. |
| N-A-6 | [spec/04](../spec/04-modules-and-names.md) §4.11 | `use P.{…}` without an alias binds only the items. |
| N-A-7 | [spec/04](../spec/04-modules-and-names.md) §4.11 | A module's member namespace contains its declared items and its `pub use` bindings. |
| N-A-8 | [spec/04](../spec/04-modules-and-names.md) §4.11 | `DUPLICATE_BINDING` is the code for every duplicate name in one scope: module scope, block, parameter list, generic parameter list, pattern, interface (§4.4.3). |
| N-A-9 | [spec/04](../spec/04-modules-and-names.md) §4.11 | Primitive type names, `Int`, `UInt` and `Never` cannot be rebound, and the reserved names `Send` and `Share` ([11](../spec/11-async-and-concurrency.md) N-I-19) cannot be declared or used as import names or aliases (`RESERVED_TYPE_NAME`). |
| N-A-10 | [spec/04](../spec/04-modules-and-names.md) §4.11 | Unqualified lookup order is locals, generic parameters, module scope, prelude. |
| N-A-11 | [spec/04](../spec/04-modules-and-names.md) §4.11 | `SHADOWS_MODULE` also applies to bindings of interfaces (§4.7.3). |
| N-A-12 | [spec/04](../spec/04-modules-and-names.md) §4.11 | A method's generic parameter MUST NOT reuse the name of a generic parameter of the enclosing impl or trait (`SHADOWS_GENERIC_PARAM`) (§4.7.4). |
| N-A-13 | [spec/04](../spec/04-modules-and-names.md) §4.11 | Outside its package, a record or resource can be constructed (also with `..base`) only if all its fields are visible. |
| N-A-14 | [spec/04](../spec/04-modules-and-names.md) §4.11 | The public surface checked by `PRIVATE_TYPE_IN_PUBLIC_API` is the table of §4.6.7. |
| N-A-15 | [spec/04](../spec/04-modules-and-names.md) §4.11 | `export` creates a core Wasm export only in the root package of the build. |
| N-A-16 | [spec/04](../spec/04-modules-and-names.md) §4.11 | `distinct D = pub B` with `D` not `pub` is the error `PUB_REPR_ON_PRIVATE_DISTINCT` (§4.6.4). |
| N-A-17 | [spec/04](../spec/04-modules-and-names.md) §4.11 | Inside an interface, names resolve in its members, then the module scope, then the prelude. |
| N-A-18 | [spec/04](../spec/04-modules-and-names.md) §4.11 | Canonical paths have the form of §4.9.1, with `field[..]`, `impl[..]`, `generic[..]`, `test[..]` and `property[..]` components, and map to symbol IDs by §4.9.2. |
| N-A-19 | [spec/04](../spec/04-modules-and-names.md) §4.11 | A `use` whose target is the current module is the error `USE_OF_CURRENT_MODULE` (§4.3.2). |
| N-A-20 | [spec/04](../spec/04-modules-and-names.md) §4.11 | `use` item lists name only module-level items. |
| N-A-21 | [spec/05](../spec/05-types.md) §5.21 | In expression position, generic arguments are all or none. |
| N-A-22 | [spec/05](../spec/05-types.md) §5.21 | The variant orders `None, Some` and `Ok, Err` of `Option` and `Result` are normative (§5.4.5). |
| N-A-23 | [spec/05](../spec/05-types.md) §5.21 | Data-carrying enums have discriminants 0, 1, … in declaration order. |
| N-A-24 | [spec/05](../spec/05-types.md) §5.21 | A distinct type inherits nothing: no methods, impls, operators or autoderef. |
| N-A-25 | [spec/05](../spec/05-types.md) §5.21 | The base of a refined type cannot be refined. |
| N-A-26 | [spec/05](../spec/05-types.md) §5.21 | Every alias parameter must be used (`UNUSED_ALIAS_PARAM`). |
| N-A-27 | [spec/05](../spec/05-types.md) §5.21 | Resource fields cannot contain views or borrows (`RESOURCE_WITH_BORROW`), and resources take no `@derive` (§5.6.6). |
| N-A-28 | [spec/05](../spec/05-types.md) §5.21 | No implied bounds from types in signatures. |
| N-A-29 | [spec/05](../spec/05-types.md) §5.21 | Projections `υ.A` are allowed only when `υ` is `Self`, a type parameter or a projection (§5.8.5). |
| N-A-30 | [spec/05](../spec/05-types.md) §5.21 | Monomorphization fails with `INSTANTIATION_DEPTH_EXCEEDED` when a type argument nests deeper than 64 (§5.7.6). |
| N-A-31 | [spec/05](../spec/05-types.md) §5.21 | A closure is `Copy`/`Clone` iff its captures are; a `borrow mut` capture makes it neither. |
| N-A-32 | [spec/05](../spec/05-types.md) §5.21 | A trait impl item must match the trait's signature exactly, up to renaming of its own generic parameters (§5.9.1). |
| N-A-33 | [spec/05](../spec/05-types.md) §5.21 | Inherent impls are allowed only for nominal types of the current package, except that `sai:core` and `sai:alloc`, as one package, may also declare them on the primitive types, `Array<T, N>` and their lang-item types ([16](../spec/16-core-library.md) N-J-6). |
| N-A-34 | [spec/05](../spec/05-types.md) §5.21 | A `Drop` impl must be exactly as generic as its type, for a local nominal type that is not refined (§5.9.3). |
| N-A-35 | [spec/05](../spec/05-types.md) §5.21 | Coherence: `borrow`, `borrow mut` and `Box` are fundamental; the owner table of §5.10.1; overlap is syntactic unification of headers, ignoring bounds; `core`+`alloc` are one package (§5.10). |
| N-A-36 | [spec/05](../spec/05-types.md) §5.21 | Trait solving: environment candidates before impl candidates; impls are global; inductive cycles fail; depth limit 128; ambiguity is deferred, then `TYPE_ANNOTATIONS_NEEDED` (§5.11). |
| N-A-37 | [spec/05](../spec/05-types.md) §5.21 | Method resolution: autoderef steps through `borrow`, `borrow mut`, `Box`, then a refined base step. |
| N-A-38 | [spec/05](../spec/05-types.md) §5.21 | `@derive` semantics: generated bounds `Ti: Tr`, or `B: Tr` for distinct types; supertraits are never derived implicitly; field order and discriminant order; forwarding for distinct types; refined types derive only `Arbitrary` (§5.12). |
| N-A-39 | [spec/05](../spec/05-types.md) §5.21 | `Copy` comes only from `@derive(Copy)` or compiler-provided impls. |
| N-A-40 | [spec/05](../spec/05-types.md) §5.21 | Coercions apply only when both heads are known, after a failed unification, at most one per site (other than regions), and never below a type constructor (§5.15.3). |
| N-A-41 | [spec/05](../spec/05-types.md) §5.21 | Integer and float literal variables default immediately at the points of §5.16.4, and otherwise at the end of the body. |
| N-A-42 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | The must-use types are exactly: instances of records, enums, distinct types and resources whose declaration carries `@must_use`; refined types that carry `@must_use` or whose base is a must-use type; and anonymous future types. |
| N-A-43 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | The discriminant type of an enum is the integer type named by `@repr(I)`, `I32` for `@repr(C)`, and `I64` otherwise. |
| N-A-44 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | `@target` is evaluated once per command, for the command's one target and the artifact kind that `sai build` would produce for it, independently of the profile. |
| N-A-45 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | The levels are `allow`, `info`, `warn` and `deny`. |
| N-A-46 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | Level attributes with different levels for one code on one target are `CONFLICTING_LINT_LEVEL`; the later one is dropped. |
| N-A-47 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | There are no lint groups. |
| N-A-48 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | The level of a lint occurrence is taken from the first source that applies, in the order `policy`, attribute (innermost first), CLI flag, `lints` block, default. |
| N-A-49 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | A warning or info about source is a lint unless it reports something the user cannot change in their source, a fact about the toolchain, manifest, lock, graph, build or compiler-private choices, or a construct whose meaning the compiler changed. |
| N-A-50 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | The attributes of one target are validated in the order of §15.2.4. |
| N-A-51 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | Exactly two pairs of attributes exclude each other on one target: `@inline` (without `never`) with `@cold`, and `@flags` with `@derive`. |
| N-A-52 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | Deprecation is not inherited by members. |
| N-A-53 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | The `instead` string is a dotted path. |
| N-A-54 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | The lint for uses of deprecated declarations is `DEPRECATED_REFERENCE`. |
| N-A-55 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | `DUPLICATE_EXPORT_NAME` is owned by this chapter. |
| N-A-56 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | `@inline` and `@cold` are hints. |
| N-A-57 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | `@repr(C)` is allowed on records and enums; `@repr(I)` with `I` one of `I8`–`I64`, `U8`–`U64` on enums, including enums with payloads. |
| N-A-58 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | A target predicate is a target name as a `STRING`, a capability as an identifier, or `all`, `any`, `none` of predicates. |
| N-A-59 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | `@doc(hidden)` affects only generated documentation and completion in other packages. |
| N-A-60 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | A level attribute is `UNUSED_ATTRIBUTE` only in the cases of §15.16.8. |
| N-A-61 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | `UNUSED_ITEM` counts a reference anywhere in the package outside the item's own syntax, including from tests and other unused items; it does not use reachability (§15.18.3). |
| N-A-62 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | Every toolchain classifies level names by the complete v0.1 registry. |
| N-A-63 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | The names in level attributes are checked in every package of the build, including dependencies (§15.15.4), even though the levels of dependency lints are capped (§15.16.7). |
| N-A-64 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | `NAMING_CONVENTION` checks declarations only, after removing `r#` and leading `_`. |
| N-A-65 | [spec/15](../spec/15-attributes-and-lints.md) §15.20 | `UNNECESSARY_VAR` is reported exactly when removing `var` would produce no `ASSIGN_TO_IMMUTABLE` or `BORROW_MUT_OF_IMMUTABLE` (§15.18.2). |
| N-A-66 | [spec/05](../spec/05-types.md) §5.21 | Derived impls have inferred effects. |
| N-A-67 | [spec/04](../spec/04-modules-and-names.md) §4.11 | Segments after a constant are field accesses and method calls on its value, as after a local binding. |

## N-B

Defined in spec/07, impl/05.

| ID | Defined in | Summary |
|---|---|---|
| N-B-1 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | There is no static promotion. |
| N-B-2 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | A non-Copy value cannot be moved out of `*b` for `b : Box<T>`. |
| N-B-3 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | Assigning to a strict sub-place of a place that is not definitely initialized is `PARTIAL_ASSIGN_TO_MOVED`. |
| N-B-4 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | A non-Copy value cannot be moved out of `a[i]` with the built-in index (`MOVE_OUT_OF_INDEX`). |
| N-B-5 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | An implicit reborrow happens only if the expected type at a coercion site is `borrow mut U` (or `SliceMut<U>`, or a shared view for the shared coercion) **before** generic inference. |
| N-B-6 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | Temporary lifetime extension applies when the whole initializer of a `let` or `var` statement is syntactically `borrow e` or `borrow mut e` with `e` a value expression, with any pattern and an optional annotation. |
| N-B-7 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | Regions inside function pointer types and `Fn`-family bounds are bound by that type (higher-ranked) and elided locally. |
| N-B-8 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | The variance of a user type's region and type parameters is computed from its fields. |
| N-B-9 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | Region subtyping applies at every value flow, not only at the coercion sites of D-TYPE-04. |
| N-B-10 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | All region positions of a return type share one output region `'o`. |
| N-B-11 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | If a function has no input region, its output region is `'static`. |
| N-B-12 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | In a trait impl, region positions in associated type definitions and trait arguments denote the target type's single region, or `'static` if it has none. |
| N-B-13 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | After elision and substitution, an impl method's signature must be identical to the trait method's, including regions (`IMPL_SIGNATURE_REGION_MISMATCH`). |
| N-B-14 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | A resource cannot carry a region (`RESOURCE_CONTAINS_BORROW`). |
| N-B-15 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | A drop is a write-like access that conflicts with shared as well as mutable loans. |
| N-B-16 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | The base of an `IndexMut` place is an auto-referenced receiver, so it is a two-phase borrow. |
| N-B-17 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | A closure takes its regions from an expected `Fn`-family bound. |
| N-B-18 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | The capture unit is the whole variable, also for a `borrow mut` variable that the body uses only through `*x`. |
| N-B-19 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | A call `f(..)` of a closure value borrows `f` (for `Fn`), borrows it mutably (for `FnMut`, which requires `var f`), or moves it (for `FnOnce` only). |
| N-B-20 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | The bindings of one pattern are dropped in reverse source order. |
| N-B-21 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | Tuple and `Array` elements are dropped in ascending index order. |
| N-B-22 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | `Box` and `Vec` are drop-transparent, and 16 may designate further core/alloc types. |
| N-B-23 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | For partial moves, a resource is treated as a type that implements `Drop`. |
| N-B-24 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | Outlives constraints are location-insensitive (`'a : 'b` means `'a ⊇ 'b` over the whole body). |
| N-B-25 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | The future of an `async fn` carries the regions of all its parameters. |
| N-B-26 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | A region bound by a closure's expected signature, and the region of an `Fn` / `FnMut` closure's environment borrow, are universal regions of the closure body. |
| N-B-27 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | Every `.await` has a cancellation exit in the CFG of the borrow rules. |
| N-B-28 | [spec/07](../spec/07-ownership-and-borrowing.md) §7.20 | A `Subslice` of an owned `Array` (a by-value `..rest` binding of a slice pattern) is a partial move of the `ConstIndex` move paths of the positions it covers. |
| N-B-29 | [impl/05](../impl/05-borrow-checker.md) §5.21 | The checker is two queries per root owner. |
| N-B-30 | [impl/05](../impl/05-borrow-checker.md) §5.21 | Pass B runs whenever pass A ran, even if pass A reported errors. |
| N-B-31 | [impl/05](../impl/05-borrow-checker.md) §5.21 | One region problem per root body. |
| N-B-32 | [impl/05](../impl/05-borrow-checker.md) §5.21 | A move path stops at every `Deref`, including the `Deref` of a `Box`. |
| N-B-33 | [impl/05](../impl/05-borrow-checker.md) §5.21 | `Return` and `CancelExit` are a shallow storage-dead of every local other than `_0` whose storage has not ended, including parameters. |
| N-B-34 | [impl/05](../impl/05-borrow-checker.md) §5.21 | The facts F1–F18 are requirements on impl/04. |
| N-B-35 | [impl/05](../impl/05-borrow-checker.md) §5.21 | The loan created by a borrow whose destination has origin `GuardScrutineeRef` is a guard loan. |
| N-B-36 | [impl/05](../impl/05-borrow-checker.md) §5.21 | In a take-and-restore call (F14), the argument `Move(p')` is a deep read of `p'` and the destination `p'` is a shallow write, instead of a move and a write. |
| N-B-37 | [impl/05](../impl/05-borrow-checker.md) §5.21 | Deduplication. |
| N-B-38 | [impl/05](../impl/05-borrow-checker.md) §5.21 | The cause search is a breadth-first search over the constraint graph in emission order. |
| N-B-39 | [impl/05](../impl/05-borrow-checker.md) §5.21 | The `borrow_conflict` and `use_after_move` entries of impl/11 §11.8.3 are filled by the table of §5.16.2. |
| N-B-40 | [impl/05](../impl/05-borrow-checker.md) §5.21 | A fix is machine-applicable only if the condition of its row in §5.16.5 holds and it is the only fix of the diagnostic. |
| N-B-41 | [impl/05](../impl/05-borrow-checker.md) §5.21 | `ENSURES_USES_MOVED_PARAMETER` is found by pass A, as a use after move of a parameter local inside an `Ensures` scope (F18), and reported instead of `USE_AFTER_MOVE`, once per scope and parameter. |
| N-B-42 | [impl/05](../impl/05-borrow-checker.md) §5.21 | A closure or `async` block type is invariant in every region it carries. |
| N-B-43 | [impl/05](../impl/05-borrow-checker.md) §5.21 | The fact dump (`borrowck_facts`) and the verify mode (Cargo feature `borrowck-verify`) are test-only. |
| N-B-44 | [impl/05](../impl/05-borrow-checker.md) §5.21 | A call of `sai:async.scope` creates a local scope region `'s_k`. |
| N-B-45 | [impl/05](../impl/05-borrow-checker.md) §5.21 | `SCOPE_REGION_ESCAPE` is decided by reachability from `'s_k` to a region position of the scope call's `F.Output` in the constraint graph, once per scope call, after the checks of §5.8. |

## N-C

Defined in spec/06, spec/09, spec/10.

| ID | Defined in | Summary |
|---|---|---|
| N-C-1 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | An integer literal never takes a floating-point type. |
| N-C-2 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Binary operators type the left operand by synthesis. |
| N-C-3 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | An implicit member `.V` uses the expected type exactly, with no see-through of `borrow`, `Box`, distinct or refined types. |
| N-C-4 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Comparison operands have the same type. |
| N-C-5 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Comparison operators take their operands by implicit shared borrow and never move them (§6.8.7, §6.8.10). |
| N-C-6 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Arithmetic, bitwise and shift operators take their operands by value; non-`Copy` operands are moved (§6.8.2). |
| N-C-7 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `Bool` has no bitwise operators (`&`, `\ |
| N-C-8 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | For signed integers, `MIN % -1` evaluates to `0` and does not panic. |
| N-C-9 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `Byte` supports shifts `<<` and `>>` (logical) in addition to `& \ |
| N-C-10 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `r.f(x)` is always a method call. |
| N-C-11 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | A tuple-variant path or a distinct-type path used as a value denotes a function item. |
| N-C-12 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Indexing never auto-derefs its base. |
| N-C-13 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Compound assignment on a non-`Copy` place is take-and-restore. |
| N-C-14 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | A block with no tail, one of whose statements diverges, has type `Never` (§6.11.1). |
| N-C-15 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `break` with a value is allowed only in `loop`. |
| N-C-16 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Jump targets resolve against a loop stack that is reset at every function, closure and `async` boundary. |
| N-C-17 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Closures capture whole variables, never field paths. |
| N-C-18 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `Array.filled(x)` requires `T: Copy`. |
| N-C-19 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | The range types are `Range`, `RangeInclusive` (D-EXPR-09), `RangeFrom`, `RangeTo`, `RangeToInclusive` and `RangeFull`. |
| N-C-20 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | The message of `assert` has type `Str` or `String`, and is evaluated only on failure. |
| N-C-21 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | f-strings desugar into `Writer.write_str` and `Format.format` calls on a fresh `String`. |
| N-C-22 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Exhaustiveness and reachability use the algorithm of §6.22: Maranget usefulness with integer and slice splitting. |
| N-C-23 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | In a match guard, bindings are shared views of the scrutinee. |
| N-C-24 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `x @ P` restrictions on combining binding modes (§6.21.4). |
| N-C-25 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Temporary scopes (§6.24.2): conditions and guards drop their temporaries after producing the `Bool`; scrutinees keep them to the end of the construct; block tails drop them before the block's locals; temporaries are dropped in reverse creation order. |
| N-C-26 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Temporaries of a `for` iterable live until the end of the `for` (§6.16.3). |
| N-C-27 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `_` never moves. |
| N-C-28 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `UNREACHABLE_CODE` is a warning. |
| N-C-29 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `as` after an expression is `NO_AS_CAST`, with conversion suggestions (§6.10). |
| N-C-30 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | "Without side effects" (D-EXPR-01) is defined syntactically (§6.25.2), and the warning is `STATEMENT_HAS_NO_EFFECT`. |
| N-C-31 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | `+% -% *%` are built-in on primitive integers only, and have no traits (§6.8.4). |
| N-C-32 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | An `if` without `else` used where a non-`()` value is required is `IF_WITHOUT_ELSE` (§6.14.1). |
| N-C-33 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Literal-pattern typing: string literals only on `Str`, byte strings only on `Slice<Byte>`. |
| N-C-34 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Fields reached through a refined type, or `.value` of a refined distinct type, are immutable places (§6.6.1). |
| N-C-35 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | A refined scrutinee: a top-level binding or `_` matches at the refined type; all other patterns and exhaustiveness use the base (§6.21.2). |
| N-C-36 | [spec/06](../spec/06-expressions-and-statements.md) §6.28 | Slice patterns apply only to `Array`, `Slice` and `SliceMut`, and have the rest-binding types of §6.21.3. |
| N-C-37 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Every contract clause, refinement clause and invariant element has a contract id made of the declaring item's canonical path, the clause kind and its index, used in `AssertKind`, diagnostics and messages (§9.1.3). |
| N-C-38 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | In a refinement predicate, `self` has the base type `B`, for both `type R = B where p` and `distinct D = B where p` (§9.2.1). |
| N-C-39 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Refinement erasure: any use of an `R` that needs a `B` sees the value as `B`. |
| N-C-40 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | `borrow R` coerces to `borrow B` at coercion sites. |
| N-C-41 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | `R.try_from(value: B) -> Option<R>` (and `D.try_from` for a refined distinct type) is compiler-provided, pure, and always a runtime test of `p` (§9.2.3). |
| N-C-42 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Every term of a refined static type contributes its instantiated predicate as a fact, wherever it is relevant (§9.2.4, §9.6.3). |
| N-C-43 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Predicates are resolved, type-checked and privacy-checked in the scope of their declaration, independent of where their checks are placed (§9.1.2). |
| N-C-44 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | `ensures` on `extern` functions are trusted, never checked, and used as facts. |
| N-C-45 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | A function with an `ensures` clause MUST NOT have a parameter named `result` (`RESULT_NAME_CONFLICT`) (§9.3.2). |
| N-C-46 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | `old(e)`: one operand naming only parameters, no nesting, no `result`, the type MUST be `Copy`, and it is evaluated at entry only if some clause using it needs a runtime check (§9.3.2). |
| N-C-47 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | A function with `requires` that is used as a function value goes through a checked entry that checks all of its `requires` at entry. |
| N-C-48 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | An `ensures` clause that mentions a by-value parameter that may be moved at a return point is `ENSURES_USES_MOVED_PARAMETER` (§9.3.4). |
| N-C-49 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Invariants are allowed only on non-generic record, enum, resource and distinct types (`INVARIANT_ON_GENERIC_TYPE`, `INVARIANT_ON_INVALID_TYPE`), at most one per type (`DUPLICATE_INVARIANT`) (§9.4.1). |
| N-C-50 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Construction of a type with an invariant is not a field write. |
| N-C-51 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Return check points cover exactly parameters of type `T` and `borrow mut T`, and results of type `T`. |
| N-C-52 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Invariant facts are generated for every clean place of an invariant type and for entry versions of parameters (§9.4.4, §9.6.3). |
| N-C-53 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Dirty places. |
| N-C-54 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Trait method contracts bind every impl. |
| N-C-55 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | The SFC runs once per generic body before monomorphization. |
| N-C-56 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Closure bodies and `async` blocks inherit no facts from the enclosing body. |
| N-C-57 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | SFC paths are roots with field, tuple and dereference projections (`.value` is transparent). |
| N-C-58 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Unsafe code MUST NOT change a path's value between its kill events except through a `borrow mut` created at a kill event (the fields of an `Arena` excepted, [12](../spec/12-unsafe-and-ffi.md) §12.6.4 rule 5). |
| N-C-59 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Range facts for `USize` and `ISize` use the 64-bit ranges on every target, and `Char` uses `[0, 0x10FFFF]` (§9.6.3). |
| N-C-60 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | A pure, statically resolved call with no `borrow mut`/`SliceMut` parameters, outside `unsafe fn` and `extern`, whose arguments have terms or keys, is a call atom identified by callee, generic arguments and argument keys (§9.6.4). |
| N-C-61 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Normal form: NNF, integer tightening of strict and negated constraints, gcd normalization with ceiling of the constant, canonical sorting, and no other simplification. |
| N-C-62 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Facts are computed by a single forward pass with intersection at joins. |
| N-C-63 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | The canonical order of atoms, versions, formulas and facts is defined over source spans, canonical paths and structural keys (§9.6.7). |
| N-C-64 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | The limits count distinct atomic formulas (256) and distinct integer variables (16) after relevance filtering, cubes after each multiplication (16), and constraints after each elimination step (256) (§9.6.8). |
| N-C-65 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Runtime contract failures panic with the messages `precondition failed: …`, `postcondition failed: …`, `invariant failed: <Type>: …` and `refinement failed: <Type>: …`, using canonical predicate text (§9.7.1). |
| N-C-66 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | External prover results are informational output without diagnostic codes. |
| N-C-67 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Trust-boundary validation covers component export arguments (depth first, then `requires`), core `export fn` arguments, and results of component imports and `extern "wasm"` functions. |
| N-C-68 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | For `async fn`, `requires` are checked when the future is created and `ensures` when the body produces its output. |
| N-C-69 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | There are exactly three kinds of failure: recoverable errors (values), panics (reported if possible, then a trap) and plain traps (never reported). |
| N-C-70 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | When `Γ.ret` is an inference variable (a closure or an `async` block), `?` unifies it with `Result<β, E>` or `Option<β>` from its operand (§10.3.2 item 2). |
| N-C-71 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | "Identical error types" in D-EXPR-05 means type equality after inference: aliases are transparent, distinct and refined types differ from their bases, and no coercion, including from `Never`, applies. |
| N-C-72 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | `e?` desugars to a `match` on the lang-item variants by identity. |
| N-C-73 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | The structured fixes of the `?` diagnostics are offered only in the deterministic cases of §10.3.5: wrapping the return type and its returns for `TRY_INVALID_RETURN_TYPE`, and `.map_err` to the unique single-payload variant for `TRY_ERROR_TYPE_MISMATCH`. |
| N-C-74 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | After a panic is raised, no SAI code runs in the instance: no drops, no `ensures` or invariant checks, no other task, and no program code during reporting. |
| N-C-75 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | The location of each panic is fixed by the table of §10.4.5. |
| N-C-76 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | The message texts of the built-in panic kinds are fixed by the table of §10.4.4. |
| N-C-77 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | A report is one UTF-8 text with the message and, if any, the location, without absolute paths or addresses. |
| N-C-78 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | The panic mode is `Report` exactly when the profile is `dev` or `release` and a channel exists: `wasi:cli/stderr` already imported, for a component, or a root-package `@panic_hook`, for a core module. |
| N-C-79 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | `@panic_hook` is allowed only on a function of an `extern "wasm"` block with exactly two `USize` parameters and result `()`, not `unsafe`, without contract clauses, in the root package, and at most once per build (§10.6.1). |
| N-C-80 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | The hook is imported only in `Report` mode, unless reachable SAI code calls it (§10.6.2). |
| N-C-81 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | The call of the hook by the panic machinery adds no effect to any function and no capability to any package. |
| N-C-82 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | A panic raised while a report is in progress traps without a report. |
| N-C-83 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | A host MUST NOT call a core module instance again after it has trapped. |
| N-C-84 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | The operands of a panic, including the message arguments of `panic`, `expect` and `expect_err`, are evaluated as written in every profile and panic mode. |
| N-C-85 | [spec/09](../spec/09-contracts-and-refinements.md) §9.10 | Borrowed places. |

## N-D

Defined in spec/08, spec/13, impl/07, impl/08.

| ID | Defined in | Summary |
|---|---|---|
| N-D-1 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Versionless interface-effect identity. |
| N-D-2 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Canonical effect-set order and duplicates. |
| N-D-3 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | No private effects in public signatures. |
| N-D-4 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Drops contribute no effects. |
| N-D-5 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | World-import effects. |
| N-D-6 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | No effects clause on interface functions. |
| N-D-7 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Trait impl methods are exempt from clause requirements. |
| N-D-8 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Constants are pure by construction. |
| N-D-9 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Unused declared effects. |
| N-D-10 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | `main` is an ordinary `pub` function for clause requirements. |
| N-D-11 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Major-version rule. |
| N-D-12 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Canonical-ABI allocation is an effect. |
| N-D-13 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Capabilities follow specified reachability and signature sets. |
| N-D-14 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Interfaces used only for types are required. |
| N-D-15 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Built-in roots are reserved in effect position. |
| N-D-16 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Acyclic covers. |
| N-D-17 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Deny by overlap. |
| N-D-18 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Scope of `AMBIENT_IMPORT`. |
| N-D-19 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Function pointer type identity. |
| N-D-20 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Caps through type parameters. |
| N-D-21 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Resolution-time footprint. |
| N-D-22 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Drop impls have no latent effects. |
| N-D-23 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Method-level type arguments are attributed at the call. |
| N-D-24 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Coercions to function pointers cover latent effects. |
| N-D-25 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Latent expansion follows the elaborated predicates. |
| N-D-26 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Latent expansion is bounded. |
| N-D-27 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | Impl methods cannot strengthen bound caps. |
| N-D-28 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Resource members are inherent functions of the bound type. |
| N-D-29 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Resolution of interface calls. |
| N-D-30 | [spec/13](../spec/13-components-and-wit.md) §13.14 | No interface both imported and exported. |
| N-D-31 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Commands and reactors. |
| N-D-32 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Worlds on core targets. |
| N-D-33 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Versionless WIT references in source. |
| N-D-34 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Unstable WIT items are invisible. |
| N-D-35 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Structural derives for the SAI view. |
| N-D-36 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Name conversion algorithm. |
| N-D-37 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Borrowed parameter views. |
| N-D-38 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Flags types. |
| N-D-39 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Exact generated WIT format. |
| N-D-40 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Contracts are not part of interfaces. |
| N-D-41 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Imports are pruned to what is required. |
| N-D-42 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Methods borrow. |
| N-D-43 | [spec/13](../spec/13-components-and-wit.md) §13.14 | External export worlds. |
| N-D-44 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Single-type variant payloads. |
| N-D-45 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Interface record fields are public. |
| N-D-46 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Core-level items in component builds. |
| N-D-47 | [impl/07](../impl/07-component-lowering.md) §7.14 | Zero fill of joined positions. |
| N-D-48 | [impl/07](../impl/07-component-lowering.md) §7.14 | Values lowered by the runtime are trusted. |
| N-D-49 | [impl/07](../impl/07-component-lowering.md) §7.14 | `cabi_realloc` is exported only when needed. |
| N-D-50 | [impl/07](../impl/07-component-lowering.md) §7.14 | Component runtime state. |
| N-D-51 | [impl/07](../impl/07-component-lowering.md) §7.14 | Post-return functions. |
| N-D-52 | [impl/07](../impl/07-component-lowering.md) §7.14 | Internal layout and Canonical ABI are kept separate. |
| N-D-53 | [impl/07](../impl/07-component-lowering.md) §7.14 | Exported resource reps are plain boxes. |
| N-D-54 | [impl/07](../impl/07-component-lowering.md) §7.14 | The release walk runs no `Drop` impl. |
| N-D-55 | [impl/07](../impl/07-component-lowering.md) §7.14 | Core names follow the `wit-component` conventions. |
| N-D-56 | [impl/07](../impl/07-component-lowering.md) §7.14 | The `component-type:sai` custom section. |
| N-D-57 | [impl/07](../impl/07-component-lowering.md) §7.14 | The synthetic world. |
| N-D-58 | [impl/07](../impl/07-component-lowering.md) §7.14 | Components for `wasm64-wasi` are not implemented. |
| N-D-59 | [impl/07](../impl/07-component-lowering.md) §7.14 | Panic reports never add imports. |
| N-D-60 | [impl/07](../impl/07-component-lowering.md) §7.14 | Sorted core imports and exports. |
| N-D-61 | [impl/07](../impl/07-component-lowering.md) §7.14 | No adapter, start function or `_initialize`. |
| N-D-62 | [impl/07](../impl/07-component-lowering.md) §7.14 | Validation failures are compiler bugs. |
| N-D-63 | [impl/07](../impl/07-component-lowering.md) §7.14 | Normalized empty buffers. |
| N-D-64 | [impl/07](../impl/07-component-lowering.md) §7.14 | Glue is synthetic MIR. |
| N-D-65 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | `Call.effects` is informative. |
| N-D-66 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Internal order and print order. |
| N-D-67 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Unspellable atoms get a help, not a fix. |
| N-D-68 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Capability analysis is gated. |
| N-D-69 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Shared latent keys. |
| N-D-70 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | One diagnostic per first contributing operation. |
| N-D-71 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Unused declarations are order independent. |
| N-D-72 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Predicate operations come from HIR. |
| N-D-73 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Operation nodes. |
| N-D-74 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | `Empty` environment keys. |
| N-D-75 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Overflow is reported through `Λ`-only paths. |
| N-D-76 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Foreign metadata contents. |
| N-D-77 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Only instantiating operations are checked for bound caps. |
| N-D-78 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Only the first failing cap is reported. |
| N-D-79 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Authority fixes are `maybe-incorrect` and local. |
| N-D-80 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Total link order, no `Covers` links, no WASI WIT locations. |
| N-D-81 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Pseudo-roots. |
| N-D-82 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Run glue and the result drop of `main`. |
| N-D-83 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | The root is checked only against its effective policy. |
| N-D-84 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Profile-case fix target. |
| N-D-85 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | The footprint is an upper bound in dependency position. |
| N-D-86 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Ownership of the unsafe check. |
| N-D-87 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | The impl-method cap check follows `compare_impl_fn`. |
| N-D-88 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Test suppression covers policy, grant and target checks. |
| N-D-89 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | `TestHarness` checks are implied. |
| N-D-90 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Tests are solved separately. |
| N-D-91 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | The body of an `async fn`. |
| N-D-92 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | The wrap target of `UNSAFE_OPERATION_OUTSIDE_UNSAFE`. |
| N-D-93 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | When an `unsafe module` header is used. |
| N-D-94 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Syntactic paths for removed items. |
| N-D-95 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Fixes that depend on name resolution. |
| N-D-96 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | `sai inspect effects`. |
| N-D-97 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Unsafe entries in test code. |
| N-D-98 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | An `unsafe` block in a predicate. |
| N-D-99 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Recognizing a `# Safety` section. |
| N-D-100 | [spec/08](../spec/08-effects-and-capabilities.md) §8.16 | WASI built-in effects permit `wasi:io`. |
| N-D-101 | [spec/13](../spec/13-components-and-wit.md) §13.14 | The imports of a composed component. |
| N-D-102 | [impl/07](../impl/07-component-lowering.md) §7.14 | Providers of composed imports. |
| N-D-103 | [impl/07](../impl/07-component-lowering.md) §7.14 | Structure of a composed component. |
| N-D-104 | [spec/13](../spec/13-components-and-wit.md) §13.14 | In-build provision by hybrid dependencies in source form. |
| N-D-105 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Provided interfaces stay inside the component. |
| N-D-106 | [spec/13](../spec/13-components-and-wit.md) §13.14 | Provided interfaces are capabilities, not imports. |
| N-D-107 | [impl/07](../impl/07-component-lowering.md) §7.14 | Provision glue. |
| N-D-108 | [impl/07](../impl/07-component-lowering.md) §7.14 | In-module handles of provided resources. |
| N-D-109 | [impl/07](../impl/07-component-lowering.md) §7.14 | Private wasm64 layout of provision glue. |
| N-D-110 | [impl/08](../impl/08-effect-and-capability-analysis.md) §8.13 | Policy names are checked per package, at resolution. |

## N-E

Defined in impl/02, impl/03, impl/04.

| ID | Defined in | Summary |
|---|---|---|
| N-E-1 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | The reference implementation's limits are the minimums of spec/01 §1.5.4. |
| N-E-2 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | Tokens and CST nodes share one `SyntaxKind` enumeration. |
| N-E-3 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | For the forbidden-character rule, "string literals" includes byte strings, format-string text, character literals, and byte literals; bidirectional controls are forbidden everywhere (§2.2.3). |
| N-E-4 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | The lexer runs to completion over the whole file before parsing; the only lexer context is the previous token (for `PKG_PATH` and tuple indices) and the bracket stack (§2.4). |
| N-E-5 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | Unterminated simple strings, characters, bytes, and format strings end before the offending line break; multiline strings, raw strings, and block comments extend to EOF. |
| N-E-6 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | A format string is lexed as the token sequence `FSTRING_START`, `FSTRING_TEXT`, `FSTRING_INTERP_OPEN`, …, `FSTRING_SPEC`, `FSTRING_INTERP_CLOSE`, `FSTRING_END`, so interpolations are ordinary CST expressions (§2.4.7). |
| N-E-7 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | Doc-comment tokens take part in NEWLINE generation as tokens that are in neither E nor C, even when the parser later demotes them (§2.4.8). |
| N-E-8 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | A `;` (reported as `NO_SEMICOLON`) acts as a line break for NEWLINE generation (§2.4.9). |
| N-E-9 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | `ERROR` tokens belong to E. |
| N-E-10 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | A NEWLINE token is zero-width and placed directly after the trailing trivia of the token that ends the line (§2.4.11, §2.6.3). |
| N-E-11 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | The parser counts entries into `expr`, `pattern`, `type_`, and `block`; beyond 512 it reports `IMPLEMENTATION_LIMIT_EXCEEDED` once per file and skips the innermost construct as an `ERROR` node (§2.5.3). |
| N-E-12 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | Function clauses are parsed in any order; misordering is `CLAUSE_ORDER` with a reordering fix, and a repeated `where`, `borrows`, or `effects` clause is `DUPLICATE_CLAUSE` (§2.5.4). |
| N-E-13 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | `a..b..c` and `..a..b` are `CHAINED_RANGE`; the tree is still built (§2.5.6). |
| N-E-14 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | Under the `NO_BLOCK_OPERAND` restriction (conditions, scrutinees, `for` iterables), an optional operand (range end, `return`/`break` value) is not parsed when the next token is `{` (§2.5.6). |
| N-E-15 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | A line that starts with a binary operator other than `-`, `*`, and `..` after a complete statement is reported as `UNEXPECTED_NEWLINE` with a fix that moves the operator, and the operator and operand are wrapped in one `ERROR` node (§2.5.11). |
| N-E-16 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | Trivia is stored on tokens as leading and trailing lists, with the attachment rule of §2.6.3. |
| N-E-17 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | v0.1 reparses the whole file after every edit; the parse is cached per file revision (§2.6.7). |
| N-E-18 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | HIR is built before name resolution: paths stay unresolved in HIR, and resolution results live in side tables of impl/03 (§2.8.1). |
| N-E-19 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | Desugared code refers to core items through `PathRoot::Lang` and binds synthetic names that user code cannot spell (hygiene, §2.8.1, §2.9.1). |
| N-E-20 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | Impls are disambiguated in `DefPath`s by the low 64 bits of the stable hash of their header tokens (§2.8.2). |
| N-E-21 | [impl/02](../impl/02-frontend-ast-hir.md) §2.11 | `NON_ASCII_PUBLIC_NAME` uses the visibility of spec/04 §4.6.2: items of a `pub` trait and fields of the variants of a `pub` enum are public names. |
| N-E-22 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | The unit of type checking is the body owner: a function or method with a body, a `const` initializer, a test, a property, a refinement predicate, an invariant. |
| N-E-23 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | Module-tree diagnostics, which concern files and directories, have the fixed primary locations and `data` of the table in §3.2.3. |
| N-E-24 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | Regions are placeholders in this chapter: `Static` for literal data, `Sig { item, index }` numbered in pre-order in signatures and declarations, `Erased` in bodies. |
| N-E-25 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | Every cycle that the language rejects (`REEXPORT_CYCLE`, `ALIAS_CYCLE`, `SUPERTRAIT_CYCLE`, `CONST_CYCLE`, `INFINITE_SIZE_TYPE`) is found by an explicit algorithm inside one query; a cycle between queries is an internal compiler error (§3.1.3). |
| N-E-26 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | The function-trait methods `call`, `call_mut` and `call_once`, and `StreamNext`, are compiler-internal: synthesized by `trait_def` or recognized by shape, never nameable in source, visible only in MIR dumps and symbols (§3.3.6). |
| N-E-27 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | A `Field` node belongs to a path chain only if its span starts where the span of the chain's first segment starts, so that `(m).S` is a field access while `m.S` is a path (§3.4.2). |
| N-E-28 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | The function traits each take one type parameter, the tuple of argument types; `FnOnce` has one internal associated type `Output`. |
| N-E-29 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | Code after a diverging expression is checked normally, warned once with `UNREACHABLE_CODE`, and recorded in `TypeckResults::unreachable`. |
| N-E-30 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | The end-of-body steps run in the fixed order of §3.7.6: selection, literal defaulting, selection, capture analysis and closure kinds, selection, literal range checks, final selection, report of unresolved variables. |
| N-E-31 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | In a predicate, `return`, `break` and `continue` are reported once, as `INVALID_CONTRACT_EXPRESSION`, during resolution, and no `RETURN_OUTSIDE_FUNCTION` or label error is added (§3.4.5). |
| N-E-32 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | An overloaded index is first resolved as `Index`; a fix-up after the enclosing expression switches it to `IndexMut` when the indexed place ends up in a mutable place context, including a `borrow mut` auto-referenced receiver (§3.7.12). |
| N-E-33 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | Constants and explicit discriminants are evaluated on typed HIR by `const_value`; no MIR is built for them. |
| N-E-34 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | A closure type is `Copy` or `Clone` through the goal `υ: Tr` on its capture tuple, which stalls until capture analysis; anonymous future types are never `Copy` or `Clone` (§3.8.5). |
| N-E-35 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | Solver caching: only goals without inference variables are canonicalized and cached; a result that depends on an inductive cycle still on the stack is cached only at the root of that cycle; overflow is never cached (§3.8.4). |
| N-E-36 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | An expected function pointer type supplies the expected signature of a closure. |
| N-E-37 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | Bounds on associated types are not added to the elaborated `ParamEnv`; the solver uses them as environment candidates on demand for rigid projections (§3.14.1). |
| N-E-38 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | `GUARD_MOVE_OR_MUTATION` is decided in `check_match` by the use classifier on typed HIR, for places rooted at the arm's bindings or the scrutinee; [impl/04](../impl/04-mir.md) holds a shallow shared borrow of the scrutinee across every guard, so mutation through other aliases is a borrow error (§3.12.2). |
| N-E-39 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | The type-system constructs of Stages 2 to 4 are gated by this chapter, once per construct, at the locations and with the feature names of §3.14.4. |
| N-E-40 | [impl/04](../impl/04-mir.md) §4.16 | MIR extends the skeleton of D-MIR-01: `ConstIndex.min_length` and `Subslice.from_end`; the target type of `Convert`; the constants `Alloc`, `Location` and `Instance`; the statements `ProofObligation` and `CheckPoint`, which are gone by `Analyzed`; the kind of `Drop`; the kind and the `cancel` edge of `Await` with the terminator `CancelExit`; the kind of `Trap`; the local origins `GuardBindingRef`, `DirtyFlag` and `CallerLocation`; obligations, coroutine data, the caller location and the unlowered HIR of a body; predicate scopes and scope locations (§4.2). |
| N-E-41 | [impl/04](../impl/04-mir.md) §4.16 | The bodies of an owner are numbered: its own body, then its closures and `async` blocks in HIR pre-order, then its contract clauses in the order of `contract_clauses`. |
| N-E-42 | [impl/04](../impl/04-mir.md) §4.16 | A function item with `requires` clauses that is used as a value, not as the callee of a direct call, resolves to `CheckedEntry(f)`, which checks the clauses at run time, located at the name of `f`. |
| N-E-43 | [impl/04](../impl/04-mir.md) §4.16 | Monomorphization untuples a call through a function trait when the callee instance is known (§4.3.4). |
| N-E-44 | [impl/04](../impl/04-mir.md) §4.16 | A place operand is used directly only if every later operand of the same rvalue or call is a place or a constant, otherwise it is copied or moved into a temporary at its position. |
| N-E-45 | [impl/04](../impl/04-mir.md) §4.16 | A `match` is a decision list: arms in source order, each pattern tested by spec/06 §6.21.7, each failed test jumping to the next alternative or arm. |
| N-E-46 | [impl/04](../impl/04-mir.md) §4.16 | The SFC runs on typed HIR in evaluation order. |
| N-E-47 | [impl/04](../impl/04-mir.md) §4.16 | The invariant checks of return points and dirty places are materialized by drop elaboration from the initialization classes of pass A and a forward may-analysis of dirtiness. |
| N-E-48 | [impl/04](../impl/04-mir.md) §4.16 | A boundary function is entered from outside only through `BoundaryEntry(f)`, created only when a parameter needs validation or a clause is not proven at the entry. |
| N-E-49 | [impl/04](../impl/04-mir.md) §4.16 | The caller location of a transparent body is a hidden parameter `_l` of every forwarding instance, a least fixed point computed per instance. |
| N-E-50 | [impl/04](../impl/04-mir.md) §4.16 | The async transform runs on generic Analyzed MIR at the start of `mir_optimized`. |
| N-E-51 | [impl/04](../impl/04-mir.md) §4.16 | Instance collection with its diagnostics runs in phase 10 on Analyzed MIR (`mono_items`), separately from the collection of phase 11 for code generation (§4.1.1). |
| N-E-52 | [impl/04](../impl/04-mir.md) §4.16 | Monomorphization is deterministic, a FIFO worklist from ordered roots that scans in block, statement and operand order, and has no polymorphization: every distinct `GenericArgs` gets its own body (§4.13.1, §4.13.2). |
| N-E-53 | [impl/04](../impl/04-mir.md) §4.16 | `ASYNC_RECURSION` is decided on the monomorphic future types by Tarjan's algorithm over inline field containment. |
| N-E-54 | [impl/04](../impl/04-mir.md) §4.16 | MIR building emits `Assert` for the check kinds and a call of `core.panic.panic` for every panic with a message. |
| N-E-55 | [impl/04](../impl/04-mir.md) §4.16 | The text format of MIR is deterministic, printed from content, not stable across compiler versions and never parsed back. |
| N-E-56 | [impl/04](../impl/04-mir.md) §4.16 | `sai_mir` reaches the downstream crates through the `MirHooks` supertrait, which the root database implements by forwarding to their queries (§4.1.4). |
| N-E-57 | [impl/04](../impl/04-mir.md) §4.16 | `mir_analyzed` removes every `old(e)` snapshot whose clause has no remaining runtime check (§4.11.3). |
| N-E-58 | [impl/04](../impl/04-mir.md) §4.16 | Every query that produces bodies runs the validator of their phase. |
| N-E-59 | [impl/04](../impl/04-mir.md) §4.16 | There are two collections: a check collection over the program roots, in a fixed order, with the limits 64 for type depth and 1024 for nesting depth, and a code-generation collection over the artifact roots, whose instances MUST be in the check collection. |
| N-E-60 | [impl/04](../impl/04-mir.md) §4.16 | In Mono, every function constant is a resolved, zero-sized `Instance` constant, and `mir_mono` lowers the intrinsics kept until monomorphization to plain MIR or runtime calls. |
| N-E-61 | [impl/04](../impl/04-mir.md) §4.16 | A synthetic body calls a core import that has no SAI declaration through the instance `CoreImport { module, field, sig }`, whose signature consists of core scalar types. |
| N-E-62 | [impl/04](../impl/04-mir.md) §4.16 | A dirty place reached through a `Deref` of a `borrow mut` or `SliceMut` held by a non-parameter local or a temporary is not cleaned by the end of its root. |
| N-E-63 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | Boundary declarations are checked in `wf_check_item` in the order form, link name, types, panic hook signature, and the hook signature is checked only when no code of spec/12 §12.7.4 or `EXTERN_EFFECTS_MISMATCH` applies. |
| N-E-64 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | `e as T` is parsed into a recovery node without a diagnostic and reported as `NO_AS_CAST` by the type checker at the end of the body, with a fix chosen from the resolved operand and target types. |
| N-E-65 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | Bindings of test-only uses are marked in item collection from the `requires test` entries of the package's own contract, in every build. |
| N-E-66 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | `UNUSED_ATTRIBUTE` and `MISSING_DOCS` are computed per item owner by `attr_lints`, from attributes and signatures only, in every build after `@target` filtering; `MISSING_DOCS` is computed only for the root package and the workspace members (§3.6.10). |
| N-E-67 | [impl/04](../impl/04-mir.md) §4.16 | A selected property has two synthetic harness entries: a case entry that generates and runs case `i`, and a counterexample entry that generates case `i` again and formats it. |
| N-E-68 | [impl/03](../impl/03-name-resolution-and-type-checking.md) §3.17 | `entry_point(root)` checks the candidate `main` of the root module in every build and check, on every target and in test builds: `async` is `ASYNC_MAIN` only, a signature that contains the error type reports nothing more, and every other deviation is one `INVALID_ENTRY_POINT` at the name. |

## N-F

Defined in impl/06, impl/10, impl/09, targets, spec/10.

| ID | Defined in | Summary |
|---|---|---|
| N-F-1 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | The Wasm feature set depends only on the target and the opt-in features, never on the profile. |
| N-F-2 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | The shadow stack size `S` is 1 MiB on every target and in every profile, and is not configurable in v0.1 (§6.3.3). |
| N-F-3 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | `needs_memory` and `needs_stack` are decided after all bodies are lowered and select one of the three layouts of §6.3.4. |
| N-F-4 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | A module has at most one memory: minimum `__initial_pages`, no maximum, never imported, never shared, exported as `memory` whenever it exists, on every target (§6.3.7). |
| N-F-5 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | The `Char` niche is the single range `0x110000..=0xFFFFFFFF`. |
| N-F-6 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | The address of a zero-sized value is the constant `align(T)`. |
| N-F-7 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | Refinement predicates never narrow a valid range and never create niches (§6.4.8). |
| N-F-8 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | Function pointers and imported resource handles are 4-byte scalars with niche 0 on every target. |
| N-F-9 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | Negative explicit discriminants use the smallest signed tag that holds every discriminant. |
| N-F-10 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | Narrow scalars, `Bool`, `Char` and `Byte` are kept in canonical form in Wasm locals, at calls and in memory (§6.5.2). |
| N-F-11 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | The reference backend never passes a borrow by value, although D-WASM-04 permits it (§6.6.1). |
| N-F-12 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | A failed value check of an `export fn` parameter or an `extern "wasm"` result traps without a report, in every profile (§6.6.7). |
| N-F-13 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | Frame slots are never shared. |
| N-F-14 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | Panic mode is `Report` iff the profile is `dev` or `release` and a reporting channel exists; otherwise `TrapOnly`. |
| N-F-15 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | A value with at most 4 leaves is copied leaf by leaf. |
| N-F-16 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | Static data items are merged only when bytes, alignment and relocations are equal. |
| N-F-17 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | A variant whose payload is uninhabited and zero-sized is absent and has no encoding. |
| N-F-18 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | Control flow is structured by the algorithm of §6.9 ("Beyond Relooper" with fixed choices). |
| N-F-19 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | `br_table` is used iff the switch has at least 4 entries, `range ≤ 4 · n` and `range ≤ 65536`. |
| N-F-20 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | The whole program is one module. |
| N-F-21 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | Custom sections per profile are those of §6.11.1. |
| N-F-22 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | DWARF is emitted only in `dev`, and contains only the compile unit, subprograms and line tables (§6.12). |
| N-F-23 | [impl/10](../impl/10-runtime.md) §10.12 | Runtime functions are Mono MIR bodies built by the compiler, not SAI source. |
| N-F-24 | [impl/10](../impl/10-runtime.md) §10.12 | Runtime state is the zero-initialized items `__sai_alloc_heads`, `__sai_tiny_head`, `__sai_custom_state`, `__sai_panic_buf` and `__sai_panic_ret`, placed after static data in this order. |
| N-F-25 | [impl/10](../impl/10-runtime.md) §10.12 | Every allocator returns `align` for size 0, never returns 0, never returns after a failure, calls no host function, traps on an alignment that is not a power of two, and is a deterministic function of the call sequence (§10.2.2, §10.2.3). |
| N-F-26 | [impl/10](../impl/10-runtime.md) §10.12 | The allocator is the root's `allocator` package in every profile if declared, otherwise the tiny allocator in `tiny` and the default allocator in `dev` and `release` (§10.2.1). |
| N-F-27 | [impl/10](../impl/10-runtime.md) §10.12 | The default allocator has eight size classes from 16 to 2048 bytes, served from free lists of never-split blocks, and one address-sorted first-fit list of large blocks that is split from the end and coalesced. |
| N-F-28 | [impl/10](../impl/10-runtime.md) §10.12 | The tiny allocator is a bump allocator with one LIFO free list that is split from the end, and it returns a block at the top of the heap to the bump pointer (§10.2.5). |
| N-F-29 | [impl/10](../impl/10-runtime.md) §10.12 | `carve` is the allocators' only source of fresh memory. |
| N-F-30 | [impl/10](../impl/10-runtime.md) §10.12 | An allocator package declares `STATE_SIZE` and the four `unsafe fn`s `add_region`, `allocate`, `deallocate` and `reallocate` with fixed signatures. |
| N-F-31 | [impl/10](../impl/10-runtime.md) §10.12 | `cabi_realloc` traps on an alignment outside `{1, 2, 4, 8}` and otherwise is `__sai_realloc`. |
| N-F-32 | [impl/10](../impl/10-runtime.md) §10.12 | An arena is library code of `sai:alloc`. |
| N-F-33 | [impl/10](../impl/10-runtime.md) §10.12 | A panic site is a static record of 7 fields (28 bytes on wasm32, 48 on wasm64), with forms `Text`, `Bounds` and `Dynamic`, and a file string `<ns>:<name>@<version>/<path>` (§10.5.2). |
| N-F-34 | [impl/10](../impl/10-runtime.md) §10.12 | A report is `panic at <file>:<line>:<column>: <message>` or `panic: <message>`, followed by a line feed, formatted by the runtime without allocation (§10.5.5). |
| N-F-35 | [impl/10](../impl/10-runtime.md) §10.12 | A report of up to 1024 bytes uses `__sai_panic_buf`. |
| N-F-36 | [impl/10](../impl/10-runtime.md) §10.12 | The stderr channel calls `get-stderr` once and `blocking-write-and-flush` in pieces of at most 4096 bytes, stops at the first error, and drops no handle (§10.5.4). |
| N-F-37 | [impl/10](../impl/10-runtime.md) §10.12 | A module has no start function and no initialization export, and instantiation runs no code: all runtime state is valid when zero, and runtime globals have constant initializers (§10.7.1). |
| N-F-38 | [impl/10](../impl/10-runtime.md) §10.12 | The test suite checks the code-size bounds of §10.10 and the tests of §10.11 (§10.10, §10.11). |
| N-F-39 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | An optimization preserves the observable behavior of every execution with defined behavior: the sequence of import calls (with arguments and escaped memory), export returns (with results and escaped memory), panics (kind, message, location), traps that are not panics, and non-termination. |
| N-F-40 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | `opt_generic` runs the same cleanup and `simplify_cfg` in every profile. |
| N-F-41 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | No optimization pass reports a diagnostic. |
| N-F-42 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | The pass pipeline of each profile is fixed by the table of §9.3.1 and pipeline M (§9.3.2). |
| N-F-43 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Every MIR pass uses one alias model: exposed locals (those with a `Borrow` or `RawPtr` of a place based on them without `Deref`), tracked places (non-exposed bases with `Field` projections), and one abstract memory `M` that calls, drops and awaits may read and write. |
| N-F-44 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Every MIR pass keeps validity, reducibility, the order of `Assert`s relative to effects, the order of calls, drops, awaits and traps, every span and source scope, and every reachable cycle. |
| N-F-45 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | `const_prop` is sparse conditional constant propagation over tracked leaves and enum variant values, with exact folding only. |
| N-F-46 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Inlining uses the cost table and `should_inline` of §9.4.5 with the constants of §9.3.4. |
| N-F-47 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Inlining creates one new source scope per callee scope; the outermost carries `InlinedRecord { callee, call_span }`. |
| N-F-48 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | An `Assert` is removed only by a constant condition, by an interval analysis with `WIDEN_AFTER` widening and branch refinement, or by a fact of a dominating branch or check found by value numbering. |
| N-F-49 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | In `TrapOnly` mode, every non-constant message argument of a panic call becomes `Const("")`, and every non-constant location argument the location of the call's own span. |
| N-F-50 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | `dead_alloc` removes a `__sai_alloc` call with a constant power-of-two alignment whose result, and every copy of it, is used only as the base of stores and as the pointer of a matching `__sai_free` call, together with those calls and stores. |
| N-F-51 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Identical code folding runs in `tiny` only. |
| N-F-52 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Dead-code elimination runs in three stages in every profile: item-level DCE on Mono instances, the late stack-overflow root after frame assignment, and final DCE on Wasm bodies. |
| N-F-53 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | A retention chain is found by breadth-first search over the final module from the roots of group A, then group B, to the first node that is in the target set or references it. |
| N-F-54 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | The Wasm passes W1 to W9 are repeated until a round changes nothing or `WASM_ROUNDS` rounds have run, then W10 to W12 run once. |
| N-F-55 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Every result of this chapter is a function of the program, the target, the profile and the toolchain version. |
| N-F-56 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | The size tree has the six top-level nodes `SAI code`, `Dependencies`, `Runtime features`, `Unicode`, `WASI` and `Other`, always present and in this order. |
| N-F-57 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Each attribution entry is assigned to a group by its owner and symbol with the tables of §9.10.3. |
| N-F-58 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | An item is a Unicode item if it belongs to a standard package shipped with the toolchain other than `sai:core` and `sai:alloc`, the package's checksum equals that of the shipped copy, and its canonical path has a module segment `unicode`. |
| N-F-59 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | A component build is attributed by parsing the component with `wasmparser` and locating the embedded core module, whose non-custom sections MUST be byte-identical to those of the core module; a difference is `INTERNAL_COMPILER_ERROR`. |
| N-F-60 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Each `wasm <= L` item of the profile's `budget` block is checked separately against the length of the written artifact, taken from the bytes of the same session. |
| N-F-61 | [impl/09](../impl/09-optimization-and-size.md) §9.12 | Every pass has golden tests with the MIR validator running after each pass; Examples A, B and C are exact goldens; generated programs and an optimization corpus are run in the three profiles; and the size suite S1 to S12 MUST hold (§9.11). |
| N-F-62 | [targets](../targets/targets-and-profiles.md) §6 | Every target name of §2.1 and §2.8.1 is known to every toolchain. |
| N-F-63 | [targets](../targets/targets-and-profiles.md) §6 | The properties of a target (kind, address width, memory type, feature set, WASI version) do not depend on the profile, the package or the command. |
| N-F-64 | [targets](../targets/targets-and-profiles.md) §6 | The shadow stack size `S` is 0 or a multiple of 16 between 1 MiB and 64 MiB, the same in every profile. |
| N-F-65 | [targets](../targets/targets-and-profiles.md) §6 | For `wasm64-wasi`, analysis and the core module are required of a compiler that offers the target. |
| N-F-66 | [targets](../targets/targets-and-profiles.md) §6 | The native target table: four targets with their C data models. |
| N-F-67 | [targets](../targets/targets-and-profiles.md) §6 | Native builds: libraries are located like `-l<name>` of the platform linker (`NATIVE_LIBRARY_NOT_FOUND` otherwise). |
| N-F-68 | [targets](../targets/targets-and-profiles.md) §6 | The capability table of §3.1 is authoritative. |
| N-F-69 | [targets](../targets/targets-and-profiles.md) §6 | The `wasi:io` implication. |
| N-F-70 | [targets](../targets/targets-and-profiles.md) §6 | No v0.1 target provides the Component Model async ABI. |
| N-F-71 | [targets](../targets/targets-and-profiles.md) §6 | The entry-point candidate is a function item `main` at the top level of the root module of the root package. |
| N-F-72 | [targets](../targets/targets-and-profiles.md) §6 | On a core module, a valid `main` is neither an export nor a root of reachability. |
| N-F-73 | [targets](../targets/targets-and-profiles.md) §6 | The `Env` value of `main` gives access to every capability value. |
| N-F-74 | [targets](../targets/targets-and-profiles.md) §6 | A core module exports exactly the root's `export fn` items and `memory` when it exists, and imports exactly the reachable `extern "wasm"` functions and the `@panic_hook` when it is used. |
| N-F-75 | [targets](../targets/targets-and-profiles.md) §6 | A build has one profile, and every source package of the build is compiled with it (§5.1 item 3). |
| N-F-76 | [targets](../targets/targets-and-profiles.md) §6 | `denied_tiny(c)` holds exactly for capabilities in the WASI namespace (including `wasi:io`), `host` and `threads`. |
| N-F-77 | [targets](../targets/targets-and-profiles.md) §6 | The effective policy is `A_p = { a ∈ allow(root) \ |
| N-F-78 | [targets](../targets/targets-and-profiles.md) §6 | A capability that the profile alone denies is `CAPABILITY_NOT_ALLOWED` in the profile case of impl/08 §8.7.6; there is no separate code. |
| N-F-79 | [targets](../targets/targets-and-profiles.md) §6 | Dynamic linking and reflection are not capabilities and cannot be lifted. |
| N-F-80 | [targets](../targets/targets-and-profiles.md) §6 | Profiles never change behavior, analysis, the feature set, `S`, layout, or the boundary. |
| N-F-81 | [spec/10](../spec/10-errors-and-panics.md) §10.10 | A panic that would take the position of the call into a library function, a derived method or the checked entry of a library function, when that function is called through a function pointer, takes the position of the function item expression that was converted to the pointer (§10.4.5). |
| N-F-82 | [impl/10](../impl/10-runtime.md) §10.12 | A test harness reports a panic through the harness channel, the core import `sai-harness` `panic` with the address and the length of the report, called once per instance. |
| N-F-90 | [impl/06](../impl/06-wasm-lowering-and-layout.md) §6.13 | A flags type with `n` flags is a one-field struct of an unsigned integer of 1, 2 or 4 bytes (for `n` at most 8, 16 or 32), with bit `i` for flag `i`, a full valid range and no niche (§6.4.18). |
| N-F-95 | [impl/10](../impl/10-runtime.md) §10.12 | The handles of resources provided within the build live in the in-module handle table: the runtime-state item `__sai_handles` (item 6, after the items of N-F-24) with the fields `base`, `cap`, `len` and `free`, and entries of `PS + 8` bytes holding `rep`, `kind` and `lends`. |
| N-F-96 | [impl/10](../impl/10-runtime.md) §10.12 | The handle table is a runtime component that is linked only when provision glue calls it, and `sai size` attributes its functions to `SAI code` (§10.1.1, §10.1.4, §10.10). |

## N-G

Defined in package/01, package/02, package/03, package/04.

| ID | Defined in | Summary |
|---|---|---|
| N-G-1 | [package/01](../package/01-package-contract.md) §1.20 | `sai.package` and `sai.lock` use a dedicated lexical grammar (§1.2.3) with the token kinds `WORD`, `UWORD`, `PKG_PATH`, `INT`, `VERSION`, `STRING`, and a few punctuators. |
| N-G-2 | [package/01](../package/01-package-contract.md) §1.20 | NEWLINE generation uses E = {`WORD`, `UWORD`, `PKG_PATH`, `INT`, `VERSION`, `STRING`, `}`} and the continuation set C = {`from`, `dir`, `only`, `grant`}. |
| N-G-3 | [package/01](../package/01-package-contract.md) §1.20 | Package name segments are `PkgSeg` of 1–64 bytes. |
| N-G-4 | [package/01](../package/01-package-contract.md) §1.20 | Package versions are full SemVer versions; build metadata is not allowed in `sai.package` or `sai.lock`. |
| N-G-5 | [package/01](../package/01-package-contract.md) §1.20 | `compiler <req>` is the only language-version gate; it is optional and `sai new` writes it. |
| N-G-6 | [package/01](../package/01-package-contract.md) §1.20 | The package kind is inferred from the presence of `source` and `artifact`. |
| N-G-7 | [package/01](../package/01-package-contract.md) §1.20 | `targets` is required and non-empty. |
| N-G-8 | [package/01](../package/01-package-contract.md) §1.20 | `export world` names a local world (a module path relative to the root module, or a world of the `wit` package for artifact packages) or an external world `ns:pkg/world`. |
| N-G-9 | [package/01](../package/01-package-contract.md) §1.20 | Every requirement has an explicit `from` source, including the bundled standard packages. |
| N-G-10 | [package/01](../package/01-package-contract.md) §1.20 | A version requirement has exactly one lower-bound comparator (`^ ~ = >=`) and at most one upper bound (`< <=`); `>` does not exist; `=` stands alone; `=` and `<=` need full versions. |
| N-G-11 | [package/01](../package/01-package-contract.md) §1.20 | No optional, feature, or build-time dependencies. |
| N-G-12 | [package/01](../package/01-package-contract.md) §1.20 | A requirement may carry `grant { .. |
| N-G-13 | [package/01](../package/01-package-contract.md) §1.20 | `policy` items are `deny unsafe`, `deny ambient_imports`, `deny runtime_contracts` (package-local), and `deny duplicate_lines` (graph-wide, root-only). |
| N-G-14 | [package/01](../package/01-package-contract.md) §1.20 | `lints { .. |
| N-G-15 | [package/01](../package/01-package-contract.md) §1.20 | Profile blocks exist only for `dev`, `release`, and `tiny`; a profile `allow` may only lift the profile's default denials within the package `allow`; the only budget metric is `wasm` with the units `B`, `KiB`, `MiB`. |
| N-G-16 | [package/01](../package/01-package-contract.md) §1.20 | Root-only declarations (`requires test`, `lints`, `profile`, `allocator`, `overrides`, and the policy item `deny duplicate_lines`) are ignored without a diagnostic when the package is a dependency. |
| N-G-17 | [package/01](../package/01-package-contract.md) §1.20 | Overrides are `use artifact`, `use path`, and `use git`. |
| N-G-18 | [package/01](../package/01-package-contract.md) §1.20 | Metadata declarations `description`, `license` (SPDX), `authors`, and `repository` exist and never affect the build; `repository` is never used for fetching. |
| N-G-19 | [package/01](../package/01-package-contract.md) §1.20 | The only artifact kind is `artifact component`, keyed by component target names. |
| N-G-20 | [package/01](../package/01-package-contract.md) §1.20 | Paths in `sai.package` are relative and `/`-separated; `..` is allowed only for `from path` and `use path`; content paths stay inside the package directory. Paths are limited to 4096 bytes with 255-byte components and reject host-ambiguous trailing dots/spaces and Windows device names; `.git`, `.sai`, and `out` are reserved case-insensitively where applicable. |
| N-G-21 | [package/02](../package/02-resolver.md) §2.16 | Range checks treat pre-releases by plain SemVer precedence. |
| N-G-22 | [package/02](../package/02-resolver.md) §2.16 | `^V` denotes exactly the part of `line(V)` at or above `V` (partial versions are completed with zeros); `^0` and `^0.0` are `INVALID_VERSION_REQUIREMENT`. |
| N-G-23 | [package/02](../package/02-resolver.md) §2.16 | The root's `requires` and `requires test` edges are resolved together; dependencies contribute `requires` only. |
| N-G-24 | [package/02](../package/02-resolver.md) §2.16 | A lock floor applies only to a node that is reached by an edge during selection. |
| N-G-25 | [package/02](../package/02-resolver.md) §2.16 | A path package pins its node at its declared version, which must satisfy the requirement and be in its line (`PATH_VERSION_MISMATCH`). |
| N-G-26 | [package/02](../package/02-resolver.md) §2.16 | The source of a node is chosen by: root override, pin, the least requirer (identity bytes, then version) among edges that named the selected version, the lock entry. |
| N-G-27 | [package/02](../package/02-resolver.md) §2.16 | There are no branch, revision, or commit sources; commits appear only in the lock. |
| N-G-28 | [package/02](../package/02-resolver.md) §2.16 | A cycle among selected package versions is `DEPENDENCY_CYCLE`; another line of the root's own package is not a cycle. |
| N-G-29 | [package/02](../package/02-resolver.md) §2.16 | Mode A: when the lock is consistent with the root contract (§2.10.2), it is used unchanged and no selection runs. |
| N-G-30 | [package/02](../package/02-resolver.md) §2.16 | `sai update` raises versions within their lines using tag listings, probes each node once, never edits `sai.package`, never changes lines or pins, and fails without writing on a conflict. |
| N-G-31 | [package/02](../package/02-resolver.md) §2.16 | `sai add` lists tags to find the highest release, checks the identity, writes `^<version>` with an explicit source in canonical layout, resolves, reviews, and writes both files only on success. |
| N-G-32 | [package/02](../package/02-resolver.md) §2.16 | Capability escalation is detected per node by comparing footprints with the previous lock under the covers relation; escalations and source changes require approval, and the lock is the approval record. |
| N-G-33 | [package/02](../package/02-resolver.md) §2.16 | Explanation chains are shortest paths from the root in the visited graph, with ties broken by identity bytes and version; non-selected versions are marked. |
| N-G-34 | [package/02](../package/02-resolver.md) §2.16 | The release tag of `v` is exactly `v<canonical v>` or `<dir>/v<canonical v>`; when listing, other refs are ignored. |
| N-G-35 | [package/02](../package/02-resolver.md) §2.16 | `sai:core` and `sai:alloc` are not graph nodes and are not locked; the `compiler` requirement relates packages to them. |
| N-G-36 | [package/03](../package/03-lock-file.md) §3.12 | `sai.lock` has exactly one canonical byte form: LF only, `lock 1` header, sorted blocks separated by one empty line, four-space field indentation, single spaces, one-line lists, `\"` and `\\` as the only escapes, no comments, timestamps, or toolchain version (§3.2.3). |
| N-G-37 | [package/03](../package/03-lock-file.md) §3.12 | The `sai1:` tree hash is computed from the Git tree of the locked commit; it selects regular files only (both file modes hashed alike), skips symbolic links, submodules, `.git` components, and top-level `.sai` and `out`, applies no Git attributes or LFS, validates every path, rejects case- and normalization-insensitive collisions, and sorts by path bytes (§3.4). |
| N-G-38 | [package/03](../package/03-lock-file.md) §3.12 | Digests are written as `sai1:` followed by 64 lowercase hexadecimal digits; no other encoding is valid (§3.4.6). |
| N-G-39 | [package/03](../package/03-lock-file.md) §3.12 | An artifact checksum is SHA-256 of the artifact file's bytes with the same `sai1:` encoding; the WIT checksum is the tree hash of the WIT directory (§3.5, §3.6). |
| N-G-40 | [package/03](../package/03-lock-file.md) §3.12 | Locks are compared semantically, through their canonical serialization. |
| N-G-41 | [package/03](../package/03-lock-file.md) §3.12 | `--locked` requires Mode A and fails without fetching tags; `--offline` uses only the cache and the bundled index and never selects differently because of what is cached; `--frozen` is both (§3.8). |
| N-G-42 | [package/03](../package/03-lock-file.md) §3.12 | After a merge, `sai lock` reconstructs both sides of a conflicted lock, takes per-node maximum floors, rejects differing content for one version, uses the union of approvals, and resolves the merged `sai.package` in Mode B. |
| N-G-43 | [package/03](../package/03-lock-file.md) §3.12 | Format 1 has a fixed field set with no ignorable extensions; new fields or checksum algorithms need a new format; existing locks keep their format and are never downgraded (§3.10). |
| N-G-44 | [package/03](../package/03-lock-file.md) §3.12 | The lock records the root block with the root's edges, and every entry's edges with their resolved versions. |
| N-G-45 | [package/03](../package/03-lock-file.md) §3.12 | Content is verified when it is fetched, before it enters the cache; builds trust the read-only, checksum-named cache; a mismatch is never resolved by updating the lock (§3.7.3). |
| N-G-46 | [package/03](../package/03-lock-file.md) §3.12 | Every package, application or library, SHOULD commit its lock; a lock is created even without dependencies; `sai.package` is written before `sai.lock`, each atomically, and concurrent writers are serialized (§3.1.1, §3.7). |
| N-G-47 | [package/04](../package/04-distribution-and-security.md) §4.13 | Git URLs are `https` or `ssh` only; `http` and `git` are `INSECURE_GIT_URL`; scp-like, `file`, local paths, other schemes, non-ASCII hosts, percent-encoding, queries, and fragments are `UNSUPPORTED_GIT_URL`; credentials in URLs are `CREDENTIALS_IN_URL`; the canonical form lowercases scheme and host, drops a default port and a trailing `/`, and changes nothing else (§4.2). |
| N-G-48 | [package/04](../package/04-distribution-and-security.md) §4.13 | Git transport details (protocol version, depth, filters, negotiation) are deliberately left to the implementation; only the Git configuration of §4.2.3 is honored (§4.3.1). |
| N-G-49 | [package/04](../package/04-distribution-and-security.md) §4.13 | The lock is authoritative after the first fetch: a release tag that no longer peels to the locked commit is `TAG_MOVED`; a deleted tag with an available commit is `TAG_MISSING`; replacing locked content under the same version needs approval in `sai update` (§4.3.3). |
| N-G-50 | [package/04](../package/04-distribution-and-security.md) §4.13 | One global, content-addressed package cache per user, located by `SAI_CACHE_DIR` or the host's cache directory, with a versioned layout; `src/` and tag records are normative, other internal formats are deliberately private (§4.4.1). |
| N-G-51 | [package/04](../package/04-distribution-and-security.md) §4.13 | Fixed, non-configurable size limits: a 16 MiB `sai.package` manifest from any source; and for fetched content, 100,000 files, 256 MiB per file, 1 GiB per package, 4 GiB per fetch, 4096/255-byte paths, and 100,000 tags (`PACKAGE_TOO_LARGE`, §4.4.4). |
| N-G-52 | [package/04](../package/04-distribution-and-security.md) §4.13 | Dependents see artifact and hybrid packages only through the SAI view of their WIT, in both forms; the WIT generated from a hybrid package's source MUST equal its declared world (`HYBRID_WIT_MISMATCH`, §4.5.3). |
| N-G-53 | [package/04](../package/04-distribution-and-security.md) §4.13 | There is no yanking in v0.1; broken releases are superseded by new versions; tags MUST NOT be deleted or moved (§4.6.2). |
| N-G-54 | [package/04](../package/04-distribution-and-security.md) §4.13 | The review has the headings Package, Source, Form, Capabilities, Supported, Not supported, Estimated contribution, Needs approval, Unused source clauses, in this order, with fixed JSON keys (§4.7.1). |
| N-G-55 | [package/04](../package/04-distribution-and-security.md) §4.13 | Approval items are `CAPABILITY_ESCALATION`, `SOURCE_CHANGED` (a different canonical URL or `dir` for a Git node in both locks), and `TAG_MOVED` content replacement; approval is all-or-nothing per command and recorded by the written lock (§4.7.2). |
| N-G-56 | [package/04](../package/04-distribution-and-security.md) §4.13 | The package system passes to the compiler only each node's identity, version, verified files, and form; URLs, commits, and the cache location are not compiler inputs (§4.8). |
| N-G-57 | [package/04](../package/04-distribution-and-security.md) §4.13 | Git fetching is the toolchain's only network access; there are no update checks or telemetry; `--offline` means no DNS lookup and no connection of any kind (§4.9). |
| N-G-58 | [package/04](../package/04-distribution-and-security.md) §4.13 | No code from a package runs while it is resolved, fetched, or compiled: no build scripts, macros, plugins, hooks, package-run code generation, or execution of artifacts; building commands never run dependency tests (§4.10). |
| N-G-59 | [package/04](../package/04-distribution-and-security.md) §4.13 | A node new in a review is checked for confusable identities against the graph, the bundled packages, and the root by a fixed skeleton rule and a Levenshtein-1 rule; matches are the warning `CONFUSABLE_PACKAGE_NAME` and never block (§4.11.1). |
| N-G-60 | [package/04](../package/04-distribution-and-security.md) §4.13 | A hybrid dependency in source form provides the interfaces of its `export world` within the build, through Canonical ABI glue inside the root's module, on every target and profile; provided interfaces are not imports of the root's component but remain required capabilities, and each compatibility line provides its own version (§4.5.3 item 4). |
| N-G-61 | [package/04](../package/04-distribution-and-security.md) §4.13 | An interface provided by a dependency never crosses the boundary of the root's component: it is not exported, its types are in no boundary type of an export or remaining import, and an interface provided within the build is not imported by an artifact (`PROVIDED_INTERFACE_AT_BOUNDARY`, §4.5.3 item 5). |

## N-H

Defined in impl/11, impl/13, impl/14, impl/12.

| ID | Defined in | Summary |
|---|---|---|
| N-H-1 | [impl/11](../impl/11-diagnostics.md) §11.15 | Codes, their stability rules (§11.2, §11.12), the span encoding (§11.5), the JSON stream format (§11.9), and the explanation record (§11.13.2) are normative for every conforming tool. |
| N-H-2 | [impl/11](../impl/11-diagnostics.md) §11.15 | Codes match `^[A-Z][A-Z0-9]*(_[A-Z][A-Z0-9]*)+$`, have 2–6 words and at most 40 characters, name the problem rather than the fix (except `NO_`), use the leading-word vocabulary of §11.2.2 and only the listed abbreviations, and carry no numeric, phase, or package prefix. |
| N-H-3 | [impl/11](../impl/11-diagnostics.md) §11.15 | There are three severities, `error`, `warning`, and `info`. |
| N-H-4 | [impl/11](../impl/11-diagnostics.md) §11.15 | After level resolution, lint occurrences in packages other than the root package and the workspace members are dropped. |
| N-H-5 | [impl/11](../impl/11-diagnostics.md) §11.15 | Candidates are names at Damerau–Levenshtein distance at most `max(1, ⌊n/3⌋)` from a name of `n` characters. |
| N-H-6 | [impl/11](../impl/11-diagnostics.md) §11.15 | Byte offsets count the raw bytes of the file, including a BOM and CR characters. |
| N-H-7 | [impl/11](../impl/11-diagnostics.md) §11.15 | Locations name files by a `source_ref` with a workspace-relative path, a `dep:<ns>:<pkg>@<version>/<path>` path for dependencies, or `<stdin>`. |
| N-H-8 | [impl/11](../impl/11-diagnostics.md) §11.15 | The human layout of §11.10 replaces the layout of draft §60. |
| N-H-9 | [impl/11](../impl/11-diagnostics.md) §11.15 | Fixes have one of three applicabilities: `machine-applicable`, `maybe-incorrect`, or `has-placeholders`. |
| N-H-10 | [impl/11](../impl/11-diagnostics.md) §11.15 | A fix's edits are computed by formatting the smallest enclosing format unit, and they change nothing outside it. |
| N-H-11 | [impl/11](../impl/11-diagnostics.md) §11.15 | Effect sets in generated text use the canonical order of spec/08 §8.2.3 (N-D-2). |
| N-H-12 | [impl/11](../impl/11-diagnostics.md) §11.15 | `related` carries typed explanations (`effect_chain`, `capability_chain`, `borrow_conflict`, `use_after_move`, `requirement_chain`, `declared_here`) with at most 32 chain steps, shortened to the first 16, a marker, and the last 15. |
| N-H-13 | [impl/11](../impl/11-diagnostics.md) §11.15 | The JSON form is an NDJSON stream on standard output of exactly one header, diagnostic records, result records, and one summary. |
| N-H-14 | [impl/11](../impl/11-diagnostics.md) §11.15 | Diagnostics are ordered by location class, package rank, file rank, start byte, end byte, severity, code, message, and serialization. |
| N-H-15 | [impl/11](../impl/11-diagnostics.md) §11.15 | `--max-errors` defaults to 50 and limits rendered errors only. |
| N-H-16 | [impl/11](../impl/11-diagnostics.md) §11.15 | Cascade suppression uses poison facts (rules C1–C6 of §11.11.4) and phase gating. |
| N-H-17 | [impl/13](../impl/13-cli.md) §13.9 | Command names, option names, exit codes, and JSON result records are normative for every conforming tool that provides the command. |
| N-H-18 | [impl/13](../impl/13-cli.md) §13.9 | The CLI reads no configuration file. |
| N-H-19 | [impl/13](../impl/13-cli.md) §13.9 | `sai fetch` and `sai edit` are added to the commands of draft §77 and D-TOOL-03. |
| N-H-20 | [impl/13](../impl/13-cli.md) §13.9 | Workspace members are the root plus the `from path` dependencies inside the root's directory. |
| N-H-21 | [impl/13](../impl/13-cli.md) §13.9 | Long options with `--name value` or `--name=value`, four short aliases, no combined short options, global options anywhere, and `--` for pass-through. |
| N-H-22 | [impl/13](../impl/13-cli.md) §13.9 | Color is decided by `--color`, then `CLICOLOR_FORCE`, then `NO_COLOR`, then terminal detection per stream. |
| N-H-23 | [impl/13](../impl/13-cli.md) §13.9 | Exit codes 0, 1, 2, 3, 4, 101, and 130 with a fixed precedence. |
| N-H-24 | [impl/13](../impl/13-cli.md) §13.9 | In v0.1 the toolchain version is the language version checked by `compiler` requirements. |
| N-H-25 | [impl/13](../impl/13-cli.md) §13.9 | The default target is the first entry of `targets`. |
| N-H-26 | [impl/13](../impl/13-cli.md) §13.9 | Naming one code in two different CLI level flags is an error. |
| N-H-27 | [impl/13](../impl/13-cli.md) §13.9 | Output file names are fixed: `<name>.wasm`, `<name>.component.wasm`, and `wit/<name>.wit`. |
| N-H-28 | [impl/13](../impl/13-cli.md) §13.9 | `.sai/index/` is local state and is listed in `.gitignore` by `sai new`. |
| N-H-29 | [impl/13](../impl/13-cli.md) §13.9 | The files in `out/` are byte-identical for identical inputs, independent of paths, time, host, `--jobs`, caches, and incrementality. |
| N-H-30 | [impl/13](../impl/13-cli.md) §13.9 | Approval: `--approve` approves. |
| N-H-31 | [impl/13](../impl/13-cli.md) §13.9 | Result records use package references `ns:pkg@version`, file records with SHA-256, and arrays in a stated or byte order. |
| N-H-32 | [impl/13](../impl/13-cli.md) §13.9 | `sai new` has one template, a library with one function and one test, and writes no lock. |
| N-H-33 | [impl/13](../impl/13-cli.md) §13.9 | `sai check` reports every error that `sai build` would report except those that need generated code. |
| N-H-34 | [impl/13](../impl/13-cli.md) §13.9 | The root package is always compiled from source. |
| N-H-35 | [impl/13](../impl/13-cli.md) §13.9 | `sai run` uses an embedded runtime and grants only the authority given by `--env`, `--dir`, and `--allow-network`. |
| N-H-36 | [impl/13](../impl/13-cli.md) §13.9 | `sai test` streams results in test order, independent of `--jobs`, and the command line adds no authority to tests. |
| N-H-37 | [impl/13](../impl/13-cli.md) §13.9 | `sai add` appends a new requirement as the last entry of its block. |
| N-H-38 | [impl/13](../impl/13-cli.md) §13.9 | Opt-in Wasm features are enabled only with the repeatable `--wasm-feature` option, whose values are `tail-call` and `simd128`. |
| N-H-39 | [impl/13](../impl/13-cli.md) §13.9 | `sai lsp` uses standard input and output for the protocol. |
| N-H-40 | [impl/13](../impl/13-cli.md) §13.9 | The global cache is located by `SAI_CACHE_DIR`, which defaults to the platform cache directory, exactly as package/04 §4.4.1 (N-G-50) defines. |
| N-H-41 | [impl/13](../impl/13-cli.md) §13.9 | `sai edit` refuses an operation that breaks the public API unless `--allow-breaking` is given. |
| N-H-42 | [impl/13](../impl/13-cli.md) §13.9 | `sai diff` compares `HEAD` with the working tree by default, and takes up to two Git revisions resolved by `gix`. |
| N-H-43 | [impl/14](../impl/14-formatter.md) §14.14 | The formatter refuses a source file if and only if the lexer reported an error, or the parser reported an error other than `CLAUSE_ORDER` (the `parser` rows of impl/02 §2.5.12). |
| N-H-44 | [impl/14](../impl/14-formatter.md) §14.14 | The output layout does not depend on the input layout, except for comments and blank lines between format units. |
| N-H-45 | [impl/14](../impl/14-formatter.md) §14.14 | The width of a line is its number of Unicode scalar values. |
| N-H-46 | [impl/14](../impl/14-formatter.md) §14.14 | After formatting, the output is re-parsed, its normal form (§14.11.1) is compared with that of the input, and the multisets of comment texts are compared. |
| N-H-47 | [impl/14](../impl/14-formatter.md) §14.14 | Every block and body is printed broken, one statement per line, whatever its length. |
| N-H-48 | [impl/14](../impl/14-formatter.md) §14.14 | Each signature clause (`where`, `borrows`, `effects`, `requires`, `ensures`) is printed on its own line, one level deeper than the item, even if the signature fits on one line. |
| N-H-49 | [impl/14](../impl/14-formatter.md) §14.14 | A broken operator chain breaks before every operator of its precedence level, with the operands at +4. |
| N-H-50 | [impl/14](../impl/14-formatter.md) §14.14 | A member chain is a head followed by at least two segments with calls. |
| N-H-51 | [impl/14](../impl/14-formatter.md) §14.14 | A call whose last argument is a closure with a block body that does not fit is printed with the other arguments on the call line and the closure body broken (hugging). |
| N-H-52 | [impl/14](../impl/14-formatter.md) §14.14 | The right-hand side after `=` (in `let`, `var`, assignments, `const`, aliases, refined and distinct types) and after `=>` is a `Choice` of five alternatives, in order: flat on the same line; flat on the next line at +4; broken starting on the same line; broken starting on the next line; broken on the same line (overflow). |
| N-H-53 | [impl/14](../impl/14-formatter.md) §14.14 | `use` declarations are moved to the top of the file, after the module header, in two groups (`use`, then `pub use`) separated by one blank line. |
| N-H-54 | [impl/14](../impl/14-formatter.md) §14.14 | Blank lines are kept only between format units, at most one. |
| N-H-55 | [impl/14](../impl/14-formatter.md) §14.14 | Comments in the leading trivia of a unit's first token that are separated from it by a blank line are detached: they keep their position when units are reordered. |
| N-H-56 | [impl/14](../impl/14-formatter.md) §14.14 | Continuation lines of simple strings and the content lines of multiline strings are reindented to the output indentation plus 4, without changing the string's value. |
| N-H-57 | [impl/14](../impl/14-formatter.md) §14.14 | The exponent marker of a float literal is printed as `e` (§14.1.3, §14.4.1). |
| N-H-58 | [impl/14](../impl/14-formatter.md) §14.14 | The leading ` |
| N-H-59 | [impl/14](../impl/14-formatter.md) §14.14 | In a broken brace list, an element other than the last that ends with an open range (`a..`, `..`) is wrapped in parentheses (§14.5.4 rule 5). |
| N-H-60 | [impl/14](../impl/14-formatter.md) §14.14 | Formatting a format unit of a file that is canonical outside the edited range produces exactly the text that formatting the whole file would produce. |
| N-H-61 | [impl/14](../impl/14-formatter.md) §14.14 | The canonical one-line text of a node is the formatter output with every group flat and unlimited width, without comments, with multi-element blocks written `{ … }` and strings with line breaks written as escaped simple strings (§14.13). |
| N-H-62 | [impl/14](../impl/14-formatter.md) §14.14 | `sai.package` files are formatted without reordering anything. |
| N-H-63 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | A symbol is an entity with a canonical path of spec/04 §4.9.1 that is not a package or a module: items, members, trait and interface impls and their items, generic parameters, tests, properties, and WIT items. |
| N-H-64 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | The URI authority uses `%3A` for the package separator; path escapes are normalized to uppercase. |
| N-H-65 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | The hexadecimal part is the 24 most significant bits of SipHash-1-3 (zero key) of `sai-symbol` `0x00` `<ns>:<pkg>` `0x00` `<canonical path>` `0x00` `k` (little-endian `u32`), for the smallest probe `k` that collides with no symbol allocated before it in canonical-path byte order and no hexadecimal part reserved by the symbol index. |
| N-H-66 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | A package identity that matches several package nodes of the resolved graph selects, in this order: the root package or a workspace member; the node that the root package requires directly; the node with the highest selected version. |
| N-H-67 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | Reconciliation of the symbol index matches current symbols to entries by `DefPathHash` (by canonical path if the compatibility line changed), then applies recorded changes of operations, rekeys members with their parent, and finally rematches symbols that have a unique partner with the same kind and fingerprint. |
| N-H-68 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | The hexadecimal part of a node ID is the pre-order index of the element among all nodes and tokens of the file's red tree, trivia excluded, written in uppercase hexadecimal with at least four digits (§12.3.2). |
| N-H-69 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | A graph query follows the edges of the selected kinds forward, in reverse, or both; starts at the given nodes or, without starts, at the roots of §12.4.3 rule 3; and prints one depth-first tree per start, with an expanded set per tree, the marks `seen` and `dep`, and children sorted forward before reverse, then by edge kind, printed form, and node key (§12.4.3). |
| N-H-70 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | Every operation of §12.6 obeys the common rules of §12.6.2: typed parameter values; nodes inside the target; edits only in workspace packages; no planning with syntax errors; edits made on the CST and formatted per format unit; the result is analyzed and must introduce no error (except the expected use-site errors of `change_type`), and refactorings preserve meaning; the symbol index is updated by the same command; application is atomic and deterministic. |
| N-H-71 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | In `type` and `expr` operation values, a path may start with a package path as in a `use` declaration, followed by `.` and an item name (`acme:user/model.Email`). |
| N-H-72 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | Every diff change has a `public_api` value: `none` if no public entity (spec/04 §4.6.7, including `@doc(hidden)` items, spec/15 §15.13 rule 4) is changed; otherwise `breaking` if one of the twelve rules of §12.8.3 applies, else `compatible`. |
| N-H-73 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | The semantic diff does not compare function bodies, comments, formatting, or declaration order. |
| N-H-74 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | Diff entities are matched by same canonical path and kind, then by same kind and hexadecimal part of the symbol ID, then by unique kind-and-fingerprint partner, with members following their parent after each match by path change (§12.8.1 rule 3). |
| N-H-75 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | Impact analysis counts distinct call sites in workspace files, tests and properties, exported interfaces, and component worlds, from the before version on the graphs of §12.4.2, and the size change only on request. |
| N-H-76 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | The symbol index uses one JSON Lines file per workspace package node; its header lists unique UTF-8-byte-sorted basenames with exact-byte BLAKE3 digests. |
| N-H-77 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | Every index-changing LSP edit uses a durable journal until the planned source and index states are committed. |
| N-H-78 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | The language server uses the `utf-8` position encoding if the client offers it, and `utf-16` otherwise. |
| N-H-79 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | Semantic tooling analyzes a workspace snapshot as `sai check --tests` does, so tests and test-only code are in every graph and are updated by every operation. |
| N-H-80 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | LSP names a file of a dependency by the `file` URI of its copy in the package cache. |
| N-H-81 | [impl/14](../impl/14-formatter.md) §14.14 | The field list of a record or resource declaration and the variant list of an enum declaration are always broken when they are not empty, one element per line, even when they fit (§14.5.4 rule 4, §14.7.3). |
| N-H-82 | [impl/13](../impl/13-cli.md) §13.9 | The test harness is one core module of the target's pointer width on every Wasm target, and it is never wrapped as a component. |
| N-H-83 | [impl/13](../impl/13-cli.md) §13.9 | The runner drives the cases of a property from the host, with one call of `case.<k>(i)` per case in one instance. |
| N-H-84 | [impl/13](../impl/13-cli.md) §13.9 | A failure is a `panic` exactly when the harness channel delivered a report. |
| N-H-85 | [impl/13](../impl/13-cli.md) §13.9 | Every test runs in a fresh store with the same engine configuration on every host: the target's features, NaN canonicalization, no fuel, no epoch interruption, no memory limit below the target's, and an 8 MiB engine stack (§13.6.6.3). |
| N-H-86 | [impl/13](../impl/13-cli.md) §13.9 | The selection options of `sai test` are the input `test_options`, and the selection is the query `test_selection`, which reads only item trees. |
| N-H-87 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | The semantic diff compares the unsafe inventories (`sai inspect unsafe`) of every dependency node whose lock entry changed version, source or checksum, or that is new, paired by identity and compatibility line, then by unique identity. |
| N-H-88 | [impl/12](../impl/12-semantic-tooling.md) §12.12 | An empty symbol package file is zero bytes; nonempty JSONL files have one canonical object and LF per entry, with no blank records. |

## N-I

Defined in spec/11, spec/12, spec/14.

| ID | Defined in | Summary |
|---|---|---|
| N-I-1 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | There are no async closures. |
| N-I-2 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | `.await` and `for await` are allowed only when the innermost body is an `async fn` or `async` block. |
| N-I-3 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | `async fn` is rejected in traits and trait impls (`ASYNC_FN_IN_TRAIT`), in extern blocks (`INVALID_EXTERN_SIGNATURE`) and as `main` (`ASYNC_MAIN`). |
| N-I-4 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | There is no `Future` or `Stream` impl for `borrow mut F`. |
| N-I-5 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | Polling a completed anonymous future panics with `"future polled after completion"`. |
| N-I-6 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | The `poll` contract consists of no move after the first poll, the drop guarantee (no freeing or reuse of a polled future's storage without dropping it in place), a valid context and exclusive access. |
| N-I-7 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | `regions(Fut_f)` is the union of the regions of all parameter types. |
| N-I-8 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | Dropping a suspended future drops the occupied await slot first, then the locals of the active scopes innermost-first in reverse declaration order, then the parameters (reverse order, `self` last) or the by-move captures (capture order). |
| N-I-9 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | `for await` takes a `Stream` by value (no `IntoStream`), keeps it in a hidden pinned slot, and expands to a `loop` over an `await_next` suspension point. |
| N-I-10 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | Because `async` is a strict keyword, `sai:async` is imported with an item list or an alias. |
| N-I-11 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | `scope` is a designated item ([16](../spec/16-core-library.md) N-J-26), not a lang item, with a fresh region `'s`. |
| N-I-12 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | The combinators and executors of `sai:async` get no special effect rule. |
| N-I-13 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | A scope polls, in each round, the body first and then the woken or never-polled children in start order, including children started during the round. |
| N-I-14 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | The child task method is `Scope.spawn`, the name of the draft's vocabulary (`spawn` is not a reserved word, D-LEX-04). |
| N-I-15 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | `join` and `select` pass the parent context through and poll every incomplete operand on every poll, in argument order. |
| N-I-16 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | `block_on` drives one root task with a refcounted waker and a reactor local to the call. |
| N-I-17 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | WASI leaf futures register pollables, one-shot, with the reactor of the context's executor extension. |
| N-I-18 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | A core `export async fn`, or an exported interface with an `async fn` on a target without the CM async ABI, is `ASYNC_EXPORT_UNSUPPORTED`. |
| N-I-19 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | `Send` and `Share` cannot be declared or used as import aliases (`RESERVED_TYPE_NAME`). |
| N-I-20 | [spec/11](../spec/11-async-and-concurrency.md) §11.14 | There is no async test form. |
| N-I-21 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | An `unsafe` block makes every expression lexically inside it an unsafe context, except the bodies of nested closures and `async` blocks. |
| N-I-22 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | The body of an `unsafe fn` is an unsafe context. |
| N-I-23 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | Every impl of an `unsafe trait` is `unsafe impl`, and `unsafe impl` is only for unsafe traits. |
| N-I-24 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | In an `unsafe module`, every function, closure and `async` block body is an unsafe context; predicates never are. |
| N-I-25 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | `Ptr<T>` and `PtrMut<T>` are non-null addresses with no region, `Copy`, and compared and hashed by address. |
| N-I-26 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | Every type has a validity invariant, listed per type. |
| N-I-27 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | Accesses through raw pointers obey the loans that 07 computes for the whole program: no write while a shared loan of the place is live, no access while another exclusive loan is live, except through pointers derived from that loan. |
| N-I-28 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | Unsafe code MUST NOT perform a primitive operation that its effect set does not permit. |
| N-I-29 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | The host may read linear memory during an `extern "wasm"` call; it may write it only during a call of an import declared `unsafe fn`. |
| N-I-30 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | `extern "wasm"` signatures use boundary scalars only (integers, floats, `Bool`, `Char`, `Byte`, unrefined distinct types over them, and `()` as the result; otherwise `EXPORT_FN_NON_SCALAR`). |
| N-I-31 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | Import field names are the function name without `r#` or the `@import_name` string, with no restriction. |
| N-I-32 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | `transmute` requires equal sizes known on the generic signature (`TRANSMUTE_SIZE_MISMATCH`), consumes its argument without drop glue, and needs a valid result. |
| N-I-33 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | An exported `export fn` has boundary-scalar parameters and result, like an `extern "wasm"` import (`EXPORT_FN_NON_SCALAR`), and no generic, `where` or `borrows` form. |
| N-I-34 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | `extern "C"` blocks are `unsafe`, may name a library with `module`, use C-identifier symbols (`INVALID_LINK_NAME`) and FFI types only (`INVALID_EXTERN_SIGNATURE`), have the effect `ffi`, need `native_ffi`, and mark the package non-portable. |
| N-I-35 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | `sai inspect unsafe` lists every unsafe block, unsafe fn, unsafe trait and impl, unsafe module and unsafe extern block of the whole graph, with canonical path, symbol and node IDs, location, contained operations and `# Safety` text, in a deterministic order. |
| N-I-36 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | SAI has no untagged unions. |
| N-I-37 | [spec/14](../spec/14-testing.md) §14.8 | A `test` body is checked as the body of a non-generic, package-private function without parameters or clauses that returns `()`. |
| N-I-38 | [spec/14](../spec/14-testing.md) §14.8 | A property's parameter types must implement `Arbitrary`, and its parameters are owned. |
| N-I-39 | [spec/14](../spec/14-testing.md) §14.8 | Tests and properties are analyzed only by `sai test` and `sai check --tests`, for the selected packages. |
| N-I-40 | [spec/14](../spec/14-testing.md) §14.8 | A `use` of a `requires test` dependency is test-only. |
| N-I-41 | [spec/14](../spec/14-testing.md) §14.8 | Tests are hermetic: the capabilities of the entry set of a test or property, which includes its generators and counterexample formatting, must be a subset of `{ alloc }`, otherwise `TEST_EFFECT_NOT_PERMITTED`. |
| N-I-42 | [spec/14](../spec/14-testing.md) §14.8 | The outcomes are `passed` and `failed`. |
| N-I-43 | [spec/14](../spec/14-testing.md) §14.8 | A property runs 100 cases unless it has `@cases`. |
| N-I-44 | [spec/14](../spec/14-testing.md) §14.8 | Case `i` of `N` uses the seed of step `i + 1` of SplitMix64 from the base seed and the size `⌊(i + 1) × 100 / N⌋`, with its own `Gen`. |
| N-I-45 | [spec/14](../spec/14-testing.md) §14.8 | A filtering derived impl makes at most 100 attempts per value and then panics with `Arbitrary: no value of <T> found in 100 attempts`, located at the parameter type when the property calls it. |
| N-I-46 | [spec/14](../spec/14-testing.md) §14.8 | There is no shrinking. |
| N-I-47 | [spec/14](../spec/14-testing.md) §14.8 | A test build has as roots only the entries of the selected tests, uses the selected target and profile, and is made only when test analysis reports no error. |
| N-I-48 | [spec/14](../spec/14-testing.md) §14.8 | Each `test` and each property runs in its own fresh instance. |
| N-I-49 | [spec/14](../spec/14-testing.md) §14.8 | A test name must be non-empty and contain no `Cc` character (`INVALID_TEST_NAME`), and names must be unique per module across tests and properties after `@target` filtering (`DUPLICATE_TEST_NAME`), checked in every build. |
| N-I-50 | [spec/14](../spec/14-testing.md) §14.8 | A test build always has the harness channel as its reporting channel, so its panic mode is `Report` under `dev` and `release` and `TrapOnly` under `tiny`. |
| N-I-51 | [spec/12](../spec/12-unsafe-and-ffi.md) §12.12 | The semantic diff compares the unsafe inventory of every dependency whose lock entry changes version, source or checksum, and of every new dependency, against its previous version, and reports added and removed entries and changed `# Safety` sections. |
| N-I-52 | [spec/14](../spec/14-testing.md) §14.8 | A trait bound through a test-only binding is in scope only in tests and properties. |

## N-J

Defined in spec/01, spec/16, impl/01, impl/15.

| ID | Defined in | Summary |
|---|---|---|
| N-J-1 | [spec/01](../spec/01-introduction.md) §1.11 | Targets required for v0.1 conformance |
| N-J-2 | [spec/01](../spec/01-introduction.md) §1.11 | Closed lists of implementation-defined and unspecified behavior |
| N-J-3 | [spec/01](../spec/01-introduction.md) §1.11 | Signature listings |
| N-J-4 | [spec/01](../spec/01-introduction.md) §1.11 | Implementation limits |
| N-J-5 | [spec/01](../spec/01-introduction.md) §1.11 | Form of `FEATURE_NOT_IMPLEMENTED` |
| N-J-6 | [spec/16](../spec/16-core-library.md) §16.19 | Inherent impls of the implicit packages |
| N-J-7 | [spec/16](../spec/16-core-library.md) §16.19 | Library effects are declared |
| N-J-8 | [spec/16](../spec/16-core-library.md) §16.19 | Location of library panics |
| N-J-9 | [spec/16](../spec/16-core-library.md) §16.19 | Stages of library items |
| N-J-10 | [spec/16](../spec/16-core-library.md) §16.19 | Prelude contents |
| N-J-11 | [spec/16](../spec/16-core-library.md) §16.19 | Sealed traits |
| N-J-12 | [spec/16](../spec/16-core-library.md) §16.19 | Effect bounds of the fundamental traits |
| N-J-13 | [spec/16](../spec/16-core-library.md) §16.19 | No reflexive or blanket conversions |
| N-J-14 | [spec/16](../spec/16-core-library.md) §16.19 | Range indexing through a temporary place |
| N-J-15 | [spec/16](../spec/16-core-library.md) §16.19 | `unwrap` messages without the payload |
| N-J-16 | [spec/16](../spec/16-core-library.md) §16.19 | Float-to-integer conversion saturates |
| N-J-17 | [spec/16](../spec/16-core-library.md) §16.19 | No transcendental float functions |
| N-J-18 | [spec/16](../spec/16-core-library.md) §16.19 | Sorting |
| N-J-19 | [spec/16](../spec/16-core-library.md) §16.19 | No blanket `IntoIterator` |
| N-J-20 | [spec/16](../spec/16-core-library.md) §16.19 | Infallible writers |
| N-J-21 | [spec/16](../spec/16-core-library.md) §16.19 | Format specifier rules |
| N-J-22 | [spec/16](../spec/16-core-library.md) §16.19 | Float formatting |
| N-J-23 | [spec/16](../spec/16-core-library.md) §16.19 | Region-polymorphic constructors instead of `From` |
| N-J-24 | [spec/16](../spec/16-core-library.md) §16.19 | Region desugaring |
| N-J-25 | [spec/16](../spec/16-core-library.md) §16.19 | Normative items of other standard packages |
| N-J-26 | [spec/16](../spec/16-core-library.md) §16.19 | Designated items |
| N-J-27 | [impl/01](../impl/01-architecture.md) §1.15 | Error gates |
| N-J-28 | [impl/01](../impl/01-architecture.md) §1.15 | A fixed crate graph |
| N-J-29 | [impl/01](../impl/01-architecture.md) §1.15 | Pure queries and session identifiers |
| N-J-30 | [impl/01](../impl/01-architecture.md) §1.15 | Determinism rules |
| N-J-31 | [impl/01](../impl/01-architecture.md) §1.15 | The persisted cache is an optimization only |
| N-J-32 | [impl/01](../impl/01-architecture.md) §1.15 | Dependency metadata |
| N-J-33 | [impl/01](../impl/01-architecture.md) §1.15 | Internal compiler errors |
| N-J-34 | [impl/01](../impl/01-architecture.md) §1.15 | No logging configuration in the shipped binary |
| N-J-35 | [impl/01](../impl/01-architecture.md) §1.15 | The embedded library is built by the compiler that uses it |
| N-J-36 | [impl/01](../impl/01-architecture.md) §1.15 | Every format has a version |
| N-J-37 | [impl/01](../impl/01-architecture.md) §1.15 | Performance targets |
| N-J-38 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | The conformance suite is black-box |
| N-J-39 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | Hermetic and deterministic tests |
| N-J-40 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | Test files are canonical |
| N-J-41 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | Expected diagnostics are matched by line and code |
| N-J-42 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | Golden files are blessed and reviewed |
| N-J-43 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | A pinned, deterministic execution engine |
| N-J-44 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | Configurations must agree |
| N-J-45 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | Fuzzing is continuous; its corpus is a gate |
| N-J-46 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | The specification is tested |
| N-J-47 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | Coverage per stage and complete claims |
| N-J-48 | [impl/15](../impl/15-conformance-and-testing.md) §15.13 | Differential oracles are independent |

## N-K

Defined in impl/16.

| ID | Defined in | Summary |
|---|---|---|
| N-K-1 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | WIT loading, the model, the printer and view `DefId`s live in a new crate `sai_wit` below `sai_resolve`. |
| N-K-2 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | Dependents see a package through exactly one WIT unit: `Dir` for artifact and hybrid packages, `Generated` for other source packages. |
| N-K-3 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | A `deps/` directory is ignored. |
| N-K-4 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | Every target loads WASI at the WASI version of targets §2.5, including core-module and native targets. |
| N-K-5 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | Unsupported WIT constructs, including world-level types and inline interfaces in worlds, are recorded at load time and reported at each use: a path, an impl of the interface, or a world reference. |
| N-K-6 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | View packages have the keys `wit:wasi:…`, `wit:dir:…` and `wit:gen:…`, and `DefPath`s built from SAI names, so view `DefId`s are stable and independent of query order. |
| N-K-7 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | The interfaces of a source dependency are seen through its generated view. |
| N-K-8 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | Shadowed WIT items get no `DefId`. |
| N-K-9 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | Implemented interface functions and bound resources are realized as a redirect of resolutions. |
| N-K-10 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | A view resource that occurs in a record field, variant payload or alias target of any visible WIT interface cannot be bound (`INVALID_RESOURCE_BINDING`). |
| N-K-11 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | View resources and resources of local interfaces carry `AdtFlags::IS_HANDLE`. |
| N-K-12 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | A world item of the wrong kind is `INVALID_WORLD_ITEM`, including a whole package in `include`. |
| N-K-13 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | `w.f` for a local world `w` resolves among the `import fn` items of `w` and its includes, first in elaboration order. |
| N-K-14 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | Elaboration keeps source order with includes expanded in place and implicit imports appended. |
| N-K-15 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | An external `export world` is looked up in WASI, then the direct edges, then the only node with the identity. |
| N-K-16 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | `WORLD_EXPORT_UNIMPLEMENTED` is checked per world on its directly written items, on the exports of included external worlds at the include, and on an external export world at the `export world` value. |
| N-K-17 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | The hybrid check compares elaborated worlds structurally. |
| N-K-18 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | A flags type is an ADT with one hidden unsigned field of 1, 2 or 4 bytes, where bit `i` is flag `i`. |
| N-K-19 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | `INVALID_FLAGS_ENUM` is reported once, with the precedence outside-interface, payload, discriminant, count, and suppresses `WIT_EXPLICIT_DISCRIMINANT`. |
| N-K-20 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | Flag constants are written `F.X`. |
| N-K-21 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | The flags operators and provided functions are compiler-provided impls and synthetic inherent items, with the shims `ShimKind::Flags`. |
| N-K-22 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | View derives are compiler-provided impls with nested field obligations and `ShimKind::ViewDerive` bodies. |
| N-K-23 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | A body on an interface function, a resource member or a world function is `INTERFACE_FN_BODY`. |
| N-K-24 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | The interfaces and worlds of a package form one WIT name scope. |
| N-K-25 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | Items with errors are omitted from generated WIT, transitively. |
| N-K-26 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | A second impl item with the name of an earlier one is `INTERFACE_IMPL_EXTRA_ITEM`. |
| N-K-27 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | A world `include` has the `DefPath` component `WorldInclude(Symbol)`. |
| N-K-28 | [impl/16](../impl/16-interfaces-and-wit.md) §16.12 | `EXPORT_FN_NOT_APPLIED` and `EXPORT_WORLD_NOT_APPLIED` are session-level queries that the driver asks for only in the builds where they apply. |
